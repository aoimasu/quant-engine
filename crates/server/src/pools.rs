//! QE-452 Phase B — formula-pool server routes + the durable pool governance lifecycle (design
//! §13.2/§13.3/§13.5/§13.6), mounted under the **session-gated** `/api` subtree.
//!
//! Read routes (session only):
//! - `GET /api/formula-pools` — list pool summaries (both roots, each hash-verified on load).
//! - `GET /api/formula-pools/{id}` — one pool's K canonical S-exprs + deflation summary + review lineage
//!   + its governance lifecycle, served from the **verified** [`FormulaPool::load`].
//! - `GET /api/runs/{id}/archive` — the evolve run's MAP-Elites archive snapshot (`archive.json`).
//!
//! Governance routes (each behind a [`require_role`](crate::auth) seam):
//! - `POST /api/formula-pools/{id}/{approve,reject,revoke,seal}` — the guarded lifecycle transitions.
//! - `POST /api/runs/{id}/halt` — cooperatively halt a running evolve run (reuses the run-cancel machinery).
//!
//! **Production sealing is FAIL-CLOSED until QE-454**: `/seal` refuses a `production`-mode pool with a
//! structured `409` **before any state change**, and a sealed pool **never auto-mints a vintage** (§13.2).
//! Sandbox lifecycle transitions may function; a sandbox seal cannot reach a production vintage.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{http::StatusCode, Extension, Json, Router};
use qe_formula_pool::{
    FormulaPool, FormulaPoolContent, FormulaPoolRepository, PoolError, PoolGovernance,
    PoolGovernanceStore, PoolLifecycleState, PoolMode, PoolTransition, RevocationRecord,
    Revocations, TransitionRecord,
};
use qe_run_protocol::EvolveArchive;
use serde::Serialize;
use serde_json::json;

use crate::audit::{AuditAction, AuditLog, SignoffState};
use crate::auth::{require_approver, require_operator, AuthedEmail, RoleConfig};
use crate::runs::store::atomic_write;
use crate::runs::RunManager;
use crate::AppState;

/// The server-side view of the frozen formula pools: the two artefact roots (research + production,
/// design §13.6 barrier 2) plus the governance store holding each pool's durable lifecycle. All loads go
/// through [`FormulaPool::load`], so an unverified pool is never served.
#[derive(Debug, Clone)]
pub struct PoolState {
    /// The **research** (sandbox) pool root (`<artifacts>/research/pools`).
    research: FormulaPoolRepository,
    /// The **production** pool root (`<artifacts>/pools`).
    production: FormulaPoolRepository,
    /// The governance store (`<data_dir>/governance`) — the durable pool lifecycle, separate from the run.
    governance: PoolGovernanceStore,
}

impl PoolState {
    /// Build pool state from the shared artefacts dir + the server data dir (mirrors the CLI's
    /// `pool_root_for`: sandbox → `<artifacts>/research/pools`, production → `<artifacts>/pools`; the
    /// governance store lives at `<data_dir>/governance`).
    pub fn from_dirs(artifacts_dir: &std::path::Path, data_dir: &std::path::Path) -> Self {
        Self {
            research: FormulaPoolRepository::new(artifacts_dir.join("research").join("pools")),
            production: FormulaPoolRepository::new(artifacts_dir.join("pools")),
            governance: PoolGovernanceStore::new(data_dir.join("governance")),
        }
    }

    /// A **disabled** pool state (all roots under a never-created temp sentinel) — the default in
    /// [`AppState::new`](crate::AppState::new) for tests/paths that don't exercise pools: every read
    /// resolves to empty/absent (list/load are read-only). Real deployments call [`from_dirs`](Self::from_dirs).
    pub fn disabled() -> Self {
        let root = std::env::temp_dir().join("qe-server-pools-disabled");
        Self {
            research: FormulaPoolRepository::new(root.join("research").join("pools")),
            production: FormulaPoolRepository::new(root.join("pools")),
            governance: PoolGovernanceStore::new(root.join("governance")),
        }
    }

    /// Load + **verify** a pool by id, trying the production root then the research root. `None` when the
    /// pool is absent from both (or fails to load/verify — a corrupt pool is never served).
    fn load(&self, id: &str) -> Option<FormulaPool> {
        self.production
            .load(id)
            .ok()
            .or_else(|| self.research.load(id).ok())
    }

    /// The governance record for `id` (a fresh `Draft` when none exists yet).
    fn governance(&self, id: &str) -> Result<PoolGovernance, PoolError> {
        self.governance.read(id)
    }

    /// The `governance/revocations.json` path (a sibling of the per-pool governance records).
    fn revocations_path(&self) -> std::path::PathBuf {
        self.governance.root().join("revocations.json")
    }

    /// Read the forward-only revocation set (missing/empty ⇒ [`Revocations::new`], fail-open on the
    /// *read* — an unreadable file must never make a revoked pool look active, so a parse error surfaces).
    ///
    /// # Errors
    /// [`PoolError`] on a malformed `revocations.json`.
    fn read_revocations(&self) -> Result<Revocations, PoolError> {
        match std::fs::read(self.revocations_path()) {
            Ok(bytes) => Revocations::from_json(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Revocations::new()),
            Err(e) => Err(PoolError::Io(e)),
        }
    }

    /// Insert a revocation + persist `revocations.json` atomically (design §13.9, forward-only — the
    /// audit chain is never rewritten). Returns the updated set.
    ///
    /// # Errors
    /// [`PoolError`] on read/serialise/write failure.
    fn revoke_pool(&self, record: RevocationRecord) -> Result<Revocations, PoolError> {
        let mut revocations = self.read_revocations()?;
        revocations.insert(record);
        let bytes = revocations.to_json()?;
        std::fs::create_dir_all(self.governance.root())?;
        atomic_write(&self.revocations_path(), &bytes).map_err(PoolError::Io)?;
        Ok(revocations)
    }

    /// Every verified pool under both roots, ascending by id, projected to a summary carrying its
    /// governance lifecycle **and** its live-path revocation status (design §13.9 — the read path filters
    /// against `revocations.json`, so a revoked pool is visibly inert even if previously sealed).
    fn summaries(&self) -> Vec<PoolSummary> {
        let mut pools = self.production.list().unwrap_or_default();
        pools.extend(self.research.list().unwrap_or_default());
        pools.sort_by(|a, b| a.content.pool_id.cmp(&b.content.pool_id));
        let revocations = self.read_revocations().unwrap_or_default();
        pools
            .iter()
            .map(|p| {
                let lifecycle = self
                    .governance(&p.content.pool_id)
                    .map(|g| g.state)
                    .unwrap_or_default();
                let revoked = revocations.is_revoked(&p.content.lineage.pool_hash);
                PoolSummary::project(p, lifecycle, revoked)
            })
            .collect()
    }

    /// The detail view for `id` (verified pool content + governance + live-path revocation), or `None`.
    fn detail(&self, id: &str) -> Option<PoolDetail> {
        let pool = self.load(id)?;
        let governance = self
            .governance(id)
            .unwrap_or_else(|_| PoolGovernance::draft(id));
        let revoked = self
            .read_revocations()
            .unwrap_or_default()
            .is_revoked(&pool.content.lineage.pool_hash);
        Some(PoolDetail {
            content_hash: pool.content_hash,
            lifecycle: governance.state,
            revoked,
            history: governance.history,
            content: pool.content,
        })
    }
}

// ---- read routes (session only) -----------------------------------------------------------------

/// The session-gated pool **read** routes + the evolve archive read. Registered inside
/// [`protected_routes`](crate::auth::protected_routes), so they inherit `require_session` (`401` without a
/// session) with no per-handler auth code.
pub fn read_routes() -> Router<AppState> {
    Router::new()
        .route("/formula-pools", get(list_pools))
        .route("/formula-pools/{id}", get(get_pool))
        .route("/runs/{id}/archive", get(get_archive))
}

/// The **governance** routes, each behind a `require_role` seam (design §13.8): the approve/reject/revoke/
/// seal transitions (approver role) and the run halt (operator role). `roles` supplies the seam's
/// allowlists. **QE-454** replaces the seam with authoritative RBAC + audit.
pub fn governance_routes(roles: Arc<RoleConfig>) -> Router<AppState> {
    let approver = Router::new()
        .route("/formula-pools/{id}/approve", post(approve))
        .route("/formula-pools/{id}/reject", post(reject))
        .route("/formula-pools/{id}/revoke", post(revoke))
        .route("/formula-pools/{id}/seal", post(seal))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&roles),
            require_approver,
        ));
    let operator = Router::new()
        .route("/runs/{id}/halt", post(halt))
        .route_layer(axum::middleware::from_fn_with_state(
            roles,
            require_operator,
        ));
    approver.merge(operator)
}

/// A pool list row — the slim summary the QE-453 PoolBrowser renders.
#[derive(Debug, Clone, Serialize)]
pub struct PoolSummary {
    /// The pool id (campaign lineage id).
    pub id: String,
    /// The campaign mode (`sandbox` / `production`).
    pub mode: String,
    /// The content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The content address over the sorted formula hashes (audit/lineage join key).
    pub pool_hash: String,
    /// Number of frozen formulas (`K ≤ 16`).
    pub formula_count: usize,
    /// Whether the trial basis came from the real GP-aware trial-counter path (QE-439).
    pub gp_aware: bool,
    /// Distinct-canonical formulas evaluated (the QE-439 trial basis).
    pub distinct_evaluations: u64,
    /// The pool's durable governance lifecycle state (design §13.3).
    pub lifecycle: PoolLifecycleState,
    /// Whether the pool is revoked on the live path (design §13.9 — `revocations.json` filter). A revoked
    /// pool is inert even if previously sealed; the audit chain is not rewritten.
    pub revoked: bool,
}

impl PoolSummary {
    /// Project a verified pool + its lifecycle + revocation status into a summary row.
    fn project(pool: &FormulaPool, lifecycle: PoolLifecycleState, revoked: bool) -> Self {
        let c = &pool.content;
        Self {
            id: c.pool_id.clone(),
            mode: mode_str(c.mode).to_owned(),
            content_hash: pool.content_hash.clone(),
            pool_hash: c.lineage.pool_hash.clone(),
            formula_count: c.formulas.len(),
            gp_aware: c.deflation.gp_aware,
            distinct_evaluations: c.deflation.distinct_evaluations,
            lifecycle,
            revoked,
        }
    }
}

/// The pool detail view — the verified content (K S-exprs + deflation summary + review lineage) plus the
/// governance lifecycle + transition history the PoolReview gate consumes.
#[derive(Debug, Clone, Serialize)]
pub struct PoolDetail {
    /// The verified, hashed pool content (`format_version`, mode, the K formulas, deflation, lineage).
    pub content: FormulaPoolContent,
    /// The content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The durable governance lifecycle state (a rebuildable cache — the tamper-evident audit log is
    /// authoritative, design §13.3).
    pub lifecycle: PoolLifecycleState,
    /// Whether the pool is revoked on the live path (design §13.9 — `revocations.json` filter).
    pub revoked: bool,
    /// The append-only lifecycle transition history (a cache; the authoritative signed trail is
    /// `GET /api/audit`).
    pub history: Vec<TransitionRecord>,
}

/// `GET /api/formula-pools` — list pool summaries under both roots (each hash-verified on load).
async fn list_pools(State(pools): State<Arc<PoolState>>) -> Response {
    match tokio::task::spawn_blocking(move || pools.summaries()).await {
        Ok(summaries) => Json(summaries).into_response(),
        Err(_) => internal("pool listing task failed".to_owned()),
    }
}

/// `GET /api/formula-pools/{id}` — one verified pool's detail, or `404`.
async fn get_pool(State(pools): State<Arc<PoolState>>, Path(id): Path<String>) -> Response {
    let task_id = id.clone();
    match tokio::task::spawn_blocking(move || pools.detail(&task_id)).await {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => not_found_pool(&id),
        Err(_) => internal("pool detail task failed".to_owned()),
    }
}

/// `GET /api/runs/{id}/archive` — the evolve run's MAP-Elites archive snapshot (`<run-dir>/archive.json`),
/// or `404` when the run is unknown or produced no archive. Reuses the run-store fs access + `404` shape
/// of `GET /api/runs/{id}/result`; the blocking read runs off the async worker (QE-411).
async fn get_archive(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    let store = manager.store().clone();
    let task_id = id.clone();
    let archive_path = store.run_dir(&task_id).join("archive.json");
    match tokio::task::spawn_blocking(move || read_archive(&archive_path)).await {
        Ok(ArchiveOutcome::Body(archive)) => Json(archive).into_response(),
        Ok(ArchiveOutcome::NotFound) => not_found_archive(&id),
        Ok(ArchiveOutcome::Malformed(msg)) => internal(msg),
        Err(_) => internal("archive task failed".to_owned()),
    }
}

/// The blocking outcome of reading `archive.json`.
enum ArchiveOutcome {
    /// The parsed archive to serve.
    Body(EvolveArchive),
    /// No archive at that path (unknown run / no archive produced).
    NotFound,
    /// The archive file exists but could not be parsed.
    Malformed(String),
}

/// Read + parse `<run-dir>/archive.json`.
fn read_archive(path: &std::path::Path) -> ArchiveOutcome {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<EvolveArchive>(&bytes) {
            Ok(archive) => ArchiveOutcome::Body(archive),
            Err(e) => ArchiveOutcome::Malformed(format!("malformed archive.json: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ArchiveOutcome::NotFound,
        Err(e) => ArchiveOutcome::Malformed(format!("failed to read archive.json: {e}")),
    }
}

// ---- governance routes (authoritative RBAC + tamper-evident audit, design §13.8/§13.9) -----------

/// `POST /api/formula-pools/{id}/approve` — record an approver's **dual-sign-off signature** (approver
/// role). Enforces **separation of duties** (`403` if the approver is the pool's launcher), appends a
/// `pool_hash`-bound `approve` audit entry (the authoritative signature), and advances the governance
/// cache `Draft → Approved` on the first sign-off (a second **distinct** approver's signature is recorded
/// in the audit log while the cache stays `Approved`). Production sealing stays fail-closed (see `/seal`).
async fn approve(
    State(pools): State<Arc<PoolState>>,
    State(audit): State<Arc<AuditLog>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    governance_action(pools, audit, id, PoolTransition::Approve, actor).await
}

/// `POST /api/formula-pools/{id}/reject` — `Draft → Rejected` (approver role; single-approver but audited).
async fn reject(
    State(pools): State<Arc<PoolState>>,
    State(audit): State<Arc<AuditLog>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    governance_action(pools, audit, id, PoolTransition::Reject, actor).await
}

/// `POST /api/formula-pools/{id}/revoke` — `Approved`/`Sealed → Revoked` (approver role). Appends an
/// append-only `revoke` audit entry referencing the approval's `entry_hash` **and** adds the pool to
/// `governance/revocations.json` — forward-only deregistration that makes the pool inert on the live path
/// **without rewriting history** (design §13.9).
async fn revoke(
    State(pools): State<Arc<PoolState>>,
    State(audit): State<Arc<AuditLog>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    governance_action(pools, audit, id, PoolTransition::Revoke, actor).await
}

/// `POST /api/formula-pools/{id}/seal` — `Approved → Sealed` (approver role).
///
/// **FAIL-CLOSED**: a `production`-mode pool is refused with a structured `409` **before any state change**
/// (the server-authoritative `seal_allowed` predicate lands in QE-454 Phase B). A sandbox pool may seal,
/// but a sandbox seal **cannot** reach a production vintage — sealing only marks the pool sealed; it
/// **never** auto-mints a vintage (§13.2). Seal is not one of the §13.9 audit actions, so it appends no
/// audit entry here.
async fn seal(
    State(pools): State<Arc<PoolState>>,
    State(audit): State<Arc<AuditLog>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    governance_action(pools, audit, id, PoolTransition::Seal, actor).await
}

/// The verified facts a governance action needs from the loaded pool.
struct PoolFacts {
    pool_id: String,
    pool_hash: String,
    mode: PoolMode,
}

/// The shared governance body (design §13.8/§13.9): load+verify the pool (`404` if absent) → **[seal
/// only]** production fail-closed (`409`) → **[approve only]** separation-of-duties (`403` if the actor is
/// the launcher) → apply the guarded lifecycle-cache transition (`409` on an illegal edge) → append the
/// tamper-evident audit entry (approve/reject/revoke) → **[revoke only]** update `revocations.json` → `200`.
async fn governance_action(
    pools: Arc<PoolState>,
    audit: Arc<AuditLog>,
    id: String,
    transition: PoolTransition,
    actor: String,
) -> Response {
    // 1. Load + verify the pool off the async worker.
    let facts = {
        let pools = Arc::clone(&pools);
        let task_id = id.clone();
        match tokio::task::spawn_blocking(move || {
            pools.load(&task_id).map(|p| PoolFacts {
                pool_id: p.content.pool_id.clone(),
                pool_hash: p.content.lineage.pool_hash.clone(),
                mode: p.content.mode,
            })
        })
        .await
        {
            Ok(Some(f)) => f,
            Ok(None) => return not_found_pool(&id),
            Err(_) => return internal("pool load task failed".to_owned()),
        }
    };

    // 2. FAIL-CLOSED: refuse to seal a production pool — before any state mutation (design §5, §13.6).
    if transition == PoolTransition::Seal && facts.mode == PoolMode::Production {
        return production_seal_gated(&id);
    }

    let ts = now_ms();
    let entries = match audit.read_all() {
        Ok(e) => e,
        Err(e) => return internal(format!("failed to read audit log: {e}")),
    };

    // 3. Separation of duties (approve only): the approver must not be the pool's launcher (design §13.8).
    let launcher = AuditLog::launcher_for_pool(&entries, &facts.pool_id);
    if transition == PoolTransition::Approve {
        if let Some(l) = &launcher {
            if l.eq_ignore_ascii_case(&actor) {
                return separation_of_duties_violation(&id);
            }
        }
    }

    // 4. Apply the guarded lifecycle-cache transition (the cache is rebuildable; the audit log is
    //    authoritative). Approve is idempotent at `Approved` (a second distinct sign-off records only an
    //    audit entry); every other edge goes through the guarded state machine.
    let cache = {
        let pools = Arc::clone(&pools);
        let task_id = id.clone();
        let actor_c = actor.clone();
        match tokio::task::spawn_blocking(move || {
            apply_cache_transition(&pools, &task_id, transition, &actor_c, ts)
        })
        .await
        {
            Ok(r) => r,
            Err(_) => return internal("governance cache task failed".to_owned()),
        }
    };
    let state = match cache {
        Ok(state) => state,
        Err(CacheError::Illegal(msg)) => {
            return (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Err(CacheError::Io(msg)) => return internal(msg),
    };

    // 5. Append the tamper-evident audit entry (approve/reject/revoke; seal is not a §13.9 action).
    let action = match transition {
        PoolTransition::Approve => Some(AuditAction::Approve),
        PoolTransition::Reject => Some(AuditAction::Reject),
        PoolTransition::Revoke => Some(AuditAction::Revoke),
        PoolTransition::Seal => None,
    };
    let mut signoff: Option<SignoffState> = None;
    if let Some(action) = action {
        // For a revoke, bind the audit entry's `evidence_hash` to the approval it deregisters.
        let evidence = if action == AuditAction::Revoke {
            latest_approval_hash(&entries, &facts.pool_hash).unwrap_or_default()
        } else {
            String::new()
        };
        let appended = audit
            .append(&actor, action, &facts.pool_hash, "", "", &evidence, ts)
            .await;
        let entry = match appended {
            Ok(e) => e,
            Err(e) => return internal(format!("failed to append audit entry: {e}")),
        };

        // 6. Revoke also updates the forward-only revocation set (no history rewrite).
        if action == AuditAction::Revoke {
            let pools = Arc::clone(&pools);
            let record = RevocationRecord {
                pool_id: facts.pool_id.clone(),
                pool_hash: facts.pool_hash.clone(),
                revoked_by: actor.clone(),
                ts_ms: ts,
                revoke_entry_hash: entry.entry_hash.clone(),
            };
            let write = tokio::task::spawn_blocking(move || pools.revoke_pool(record)).await;
            match write {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return internal(format!("failed to persist revocation: {e}")),
                Err(_) => return internal("revocation task failed".to_owned()),
            }
        }

        // Re-derive the dual-sign-off state over the fresh chain (approve response surfaces it).
        if action == AuditAction::Approve {
            if let Ok(fresh) = audit.read_all() {
                signoff = Some(AuditLog::derive_signoff(
                    &fresh,
                    &facts.pool_hash,
                    launcher.as_deref(),
                ));
            }
        }
    }

    let mut body = json!({ "pool_id": facts.pool_id, "lifecycle": state });
    if let Some(signoff) = signoff {
        body["signoff"] = json!(signoff);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// A governance-cache transition failure.
enum CacheError {
    /// An illegal lifecycle edge (carries the guarded-transition message).
    Illegal(String),
    /// A persistence/read failure.
    Io(String),
}

/// Apply the guarded lifecycle-cache transition. `Approve` is **idempotent at `Approved`** — a second
/// distinct sign-off leaves the cache `Approved` (the audit log carries the two signatures); the first
/// `Approve` advances `Draft → Approved`. Reject/revoke/seal go through the pure guarded machine.
fn apply_cache_transition(
    pools: &PoolState,
    id: &str,
    transition: PoolTransition,
    actor: &str,
    ts_ms: u64,
) -> Result<PoolLifecycleState, CacheError> {
    let mut governance = pools
        .governance(id)
        .map_err(|e| CacheError::Io(format!("failed to read governance for `{id}`: {e}")))?;

    // A second sign-off on an already-`Approved` pool records only the audit signature (idempotent cache).
    if transition == PoolTransition::Approve && governance.state == PoolLifecycleState::Approved {
        return Ok(PoolLifecycleState::Approved);
    }

    let state = governance
        .apply(transition, actor, ts_ms)
        .map_err(|e| CacheError::Illegal(e.to_string()))?;
    pools
        .governance
        .write(&governance)
        .map_err(|e| CacheError::Io(format!("failed to persist governance for `{id}`: {e}")))?;
    Ok(state)
}

/// The `entry_hash` of the most recent `approve` audit entry bound to `pool_hash` (for a revoke's
/// `evidence_hash` linkage), if any.
fn latest_approval_hash(entries: &[crate::audit::AuditEntry], pool_hash: &str) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|e| e.action == AuditAction::Approve && e.subject_hash == pool_hash)
        .map(|e| e.entry_hash.clone())
}

/// The structured `409` for a fail-closed production seal (unchanged from Phase B — Phase A must not open it).
fn production_seal_gated(id: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "governance not yet enabled — sealing to production is gated on QE-454 Phase B \
                      (seal_allowed / DEFLATION_BASIS_VERSION)",
            "pool_id": id,
            "mode": "production",
        })),
    )
        .into_response()
}

/// The `403` for a separation-of-duties violation (the approver is the pool's launcher, design §13.8).
fn separation_of_duties_violation(id: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "separation of duties — the launcher of a campaign cannot approve its own pool",
            "pool_id": id,
        })),
    )
        .into_response()
}

/// `POST /api/runs/{id}/halt` — cooperatively halt a running evolve run (operator role). Reuses the
/// existing run-cancel machinery ([`RunManager::halt`]): no new kill path.
async fn halt(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    match manager.halt(&id).await {
        crate::runs::HaltOutcome::Halted(status) => (
            StatusCode::OK,
            Json(json!({ "id": id, "status": status, "halted": true })),
        )
            .into_response(),
        crate::runs::HaltOutcome::NotFound => not_found_run(&id),
        crate::runs::HaltOutcome::AlreadyTerminal(status) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "run is already terminal — nothing to halt", "status": status })),
        )
            .into_response(),
    }
}

/// The `sandbox`/`production` wire string for a [`PoolMode`].
fn mode_str(mode: PoolMode) -> &'static str {
    match mode {
        PoolMode::Sandbox => "sandbox",
        PoolMode::Production => "production",
    }
}

/// Milliseconds since the Unix epoch (operational governance timestamp — not a hashed field).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `404` for an unknown pool id.
fn not_found_pool(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("formula pool `{id}` not found") })),
    )
        .into_response()
}

/// `404` for a run with no archive.
fn not_found_archive(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("no archive for run `{id}`") })),
    )
        .into_response()
}

/// `404` for an unknown run id.
fn not_found_run(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("run `{id}` not found") })),
    )
        .into_response()
}

/// A `500` JSON error body with a message.
fn internal(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
        .into_response()
}
