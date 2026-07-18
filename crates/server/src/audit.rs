//! QE-454 Phase A — the **tamper-evident audit log** + the dual-sign-off predicate it feeds (design
//! §13.9 / §13.8).
//!
//! `<data_dir>/audit/log.jsonl` (a sibling of `runs/`) is an append-only JSONL of [`AuditEntry`]s. Each
//! entry is bound into a **hash chain** (`entry_hash = SHA256(canonical_json ‖ prev_hash)`) **and** an
//! **HMAC** under a persistent `QE_AUDIT_SIGNING_KEY`, so any post-hoc mutation of a field — or of the
//! HMAC itself — is detected by [`verify_chain`](AuditLog::verify_chain) at the offending `seq`. Appends
//! are serialised under an `index_lock`-style [`tokio::sync::Mutex`] and persisted with the run store's
//! [`atomic_write`](crate::runs::store::atomic_write) (read-all → push → atomic rewrite).
//!
//! §13.3 makes the append-only signed log the **authoritative** source of pool approvals; the
//! `governance/<pool>.json` record is a rebuildable cache the seal gate never reads. The dual-sign-off
//! clause is therefore re-derived here from `pool_hash`-bound `approve` events
//! ([`derive_signoff`](AuditLog::derive_signoff)), never from the stored `review.json` status — a
//! mismatched `pool_hash` invalidates every prior signature.
//!
//! **Fail-closed:** [`production_seal_capability_allowed`](AuditLog::production_seal_capability_allowed)
//! returns `false` while the signing key is unset/ephemeral (mirrors `check_session_secret_policy`), so
//! production-seal capability can never be enabled under a restart-invalidated key. (Chain + HMAC is
//! tamper-*evidence*, sufficient for v1; external WORM checkpointing of the chain head is a follow-up.)

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{http::StatusCode, Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::runs::store::atomic_write;
use crate::AppState;

/// Default `GET /api/audit` page size.
const DEFAULT_AUDIT_LIMIT: usize = 100;
/// Max `GET /api/audit` page size.
const MAX_AUDIT_LIMIT: usize = 500;

/// The session-gated audit read route (design §13.9): `GET /api/audit`, paginated + chain-verified.
/// Registered inside [`protected_routes`](crate::auth::protected_routes), so it inherits `require_session`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/audit", get(get_audit))
}

/// Query for `GET /api/audit`: `?limit=` (page size) + `?offset=` (starting `seq`, 0-based).
#[derive(Debug, Default, Deserialize)]
struct AuditQuery {
    /// Page size; `None` ⇒ [`DEFAULT_AUDIT_LIMIT`], clamped to [`MAX_AUDIT_LIMIT`].
    limit: Option<usize>,
    /// Starting `seq` (0-based); `None` ⇒ 0.
    offset: Option<u64>,
}

/// `GET /api/audit` — one page of audit entries (ascending by `seq`) plus the **whole-chain**
/// verification status (§13.9), so a reader can see tamper-evidence at a glance. The chain is verified
/// over the entire log (governance volume is small); the returned `entries` are the requested slice.
async fn get_audit(State(audit): State<Arc<AuditLog>>, Query(q): Query<AuditQuery>) -> Response {
    match tokio::task::spawn_blocking(move || {
        let entries = audit.read_all()?;
        let chain = audit.verify_chain(&entries);
        Ok::<_, io::Error>((entries, chain))
    })
    .await
    {
        Ok(Ok((entries, chain))) => {
            let total = entries.len();
            let offset = q.offset.unwrap_or(0) as usize;
            let limit = q
                .limit
                .unwrap_or(DEFAULT_AUDIT_LIMIT)
                .clamp(1, MAX_AUDIT_LIMIT);
            let page: Vec<&AuditEntry> = entries.iter().skip(offset).take(limit).collect();
            let next_offset = if offset + page.len() < total {
                Some((offset + page.len()) as u64)
            } else {
                None
            };
            Json(json!({
                "entries": page,
                "total": total,
                "chain": chain,
                "next_offset": next_offset,
            }))
            .into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to read audit log: {e}") })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "audit read task failed" })),
        )
            .into_response(),
    }
}

type HmacSha256 = Hmac<Sha256>;

/// Env var holding the persistent audit HMAC signing key (§13.9). Unset/blank ⇒ ephemeral (fail-closed).
const ENV_AUDIT_SIGNING_KEY: &str = "QE_AUDIT_SIGNING_KEY";

/// The genesis `prev_hash` for the first entry in the chain (a fixed, well-known sentinel).
const GENESIS_PREV: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A governance action recorded in the audit log (design §13.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// A campaign was launched (the launcher's committed first entry).
    Launch,
    /// An approver signed off a pool (a dual-sign-off signature, bound to `pool_hash`).
    Approve,
    /// An approver rejected a pool.
    Reject,
    /// An approver revoked a pool (forward-only deregistration).
    Revoke,
    /// An admin changed a role assignment.
    RoleChange,
}

/// The content fields of an [`AuditEntry`] that the chain hash + HMAC cover — everything **except** the
/// derived `entry_hash`/`hmac`. Serialised deterministically (fixed field order, no maps) to form the
/// canonical preimage; `prev_hash` is concatenated **after** this canonical JSON per §13.9.
#[derive(Debug, Clone, Serialize)]
struct EntryCore<'a> {
    seq: u64,
    ts_ms: u64,
    actor_email: &'a str,
    action: AuditAction,
    subject_hash: &'a str,
    run_id: &'a str,
    vintage_id: &'a str,
    evidence_hash: &'a str,
}

/// One append-only audit entry (design §13.9). `entry_hash` chains to the previous entry via `prev_hash`;
/// `hmac` authenticates the same preimage under the signing key. Both are lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Monotonic sequence number (0-based, dense).
    pub seq: u64,
    /// Wall-clock epoch-ms the entry was recorded.
    pub ts_ms: u64,
    /// The actor's identity (the `AuthedEmail` on the request).
    pub actor_email: String,
    /// The governance action.
    pub action: AuditAction,
    /// The subject hash: the pool's `pool_hash` for pool-bound actions (approve/reject/revoke), or the
    /// campaign/`pool_id` for a `launch` (the pool is not yet frozen at launch). Empty when N/A.
    pub subject_hash: String,
    /// The originating run id (present on `launch`), else empty.
    pub run_id: String,
    /// The bound vintage id, else empty (Phase A never mints one).
    pub vintage_id: String,
    /// A hash over the evidence the action was gated on (e.g. the revoked approval's `entry_hash`). Empty
    /// when N/A.
    pub evidence_hash: String,
    /// The previous entry's `entry_hash` (or [`GENESIS_PREV`] for `seq == 0`) — the chain link.
    pub prev_hash: String,
    /// `SHA256(canonical_json(core) ‖ prev_hash)`.
    pub entry_hash: String,
    /// `HMAC-SHA256(signing_key, canonical_json(core) ‖ prev_hash)`.
    pub hmac: String,
}

impl AuditEntry {
    /// The canonical preimage: canonical JSON over the content core, then the `prev_hash` bytes (§13.9).
    fn preimage(&self) -> io::Result<Vec<u8>> {
        let core = EntryCore {
            seq: self.seq,
            ts_ms: self.ts_ms,
            actor_email: &self.actor_email,
            action: self.action,
            subject_hash: &self.subject_hash,
            run_id: &self.run_id,
            vintage_id: &self.vintage_id,
            evidence_hash: &self.evidence_hash,
        };
        let mut bytes = serde_json::to_vec(&core).map_err(io::Error::other)?;
        bytes.extend_from_slice(self.prev_hash.as_bytes());
        Ok(bytes)
    }
}

/// The outcome of verifying an audit chain (design §13.9): either the whole chain is intact, or the first
/// `seq` where the hash chain / HMAC / linkage breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ChainStatus {
    /// Every entry's `entry_hash` recomputes, its HMAC verifies, and `prev_hash` links to its predecessor.
    Ok,
    /// The chain is broken at this `seq` (a mutated field, a bad HMAC, or a broken link).
    BrokenAt {
        /// The sequence number of the first entry that fails verification.
        seq: u64,
    },
}

impl ChainStatus {
    /// Whether the chain is fully intact.
    #[must_use]
    pub fn is_ok(self) -> bool {
        matches!(self, ChainStatus::Ok)
    }
}

/// The derived dual-sign-off state of a pool, re-computed from `pool_hash`-bound `approve` events
/// (design §13.8). Never read from the stored `review.json` status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignoffState {
    /// No valid distinct approver signature bound to the current `pool_hash`.
    NoSignoff,
    /// Exactly one distinct approver (≠ launcher) has signed — awaiting the second signoff.
    AwaitingSecondSignoff,
    /// Two or more distinct approvers (each ≠ launcher) have signed — the two-signature clause is
    /// satisfiable (Phase B's `seal_allowed` consumes this).
    TwoDistinctSignoffs,
}

/// Whether the caller may hold **production-seal capability**, given the signing-key posture. Fail-closed:
/// an unset/ephemeral key can never enable it (§13.9, mirrors `check_session_secret_policy`).
#[must_use]
pub fn production_seal_capability_allowed(signing_key_is_ephemeral: bool) -> bool {
    !signing_key_is_ephemeral
}

/// The tamper-evident audit log: the on-disk JSONL path, the append-serialising mutex, and the HMAC
/// signing key (+ whether it is an ephemeral fallback).
#[derive(Debug)]
pub struct AuditLog {
    /// `<data_dir>/audit/log.jsonl`.
    path: PathBuf,
    /// Serialises concurrent appends (an `index_lock`-style mutex).
    append_lock: Mutex<()>,
    /// The HMAC signing key.
    signing_key: Vec<u8>,
    /// Whether [`signing_key`](Self::signing_key) is a random ephemeral fallback (no persistent key set).
    signing_key_is_ephemeral: bool,
}

impl AuditLog {
    /// Build a log at `path` with an explicit key (+ ephemeral flag). Real deployments call
    /// [`from_env`](Self::from_env); tests use this directly.
    pub fn new(
        path: impl Into<PathBuf>,
        signing_key: Vec<u8>,
        signing_key_is_ephemeral: bool,
    ) -> Self {
        Self {
            path: path.into(),
            append_lock: Mutex::new(()),
            signing_key,
            signing_key_is_ephemeral,
        }
    }

    /// Resolve the log at `<data_dir>/audit/log.jsonl`, reading the signing key from
    /// `QE_AUDIT_SIGNING_KEY`. **Fail-closed:** an unset/blank key falls back to a random ephemeral key
    /// (per-process, restart-invalidated) with `signing_key_is_ephemeral = true`, which
    /// [`production_seal_capability_allowed`] refuses — the server still boots, but production-seal
    /// capability stays disabled until a persistent key is set.
    pub fn from_env(data_dir: &Path) -> Self {
        let explicit = std::env::var(ENV_AUDIT_SIGNING_KEY)
            .ok()
            .filter(|s| !s.is_empty());
        let signing_key_is_ephemeral = explicit.is_none();
        let signing_key = explicit.map(String::into_bytes).unwrap_or_else(|| {
            tracing::warn!(
                "{ENV_AUDIT_SIGNING_KEY} unset — using a random ephemeral audit signing key; \
                 production-seal capability stays DISABLED (fail-closed). Set {ENV_AUDIT_SIGNING_KEY} \
                 to a persistent value to enable it."
            );
            let mut key = Uuid::new_v4().as_bytes().to_vec();
            key.extend_from_slice(Uuid::new_v4().as_bytes());
            key
        });
        Self::new(
            data_dir.join("audit").join("log.jsonl"),
            signing_key,
            signing_key_is_ephemeral,
        )
    }

    /// A **disabled** log (an ephemeral key under a **unique** throwaway temp path) — the default in
    /// [`AppState::new`](crate::AppState::new) for tests/paths that don't exercise governance. The path is
    /// per-instance (a fresh UUID) so concurrent tests sharing the default state never interfere. Because
    /// the key is ephemeral, production-seal capability is refused.
    pub fn disabled() -> Self {
        let mut key = Uuid::new_v4().as_bytes().to_vec();
        key.extend_from_slice(Uuid::new_v4().as_bytes());
        Self::new(
            std::env::temp_dir()
                .join(format!("qe-server-audit-disabled-{}", Uuid::new_v4()))
                .join("log.jsonl"),
            key,
            true,
        )
    }

    /// The on-disk log path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the signing key is an ephemeral fallback (⇒ production-seal capability refused).
    #[must_use]
    pub fn signing_key_is_ephemeral(&self) -> bool {
        self.signing_key_is_ephemeral
    }

    /// Whether production-seal capability may be enabled under the current signing-key posture
    /// (fail-closed on an ephemeral key).
    #[must_use]
    pub fn production_seal_capability_allowed(&self) -> bool {
        production_seal_capability_allowed(self.signing_key_is_ephemeral)
    }

    /// Compute `(entry_hash, hmac)` for a fully-populated core + `prev_hash`.
    fn seal_entry(&self, entry: &AuditEntry) -> io::Result<(String, String)> {
        let preimage = entry.preimage()?;
        let entry_hash = hex(&Sha256::digest(&preimage));
        let mut mac = HmacSha256::new_from_slice(&self.signing_key)
            .expect("HMAC accepts a key of any length");
        mac.update(&preimage);
        let hmac = hex(&mac.finalize().into_bytes());
        Ok((entry_hash, hmac))
    }

    /// Append a new entry, serialised under the append lock. Reads the current chain to derive the next
    /// `seq` + `prev_hash`, seals the entry (chain hash + HMAC), and atomically rewrites the JSONL.
    ///
    /// The parameters are exactly the §13.9 audit-entry content fields the caller controls (`seq` +
    /// `prev_hash` + the hashes are derived here), so the argument list mirrors the record shape.
    ///
    /// # Errors
    /// Any filesystem/serialisation error reading or rewriting the log.
    #[allow(clippy::too_many_arguments)] // one arg per §13.9 audit-entry content field
    pub async fn append(
        &self,
        actor_email: &str,
        action: AuditAction,
        subject_hash: &str,
        run_id: &str,
        vintage_id: &str,
        evidence_hash: &str,
        ts_ms: u64,
    ) -> io::Result<AuditEntry> {
        let _guard = self.append_lock.lock().await;
        let mut entries = read_all(&self.path)?;
        let seq = entries.len() as u64;
        let prev_hash = entries
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| GENESIS_PREV.to_owned());
        let mut entry = AuditEntry {
            seq,
            ts_ms,
            actor_email: actor_email.to_owned(),
            action,
            subject_hash: subject_hash.to_owned(),
            run_id: run_id.to_owned(),
            vintage_id: vintage_id.to_owned(),
            evidence_hash: evidence_hash.to_owned(),
            prev_hash,
            entry_hash: String::new(),
            hmac: String::new(),
        };
        let (entry_hash, hmac) = self.seal_entry(&entry)?;
        entry.entry_hash = entry_hash;
        entry.hmac = hmac;
        entries.push(entry.clone());
        write_all(&self.path, &entries)?;
        Ok(entry)
    }

    /// Read the entire chain from disk (empty when the log does not exist yet).
    ///
    /// # Errors
    /// Any filesystem/parse error.
    pub fn read_all(&self) -> io::Result<Vec<AuditEntry>> {
        read_all(&self.path)
    }

    /// Verify a chain: each entry's `entry_hash` must recompute, its HMAC must verify, and its `prev_hash`
    /// must link to its predecessor (`GENESIS_PREV` for `seq 0`). Returns the first broken `seq`, if any.
    #[must_use]
    pub fn verify_chain(&self, entries: &[AuditEntry]) -> ChainStatus {
        let mut expected_prev = GENESIS_PREV.to_owned();
        for (i, entry) in entries.iter().enumerate() {
            // Dense, ordered seq + correct linkage.
            if entry.seq != i as u64 || entry.prev_hash != expected_prev {
                return ChainStatus::BrokenAt { seq: i as u64 };
            }
            // Recompute the chain hash + HMAC over the stored content.
            let Ok((entry_hash, _)) = self.seal_entry(entry) else {
                return ChainStatus::BrokenAt { seq: entry.seq };
            };
            if entry_hash != entry.entry_hash {
                return ChainStatus::BrokenAt { seq: entry.seq };
            }
            // Constant-time HMAC verify over the same preimage.
            let Ok(preimage) = entry.preimage() else {
                return ChainStatus::BrokenAt { seq: entry.seq };
            };
            let Ok(mac_bytes) = decode_hex(&entry.hmac) else {
                return ChainStatus::BrokenAt { seq: entry.seq };
            };
            let mut mac = HmacSha256::new_from_slice(&self.signing_key)
                .expect("HMAC accepts a key of any length");
            mac.update(&preimage);
            if mac.verify_slice(&mac_bytes).is_err() {
                return ChainStatus::BrokenAt { seq: entry.seq };
            }
            expected_prev = entry.entry_hash.clone();
        }
        ChainStatus::Ok
    }

    /// Re-derive the dual-sign-off state for a pool from the chain (design §13.8): count **distinct**
    /// approver emails from `approve` entries whose `subject_hash == pool_hash`, **excluding** `launcher`.
    /// A `pool_hash` that no longer matches any signature (e.g. the pool's formulas changed) yields
    /// [`SignoffState::NoSignoff`] — signatures are invalidated by the hash change.
    #[must_use]
    pub fn derive_signoff(
        entries: &[AuditEntry],
        pool_hash: &str,
        launcher: Option<&str>,
    ) -> SignoffState {
        let launcher = launcher.map(str::to_lowercase);
        let mut approvers: BTreeSet<String> = BTreeSet::new();
        for e in entries {
            if e.action == AuditAction::Approve && e.subject_hash == pool_hash {
                let actor = e.actor_email.to_lowercase();
                if launcher.as_deref() != Some(actor.as_str()) {
                    approvers.insert(actor);
                }
            }
        }
        match approvers.len() {
            0 => SignoffState::NoSignoff,
            1 => SignoffState::AwaitingSecondSignoff,
            _ => SignoffState::TwoDistinctSignoffs,
        }
    }

    /// The launcher of the pool `pool_id`, if the log carries a `launch` entry bound to it
    /// (`subject_hash == pool_id`). The launcher is the campaign's launcher; the frozen pool inherits it
    /// via the stable `pool_id`. `None` for a pool with no recorded launch (e.g. a directly-seeded pool).
    #[must_use]
    pub fn launcher_for_pool(entries: &[AuditEntry], pool_id: &str) -> Option<String> {
        entries
            .iter()
            .find(|e| e.action == AuditAction::Launch && e.subject_hash == pool_id)
            .map(|e| e.actor_email.clone())
    }
}

/// A shared, cheaply-cloneable handle to the audit log.
pub type SharedAuditLog = Arc<AuditLog>;

/// Read the whole JSONL chain (empty when absent). Each non-blank line is one [`AuditEntry`].
fn read_all(path: &Path) -> io::Result<Vec<AuditEntry>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str::<AuditEntry>(line).map_err(io::Error::other)?);
    }
    Ok(entries)
}

/// Atomically rewrite the JSONL chain (read-all → push → rewrite, matching the run store's `index.json`
/// discipline; the whole file is small at governance volume).
fn write_all(path: &Path, entries: &[AuditEntry]) -> io::Result<()> {
    let mut buf = String::new();
    for entry in entries {
        let line = serde_json::to_string(entry).map_err(io::Error::other)?;
        buf.push_str(&line);
        buf.push('\n');
    }
    atomic_write(path, buf.as_bytes())
}

/// Lowercase-hex encode a byte slice.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode a lowercase-hex string into bytes (rejects odd length / non-hex).
fn decode_hex(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).ok_or(())?;
        let lo = (pair[1] as char).to_digit(16).ok_or(())?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(dir: &Path) -> AuditLog {
        AuditLog::new(
            dir.join("audit").join("log.jsonl"),
            b"test-key".to_vec(),
            false,
        )
    }

    async fn approve(l: &AuditLog, actor: &str, pool_hash: &str) -> AuditEntry {
        l.append(actor, AuditAction::Approve, pool_hash, "", "", "", 1)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn chain_appends_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        let e0 = l
            .append("op@x.io", AuditAction::Launch, "pool-1", "run-1", "", "", 1)
            .await
            .unwrap();
        assert_eq!(e0.seq, 0);
        assert_eq!(e0.prev_hash, GENESIS_PREV);
        let e1 = approve(&l, "a@x.io", "hash-1").await;
        assert_eq!(e1.seq, 1);
        assert_eq!(e1.prev_hash, e0.entry_hash);

        let entries = l.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(l.verify_chain(&entries).is_ok());
    }

    #[tokio::test]
    async fn mutating_a_field_breaks_the_chain_at_that_seq() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        l.append("op@x.io", AuditAction::Launch, "pool-1", "run-1", "", "", 1)
            .await
            .unwrap();
        approve(&l, "a@x.io", "hash-1").await;
        approve(&l, "b@x.io", "hash-1").await;

        let mut entries = l.read_all().unwrap();
        // Tamper with entry #1's actor (a rosier rewrite of who approved).
        entries[1].actor_email = "attacker@evil.com".to_owned();
        assert_eq!(l.verify_chain(&entries), ChainStatus::BrokenAt { seq: 1 });
    }

    #[tokio::test]
    async fn a_bad_hmac_breaks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        l.append("op@x.io", AuditAction::Launch, "pool-1", "run-1", "", "", 1)
            .await
            .unwrap();
        let mut entries = l.read_all().unwrap();
        // Corrupt the HMAC only (leave entry_hash intact) — HMAC verification must still fail.
        entries[0].hmac = "0".repeat(64);
        assert_eq!(l.verify_chain(&entries), ChainStatus::BrokenAt { seq: 0 });
    }

    #[tokio::test]
    async fn dual_signoff_requires_two_distinct_approvers_not_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        let pool_hash = "hash-1";
        // Launcher recorded (bound to the pool id, distinct from the pool_hash used for signatures).
        l.append(
            "launcher@x.io",
            AuditAction::Launch,
            "pool-1",
            "run-1",
            "",
            "",
            1,
        )
        .await
        .unwrap();

        let entries = l.read_all().unwrap();
        assert_eq!(
            AuditLog::derive_signoff(&entries, pool_hash, Some("launcher@x.io")),
            SignoffState::NoSignoff
        );

        // One distinct approver → AwaitingSecondSignoff.
        approve(&l, "a@x.io", pool_hash).await;
        let entries = l.read_all().unwrap();
        assert_eq!(
            AuditLog::derive_signoff(&entries, pool_hash, Some("launcher@x.io")),
            SignoffState::AwaitingSecondSignoff
        );

        // The SAME approver signing again → still AwaitingSecondSignoff (distinct count stays 1).
        approve(&l, "a@x.io", pool_hash).await;
        let entries = l.read_all().unwrap();
        assert_eq!(
            AuditLog::derive_signoff(&entries, pool_hash, Some("launcher@x.io")),
            SignoffState::AwaitingSecondSignoff
        );

        // A second DISTINCT approver → the two-signature clause is satisfiable.
        approve(&l, "b@x.io", pool_hash).await;
        let entries = l.read_all().unwrap();
        assert_eq!(
            AuditLog::derive_signoff(&entries, pool_hash, Some("launcher@x.io")),
            SignoffState::TwoDistinctSignoffs
        );
    }

    #[tokio::test]
    async fn launcher_as_approver_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        let pool_hash = "hash-1";
        approve(&l, "launcher@x.io", pool_hash).await; // launcher tries to self-sign
        approve(&l, "a@x.io", pool_hash).await;
        let entries = l.read_all().unwrap();
        // Only one distinct NON-launcher approver counts.
        assert_eq!(
            AuditLog::derive_signoff(&entries, pool_hash, Some("launcher@x.io")),
            SignoffState::AwaitingSecondSignoff
        );
    }

    #[tokio::test]
    async fn pool_hash_mismatch_invalidates_prior_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        approve(&l, "a@x.io", "old-hash").await;
        approve(&l, "b@x.io", "old-hash").await;
        let entries = l.read_all().unwrap();
        // Two signoffs against the OLD hash …
        assert_eq!(
            AuditLog::derive_signoff(&entries, "old-hash", None),
            SignoffState::TwoDistinctSignoffs
        );
        // … but re-derived against a DIFFERENT (current) pool_hash, none count → NoSignoff.
        assert_eq!(
            AuditLog::derive_signoff(&entries, "new-hash", None),
            SignoffState::NoSignoff
        );
    }

    #[tokio::test]
    async fn launcher_for_pool_reads_the_launch_entry() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(dir.path());
        l.append("op@x.io", AuditAction::Launch, "pool-1", "run-1", "", "", 1)
            .await
            .unwrap();
        let entries = l.read_all().unwrap();
        assert_eq!(
            AuditLog::launcher_for_pool(&entries, "pool-1").as_deref(),
            Some("op@x.io")
        );
        assert_eq!(AuditLog::launcher_for_pool(&entries, "pool-2"), None);
    }

    #[test]
    fn ephemeral_key_refuses_production_seal_capability() {
        assert!(!production_seal_capability_allowed(true));
        assert!(production_seal_capability_allowed(false));
        let disabled = AuditLog::disabled();
        assert!(disabled.signing_key_is_ephemeral());
        assert!(!disabled.production_seal_capability_allowed());
    }

    #[test]
    fn decode_hex_round_trips_and_rejects_bad_input() {
        assert_eq!(decode_hex("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(decode_hex("abc").is_err()); // odd length
        assert!(decode_hex("zz").is_err()); // non-hex
    }
}
