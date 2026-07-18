//! The durable **pool governance lifecycle** (QE-452 Phase B, design §13.3).
//!
//! A formula pool has its **own** review lifecycle, entirely distinct from the ephemeral **run**
//! lifecycle that produced it: the `evolve` run terminates normally at `succeeded` when the pool artefact
//! is written, while the *pool* moves through a human-paced, revocable governance state machine that lives
//! **alongside the pool artefact** (persisted), never in the run.
//!
//! This module is the **pure, guarded state machine** ([`PoolLifecycleState::apply`]) plus a small
//! directory-backed persistence layer ([`PoolGovernanceStore`]). It is a pure serde leaf (no `qe-*` dep),
//! so it is firewall-safe exactly like the rest of this crate.
//!
//! **QE-454 boundary.** The append-only [`PoolGovernance::history`] here is a *placeholder* for QE-454's
//! tamper-evident, HMAC-chained audit log — which §13.3 makes the *authoritative* source of pool state (the
//! on-disk record becoming a rebuildable cache). Phase B ships the state machine + a plain persisted record;
//! **real RBAC, dual sign-off, server-authoritative `seal_allowed`, and the `GovernanceRecord`
//! governance↔lineage binding are QE-454.**

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::PoolError;

/// The durable review state of a formula pool (design §13.3). Distinct from `qe_server`'s 4-variant run
/// `RunStatus`: a pool is a separate resource whose state outlives — and is revocable independently of —
/// the run that produced it. Default [`Draft`](PoolLifecycleState::Draft): a freshly-sealed pool artefact
/// with no governance record yet reads as `Draft` (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolLifecycleState {
    /// Illuminated + frozen, awaiting review (≈ §13.3 `PendingReview`).
    #[default]
    Draft,
    /// A reviewer signed off (≈ post-first-signoff; QE-454 adds the dual-signoff `AwaitingSecondSignoff`
    /// sub-state and the "two distinct approvers ≠ launcher" rule).
    Approved,
    /// Sealed — the pool is frozen for consumption. **Phase B seals sandbox pools only**; a
    /// production seal is fail-closed until QE-454, and sealing NEVER auto-mints a vintage (§13.2).
    Sealed,
    /// Rejected in review (terminal).
    Rejected,
    /// Revoked after approval/seal (terminal) — forward-only deregistration (§13.9).
    Revoked,
}

impl PoolLifecycleState {
    /// The wire/log token for this state.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PoolLifecycleState::Draft => "draft",
            PoolLifecycleState::Approved => "approved",
            PoolLifecycleState::Sealed => "sealed",
            PoolLifecycleState::Rejected => "rejected",
            PoolLifecycleState::Revoked => "revoked",
        }
    }

    /// Whether this is a terminal state (no outgoing transition).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            PoolLifecycleState::Rejected | PoolLifecycleState::Revoked
        )
    }

    /// Apply `transition`, returning the next state or an [`LifecycleError::Illegal`] for any edge not on
    /// the guarded graph (design §13.3):
    ///
    /// ```text
    /// Draft ──approve──▶ Approved ──seal──▶ Sealed
    ///   │                   │                  │
    ///   └──reject──▶ Rejected │                │
    ///                        └──revoke──▶ Revoked ◀──revoke──┘
    /// ```
    ///
    /// The ONLY legal edges are `Draft→Approve`, `Draft→Reject`, `Approved→Seal`, `Approved→Revoke`,
    /// `Sealed→Revoke`. Every other `(state, transition)` pair — seal-before-approve, approve-after-revoke,
    /// re-approve, seal-after-reject, revoke-from-draft, … — is rejected.
    ///
    /// # Errors
    /// [`LifecycleError::Illegal`] when `transition` is not a legal edge out of `self`.
    pub fn apply(self, transition: PoolTransition) -> Result<Self, LifecycleError> {
        use PoolLifecycleState::{Approved, Draft, Revoked, Sealed};
        use PoolTransition::{Approve, Reject, Revoke, Seal};
        match (self, transition) {
            (Draft, Approve) => Ok(Approved),
            (Draft, Reject) => Ok(PoolLifecycleState::Rejected),
            (Approved, Seal) => Ok(Sealed),
            (Approved, Revoke) => Ok(Revoked),
            (Sealed, Revoke) => Ok(Revoked),
            (from, transition) => Err(LifecycleError::Illegal { from, transition }),
        }
    }
}

/// A governance action requested against a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolTransition {
    /// Sign off a `Draft` pool → `Approved`.
    Approve,
    /// Reject a `Draft` pool → `Rejected` (terminal).
    Reject,
    /// Seal an `Approved` pool → `Sealed` (sandbox only in Phase B; production fail-closed at the route).
    Seal,
    /// Revoke an `Approved`/`Sealed` pool → `Revoked` (terminal).
    Revoke,
}

impl PoolTransition {
    /// The wire/log token for this transition.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PoolTransition::Approve => "approve",
            PoolTransition::Reject => "reject",
            PoolTransition::Seal => "seal",
            PoolTransition::Revoke => "revoke",
        }
    }
}

/// Errors from a lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LifecycleError {
    /// The requested transition is not a legal edge out of the current state.
    #[error("illegal pool lifecycle transition: cannot `{}` a `{}` pool", transition.as_str(), from.as_str())]
    Illegal {
        /// The state the pool was in.
        from: PoolLifecycleState,
        /// The transition that was attempted.
        transition: PoolTransition,
    },
}

/// One appended governance event (the placeholder for QE-454's tamper-evident audit entry). Records who
/// did what, when, and the `from → to` states — the append-only history from which pool state is derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionRecord {
    /// The transition applied.
    pub transition: PoolTransition,
    /// The actor's identity (the `AuthedEmail` on the request).
    pub actor: String,
    /// Wall-clock epoch-ms the transition was recorded (operational timestamp, not a hashed field).
    pub ts_ms: u64,
    /// The state before the transition.
    pub from: PoolLifecycleState,
    /// The state after the transition.
    pub to: PoolLifecycleState,
}

/// The persisted governance record for one pool: its current [`state`](Self::state) + the append-only
/// [`history`](Self::history). Lives **alongside** the pool artefact under a governance root, NOT in the
/// run's `meta.json`. QE-454 replaces this with the audit-log-derived authoritative state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolGovernance {
    /// The pool this record governs.
    pub pool_id: String,
    /// The current lifecycle state (derived by folding [`history`](Self::history)).
    pub state: PoolLifecycleState,
    /// The append-only transition history (audit placeholder).
    #[serde(default)]
    pub history: Vec<TransitionRecord>,
}

impl PoolGovernance {
    /// A fresh `Draft` governance record for `pool_id` (no history yet).
    #[must_use]
    pub fn draft(pool_id: impl Into<String>) -> Self {
        Self {
            pool_id: pool_id.into(),
            state: PoolLifecycleState::Draft,
            history: Vec::new(),
        }
    }

    /// Apply `transition` by `actor` at `ts_ms`: guard it through [`PoolLifecycleState::apply`], then on
    /// success advance [`state`](Self::state) and append a [`TransitionRecord`]. On an illegal edge the
    /// record is left **unchanged** (no partial mutation).
    ///
    /// # Errors
    /// [`LifecycleError::Illegal`] when `transition` is not legal out of the current state.
    pub fn apply(
        &mut self,
        transition: PoolTransition,
        actor: impl Into<String>,
        ts_ms: u64,
    ) -> Result<PoolLifecycleState, LifecycleError> {
        let from = self.state;
        let to = from.apply(transition)?;
        self.history.push(TransitionRecord {
            transition,
            actor: actor.into(),
            ts_ms,
            from,
            to,
        });
        self.state = to;
        Ok(to)
    }

    /// Serialise the record as JSON to `w`.
    ///
    /// # Errors
    /// [`PoolError::Serialize`] / [`PoolError::Io`] on failure.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), PoolError> {
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|e| PoolError::Serialize(e.to_string()))?;
        w.write_all(&bytes)?;
        Ok(())
    }

    /// Load a record from a JSON reader.
    ///
    /// # Errors
    /// [`PoolError::Deserialize`] / [`PoolError::Io`] on failure.
    pub fn load<R: Read>(r: R) -> Result<Self, PoolError> {
        serde_json::from_reader(r).map_err(|e| PoolError::Deserialize(e.to_string()))
    }
}

/// A directory-backed store of [`PoolGovernance`] records: one `<root>/<pool_id>.json` per pool. Rooted at
/// a governance directory (`<data_dir>/governance`) — a sibling of the run store, separate from the pool
/// artefact roots. A missing record reads as a fresh [`PoolGovernance::draft`] (fail-closed default).
#[derive(Debug, Clone)]
pub struct PoolGovernanceStore {
    root: PathBuf,
}

impl PoolGovernanceStore {
    /// A store rooted at `root` (created on first [`write`](Self::write)).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The governance root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The on-disk path for `pool_id`.
    #[must_use]
    pub fn path_for(&self, pool_id: &str) -> PathBuf {
        self.root.join(format!("{pool_id}.json"))
    }

    /// Read the governance record for `pool_id`, or a fresh `Draft` record when none exists yet.
    ///
    /// # Errors
    /// [`PoolError::Io`] on a filesystem error (other than "not found"), or [`PoolError::Deserialize`].
    pub fn read(&self, pool_id: &str) -> Result<PoolGovernance, PoolError> {
        match std::fs::File::open(self.path_for(pool_id)) {
            Ok(file) => PoolGovernance::load(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(PoolGovernance::draft(pool_id))
            }
            Err(e) => Err(PoolError::Io(e)),
        }
    }

    /// Persist `governance` to `<root>/<pool_id>.json`, creating `root` if needed. Returns the path.
    ///
    /// # Errors
    /// [`PoolError::Io`] / [`PoolError::Serialize`] on failure.
    pub fn write(&self, governance: &PoolGovernance) -> Result<PathBuf, PoolError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_for(&governance.pool_id);
        let mut file = std::fs::File::create(&path)?;
        governance.write(&mut file)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::PoolLifecycleState::{Approved, Draft, Rejected, Revoked, Sealed};
    use super::PoolTransition::{Approve, Reject, Revoke, Seal};
    use super::*;

    #[test]
    fn legal_path_draft_approved_sealed() {
        assert_eq!(Draft.apply(Approve).unwrap(), Approved);
        assert_eq!(Approved.apply(Seal).unwrap(), Sealed);
        // And a full record folds the same way, appending history.
        let mut g = PoolGovernance::draft("pool-x");
        assert_eq!(g.apply(Approve, "a@x.io", 1).unwrap(), Approved);
        assert_eq!(g.apply(Seal, "a@x.io", 2).unwrap(), Sealed);
        assert_eq!(g.state, Sealed);
        assert_eq!(g.history.len(), 2);
        assert_eq!(g.history[0].from, Draft);
        assert_eq!(g.history[1].to, Sealed);
    }

    #[test]
    fn legal_reject_and_revoke_edges() {
        assert_eq!(Draft.apply(Reject).unwrap(), Rejected);
        assert_eq!(Approved.apply(Revoke).unwrap(), Revoked);
        assert_eq!(Sealed.apply(Revoke).unwrap(), Revoked);
    }

    // ---- one test per illegal edge (guarded transitions rejected) --------------------------------

    #[test]
    fn illegal_seal_before_approve() {
        assert_eq!(
            Draft.apply(Seal),
            Err(LifecycleError::Illegal {
                from: Draft,
                transition: Seal
            })
        );
    }

    #[test]
    fn illegal_approve_after_revoke() {
        assert!(matches!(
            Revoked.apply(Approve),
            Err(LifecycleError::Illegal { .. })
        ));
    }

    #[test]
    fn illegal_seal_after_reject() {
        assert!(matches!(
            Rejected.apply(Seal),
            Err(LifecycleError::Illegal { .. })
        ));
    }

    #[test]
    fn illegal_revoke_from_draft() {
        assert!(matches!(
            Draft.apply(Revoke),
            Err(LifecycleError::Illegal { .. })
        ));
    }

    #[test]
    fn illegal_reapprove_after_approve() {
        assert!(matches!(
            Approved.apply(Approve),
            Err(LifecycleError::Illegal { .. })
        ));
    }

    #[test]
    fn illegal_reject_after_seal() {
        assert!(matches!(
            Sealed.apply(Reject),
            Err(LifecycleError::Illegal { .. })
        ));
    }

    #[test]
    fn illegal_edge_leaves_the_record_unchanged() {
        let mut g = PoolGovernance::draft("pool-y");
        assert!(g.apply(Seal, "a@x.io", 1).is_err()); // seal-before-approve
        assert_eq!(g.state, Draft, "state unchanged on an illegal edge");
        assert!(
            g.history.is_empty(),
            "no history appended on an illegal edge"
        );
    }

    #[test]
    fn terminal_states_have_no_outgoing_edge() {
        for terminal in [Rejected, Revoked] {
            for t in [Approve, Reject, Seal] {
                assert!(
                    terminal.apply(t).is_err(),
                    "{terminal:?} +{t:?} must be illegal"
                );
            }
        }
    }

    #[test]
    fn store_round_trips_and_missing_reads_as_draft() {
        let dir = tempfile::tempdir().unwrap();
        let store = PoolGovernanceStore::new(dir.path().join("governance"));
        // Missing ⇒ a fresh Draft record.
        let fresh = store.read("pool-z").unwrap();
        assert_eq!(fresh.state, Draft);
        assert!(fresh.history.is_empty());

        let mut g = store.read("pool-z").unwrap();
        g.apply(Approve, "a@x.io", 10).unwrap();
        store.write(&g).unwrap();

        let loaded = store.read("pool-z").unwrap();
        assert_eq!(loaded.state, Approved);
        assert_eq!(loaded.history.len(), 1);
        assert_eq!(loaded, g);
    }
}
