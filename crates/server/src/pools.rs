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
    PoolGovernanceStore, PoolLifecycleState, PoolMode, PoolTransition, TransitionRecord,
};
use qe_run_protocol::EvolveArchive;
use serde::Serialize;
use serde_json::json;

use crate::auth::{require_approver, require_operator, AuthedEmail, RoleConfig};
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

    /// Every verified pool under both roots, ascending by id, projected to a summary carrying its
    /// governance lifecycle.
    fn summaries(&self) -> Vec<PoolSummary> {
        let mut pools = self.production.list().unwrap_or_default();
        pools.extend(self.research.list().unwrap_or_default());
        pools.sort_by(|a, b| a.content.pool_id.cmp(&b.content.pool_id));
        pools
            .iter()
            .map(|p| {
                let lifecycle = self
                    .governance(&p.content.pool_id)
                    .map(|g| g.state)
                    .unwrap_or_default();
                PoolSummary::project(p, lifecycle)
            })
            .collect()
    }

    /// The detail view for `id` (verified pool content + governance), or `None` if absent.
    fn detail(&self, id: &str) -> Option<PoolDetail> {
        let pool = self.load(id)?;
        let governance = self
            .governance(id)
            .unwrap_or_else(|_| PoolGovernance::draft(id));
        Some(PoolDetail {
            content_hash: pool.content_hash,
            lifecycle: governance.state,
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
}

impl PoolSummary {
    /// Project a verified pool + its lifecycle into a summary row.
    fn project(pool: &FormulaPool, lifecycle: PoolLifecycleState) -> Self {
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
    /// The durable governance lifecycle state.
    pub lifecycle: PoolLifecycleState,
    /// The append-only lifecycle transition history (the QE-454 audit-log placeholder).
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

// ---- governance routes (behind require_role) ----------------------------------------------------

/// `POST /api/formula-pools/{id}/approve` — `Draft → Approved` (approver role).
async fn approve(
    State(pools): State<Arc<PoolState>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    transition(pools, id, PoolTransition::Approve, actor).await
}

/// `POST /api/formula-pools/{id}/reject` — `Draft → Rejected` (approver role).
async fn reject(
    State(pools): State<Arc<PoolState>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    transition(pools, id, PoolTransition::Reject, actor).await
}

/// `POST /api/formula-pools/{id}/revoke` — `Approved`/`Sealed → Revoked` (approver role).
async fn revoke(
    State(pools): State<Arc<PoolState>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    transition(pools, id, PoolTransition::Revoke, actor).await
}

/// `POST /api/formula-pools/{id}/seal` — `Approved → Sealed` (approver role).
///
/// **FAIL-CLOSED**: a `production`-mode pool is refused with a structured `409` **before any state change**
/// (production governance is gated on QE-454). A sandbox pool may seal, but a sandbox seal **cannot** reach
/// a production vintage — sealing only marks the pool sealed; it **never** auto-mints a vintage (§13.2).
async fn seal(
    State(pools): State<Arc<PoolState>>,
    Extension(AuthedEmail(actor)): Extension<AuthedEmail>,
    Path(id): Path<String>,
) -> Response {
    transition(pools, id, PoolTransition::Seal, actor).await
}

/// The shared governance-transition body: load+verify the pool (`404` if absent) → **[seal only]**
/// production fail-closed check (`409`) → apply the guarded lifecycle transition (`409` on an illegal
/// edge) → persist the new state + append the actor's history record → `200`.
async fn transition(
    pools: Arc<PoolState>,
    id: String,
    transition: PoolTransition,
    actor: String,
) -> Response {
    let task_id = id.clone();
    let outcome =
        tokio::task::spawn_blocking(move || apply_transition(&pools, &task_id, transition, &actor))
            .await;
    match outcome {
        Ok(TransitionOutcome::Ok { pool_id, state }) => (
            StatusCode::OK,
            Json(json!({ "pool_id": pool_id, "lifecycle": state })),
        )
            .into_response(),
        Ok(TransitionOutcome::NotFound) => not_found_pool(&id),
        Ok(TransitionOutcome::ProductionSealGated) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "governance not yet enabled — sealing to production is gated on QE-454 \
                          (seal_allowed / DEFLATION_BASIS_VERSION)",
                "pool_id": id,
                "mode": "production",
            })),
        )
            .into_response(),
        Ok(TransitionOutcome::Illegal(msg)) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(TransitionOutcome::Io(msg)) => internal(msg),
        Err(_) => internal("governance task failed".to_owned()),
    }
}

/// The blocking outcome of a governance transition.
enum TransitionOutcome {
    /// The transition applied; the pool's new lifecycle state.
    Ok {
        pool_id: String,
        state: PoolLifecycleState,
    },
    /// The pool id is unknown (absent from both roots).
    NotFound,
    /// A production-mode pool's `/seal` — fail-closed until QE-454.
    ProductionSealGated,
    /// An illegal lifecycle edge (carries the guarded-transition message).
    Illegal(String),
    /// A persistence failure.
    Io(String),
}

/// The blocking core of [`transition`], run off the async worker.
fn apply_transition(
    pools: &PoolState,
    id: &str,
    transition: PoolTransition,
    actor: &str,
) -> TransitionOutcome {
    let Some(pool) = pools.load(id) else {
        return TransitionOutcome::NotFound;
    };
    // FAIL-CLOSED: refuse to seal a production pool — before any state mutation (design §5, §13.6).
    if transition == PoolTransition::Seal && pool.content.mode == PoolMode::Production {
        return TransitionOutcome::ProductionSealGated;
    }
    let mut governance = match pools.governance(id) {
        Ok(g) => g,
        Err(e) => {
            return TransitionOutcome::Io(format!("failed to read governance for `{id}`: {e}"))
        }
    };
    match governance.apply(transition, actor, now_ms()) {
        Ok(state) => match pools.governance.write(&governance) {
            Ok(_) => TransitionOutcome::Ok {
                pool_id: id.to_owned(),
                state,
            },
            Err(e) => {
                TransitionOutcome::Io(format!("failed to persist governance for `{id}`: {e}"))
            }
        },
        Err(e) => TransitionOutcome::Illegal(e.to_string()),
    }
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
