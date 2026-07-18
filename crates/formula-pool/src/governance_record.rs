//! QE-454 Phase A — the **governance↔lineage binding** types (design §13.9), kept in this pure serde
//! leaf so **both** `qe-server` (which writes them) **and** the Phase-B G1/promotion + evolved-catalogue
//! read paths (which filter against them) can share one definition without a `qe-*` runtime edge.
//!
//! Two records, both living **outside** the hashed vintage/pool structs (under `<data_dir>`, never in a
//! hashed artefact):
//!
//! - [`GovernanceRecord`] — a **separate content-addressed** record joining a sealed vintage's
//!   `content_hash` to the pool formulas + the two approvals + the launch entry. It **references** a
//!   vintage's hash but is **never a member of `VintageContent`**: embedding post-hoc approver identity
//!   into the hashed struct would change `vintage_id` and break QE-450 **AC4** byte-identity. Keeping it
//!   separate binds governance→lineage while leaving the reproducible hash untouched.
//! - [`Revocations`] — the `governance/revocations.json` set (keyed by `pool_hash`). Revocation is
//!   **forward-only**: a revoked pool becomes inert on the live/read path (even if previously sealed)
//!   **without rewriting history** — the audit chain keeps its earlier approve/seal entries and an
//!   already-sealed vintage keeps its immutable `formula_hash` pin.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{hex, PoolError};

/// A content-addressed record joining governance (who approved, which launch) to the **reproducible**
/// vintage/pool hash (design §13.9). Lives **outside** `VintageContent` (AC4 byte-identity): it carries the
/// vintage's `content_hash` by value but is a separate artefact, so building/serialising it can never change
/// `vintage_id`/`content_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceRecord {
    /// The sealed vintage's `content_hash` (== its `vintage_id`) — referenced, never embedded upstream.
    pub vintage_content_hash: String,
    /// The pool's sorted `formula_hash` list (the reproducible pool identity payload).
    pub pool_formula_hashes: Vec<String>,
    /// The `entry_hash` of the `launch` audit entry (the launcher's committed first entry).
    pub launch_entry_hash: String,
    /// The `entry_hash`es of the two distinct approver signatures (production dual sign-off). Fewer than
    /// two while a pool is still awaiting its second signoff; Phase B's `seal_allowed` requires exactly two.
    pub approval_entry_hashes: Vec<String>,
    /// The evidence hash over the deflation/tradability stat set the seal was gated on (§13.5).
    pub evidence_hash: String,
}

impl GovernanceRecord {
    /// Lowercase-hex SHA-256 over the record's canonical JSON — the record's **own** content address
    /// (independent of the vintage's `content_hash` it references).
    ///
    /// # Errors
    /// [`PoolError::Serialize`] if the record cannot be serialised.
    pub fn content_hash(&self) -> Result<String, PoolError> {
        let bytes = serde_json::to_vec(self).map_err(|e| PoolError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }
}

/// One forward-only revocation of a pool (design §13.9). Records who revoked it, when, and the audit
/// linkage (the approval entry being revoked + the append-only `revoke` entry) — enough to audit the
/// deregistration **without** rewriting the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationRecord {
    /// The revoked pool's id (campaign lineage id).
    pub pool_id: String,
    /// The revoked pool's `pool_hash` (content address over its sorted `formula_hash` list) — the filter key.
    pub pool_hash: String,
    /// The actor (approver) who revoked it.
    pub revoked_by: String,
    /// Wall-clock epoch-ms of the revocation (operational timestamp, not hashed).
    pub ts_ms: u64,
    /// The `entry_hash` of the `revoke` audit entry that recorded this (append-only, references the approval).
    pub revoke_entry_hash: String,
}

/// The `governance/revocations.json` set — the forward-only deregistration list keyed by `pool_hash`. Both
/// the Phase-B G1/promotion path and the production evolved-catalogue read path filter against
/// [`is_revoked`](Self::is_revoked), so a revoked pool is inert on the live path without any history rewrite.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocations {
    /// Revocations keyed by `pool_hash` (deterministic order; a `BTreeMap` keeps the JSON stable).
    #[serde(default)]
    pub revoked: BTreeMap<String, RevocationRecord>,
}

impl Revocations {
    /// An empty revocation set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the pool with `pool_hash` has been revoked (the live/read-path filter).
    #[must_use]
    pub fn is_revoked(&self, pool_hash: &str) -> bool {
        self.revoked.contains_key(pool_hash)
    }

    /// Retain only the **non-revoked** `pool_hash`es from `pool_hashes` (order-preserving). The drop-in
    /// primitive the QE-454 AC5 live-path filters use: the server read/seal paths already gate on
    /// [`is_revoked`], and the G1/promotion + evolved-catalogue activation paths (which do not yet consume
    /// pools — "runtime never loads a pool") reuse this once they activate an evolved pool, so a revoked
    /// pool becomes inert on the live path **without** rewriting history (design §13.9).
    #[must_use]
    pub fn retain_active<'a>(&self, pool_hashes: &'a [String]) -> Vec<&'a String> {
        pool_hashes.iter().filter(|h| !self.is_revoked(h)).collect()
    }

    /// Record a revocation (idempotent by `pool_hash` — a re-revoke overwrites with the latest record).
    pub fn insert(&mut self, record: RevocationRecord) {
        self.revoked.insert(record.pool_hash.clone(), record);
    }

    /// Serialise as pretty JSON bytes (for `atomic_write` to `revocations.json`).
    ///
    /// # Errors
    /// [`PoolError::Serialize`] on failure.
    pub fn to_json(&self) -> Result<Vec<u8>, PoolError> {
        serde_json::to_vec_pretty(self).map_err(|e| PoolError::Serialize(e.to_string()))
    }

    /// Parse from JSON bytes (a missing/empty file should be treated as [`Revocations::new`] by the caller).
    ///
    /// # Errors
    /// [`PoolError::Deserialize`] on malformed JSON.
    pub fn from_json(bytes: &[u8]) -> Result<Self, PoolError> {
        serde_json::from_slice(bytes).map_err(|e| PoolError::Deserialize(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> GovernanceRecord {
        GovernanceRecord {
            vintage_content_hash: "a".repeat(64),
            pool_formula_hashes: vec!["b".repeat(64), "c".repeat(64)],
            launch_entry_hash: "d".repeat(64),
            approval_entry_hashes: vec!["e".repeat(64), "f".repeat(64)],
            evidence_hash: "0".repeat(64),
        }
    }

    #[test]
    fn governance_record_hash_is_stable_and_independent_of_the_vintage_hash() {
        let r = record();
        let h1 = r.content_hash().unwrap();
        let h2 = r.content_hash().unwrap();
        assert_eq!(h1, h2, "deterministic content address");
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        // The record's own hash is NOT the vintage hash it references (it is a separate content address).
        assert_ne!(h1, r.vintage_content_hash);

        // Changing the referenced vintage hash changes the record hash — but that is the *record's* hash,
        // never the vintage's (the vintage struct is untouched).
        let mut r2 = record();
        r2.vintage_content_hash = "9".repeat(64);
        assert_ne!(r2.content_hash().unwrap(), h1);
    }

    #[test]
    fn revocations_round_trip_and_filter() {
        let mut rev = Revocations::new();
        assert!(!rev.is_revoked(&"b".repeat(64)));
        rev.insert(RevocationRecord {
            pool_id: "pool-x".to_owned(),
            pool_hash: "b".repeat(64),
            revoked_by: "approver@x.io".to_owned(),
            ts_ms: 42,
            revoke_entry_hash: "c".repeat(64),
        });
        assert!(rev.is_revoked(&"b".repeat(64)));
        assert!(!rev.is_revoked(&"z".repeat(64)));

        let bytes = rev.to_json().unwrap();
        let back = Revocations::from_json(&bytes).unwrap();
        assert_eq!(back, rev);
    }

    #[test]
    fn retain_active_drops_revoked_pool_hashes_order_preserving() {
        let mut rev = Revocations::new();
        rev.insert(RevocationRecord {
            pool_id: "p".to_owned(),
            pool_hash: "b".repeat(64),
            revoked_by: "a@x.io".to_owned(),
            ts_ms: 1,
            revoke_entry_hash: "c".repeat(64),
        });
        let hashes = vec!["a".repeat(64), "b".repeat(64), "z".repeat(64)];
        let active = rev.retain_active(&hashes);
        // The revoked "b…" is dropped; the others survive in order.
        assert_eq!(active, vec![&hashes[0], &hashes[2]]);
    }
}
