//! Run lifecycle manager (ADR D4c): validates + creates runs, appends the index, and drives a
//! bounded worker pool of supervised subprocesses.
//!
//! Concurrency: a [`Semaphore`] with `max_concurrency` permits bounds how many subprocesses run at
//! once; runs beyond the cap block on `acquire` and remain observably `queued` until a slot frees. A
//! [`Mutex`] serialises `index.json` read-modify-write. `meta.json` is the authoritative per-run
//! record, written atomically by the supervisor on every transition/progress update.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qe_run_protocol::{
    EvolveMode, ProgressLine, EVOLVE_MAX_DEPTH, EVOLVE_MAX_LOOKBACK, EVOLVE_MAX_NODES,
    EVOLVE_MAX_POOL, EVOLVE_WINDOW_LATTICE, PROTOCOL_VERSION,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;

use super::model::{
    BacktestParams, CreateRunRequest, EnsembleSnapshot, EvolveParams, FlowParams, GateSnapshot,
    GenSnapshot, IndexEntry, Progress, RunMeta, RunSpec, RunStatus, TrainParams, TrainProgress,
};
use super::spawn::JobSpawner;
use super::store::RunStore;

/// How many trailing bytes of subprocess stderr to keep as the failure message.
const STDERR_TAIL_BYTES: usize = 4096;

/// Reason recorded when a run is cooperatively halted by an operator (QE-452 Phase B `POST
/// /api/runs/{id}/halt`). Because `RunStatus` stays 4-state (design §13.12 AC5), a halt is a terminal
/// [`RunStatus::Failed`] carrying this distinguishing reason rather than a new wire variant.
const HALT_REASON: &str = "run halted by operator request";

/// The outcome of [`RunManager::halt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltOutcome {
    /// The run was halted (or was already being torn down); its resulting terminal status.
    Halted(RunStatus),
    /// The run id is unknown.
    NotFound,
    /// The run had already reached a terminal state — nothing to halt; its terminal status.
    AlreadyTerminal(RunStatus),
}

/// A create-run failure.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// The request failed validation (missing/empty required field, unsupported type).
    #[error("invalid run request: {0}")]
    Validation(String),
    /// The server is shutting down and no longer accepts new runs (QE-407 → HTTP 503).
    #[error("server is shutting down")]
    ShuttingDown,
    /// A filesystem error persisting the new run.
    #[error("failed to persist run: {0}")]
    Io(#[from] std::io::Error),
}

/// Reason recorded when the startup reconciler fails an orphaned run (QE-407 / widens QE-263): the run
/// was `running`/`queued` in a `meta.json` left behind by a hard-killed prior process, so no live
/// supervisor exists and it can never make progress.
const RECONCILE_REASON: &str = "run was interrupted by a server restart (no live supervisor)";

/// Reason recorded when a still-live run is aborted because its bounded shutdown-drain window elapsed
/// (QE-407): the graceful drain could not let it finish, so it is terminally failed rather than left
/// `running`.
const SHUTDOWN_DRAIN_REASON: &str = "run did not finish before server shutdown";

/// Reason recorded when a run is terminally failed because it blew past its wall-clock ceiling (QE-454
/// §13.10). A multi-hour campaign that never terminates is killed (`kill_on_drop`) and marked `failed`
/// rather than left `running` forever.
const RUN_DEADLINE_REASON: &str = "run exceeded the wall-clock ceiling";

/// Default per-run wall-clock ceiling (design §13.10 "~24h hard ceiling"): a run that has not terminated by
/// this bound is aborted + terminally failed. Overridable per-manager for tests via
/// [`RunManager::with_run_deadline`].
pub const DEFAULT_MAX_RUN_SECS: u64 = 24 * 60 * 60;

/// Default bound on **concurrently-running evolve campaigns** (design §13.10): a separate semaphore
/// (default 1) so a multi-hour illumination campaign never starves interactive backtests. Overridable via
/// `QE_SERVER_MAX_EVOLVE_CONCURRENCY` / [`RunManager::with_evolve_concurrency`].
pub const DEFAULT_MAX_EVOLVE_CONCURRENCY: usize = 1;

/// QE-461 (design §5.3): default bound on **concurrently-running composite flows** — a separate semaphore
/// (default 1), a byte-for-byte mirror of [`DEFAULT_MAX_EVOLVE_CONCURRENCY`], so a multi-hour train→backtest
/// flow serialises against other flows without ever starving interactive backtests. Overridable via
/// `QE_SERVER_MAX_FLOW_CONCURRENCY` / [`RunManager::with_flow_concurrency`].
pub const DEFAULT_MAX_FLOW_CONCURRENCY: usize = 1;

/// Owns the run store, the spawn seam, and the worker-pool bound. Wrapped in an `Arc` and shared as
/// axum state.
pub struct RunManager {
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    /// QE-454 §13.10: a **separate** bound on concurrently-running evolve campaigns (default 1). An evolve
    /// supervisor acquires one of these **in addition to** a shared worker-pool permit, so a long campaign
    /// serialises against other campaigns without ever blocking interactive backtests.
    evolve_permits: Arc<Semaphore>,
    /// QE-461 §5.3: a **separate** bound on concurrently-running composite flows (default 1), mirroring
    /// [`Self::evolve_permits`]. A flow supervisor acquires one of these **in addition to** a shared
    /// worker-pool permit, so a multi-hour flow serialises against other flows without blocking backtests.
    flow_permits: Arc<Semaphore>,
    /// QE-454 §13.10: the per-run wall-clock ceiling; a run that exceeds it is aborted + terminally failed.
    run_deadline: Duration,
    index_lock: Arc<Mutex<()>>,
    /// QE-407: live supervisor `JoinHandle`s keyed by run id. Each `create` inserts its handle and the
    /// supervisor self-deregisters on completion, so at shutdown this is exactly the set of in-flight
    /// runs to drain/cancel — the registry QE-263 lacked.
    registry: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    /// QE-407: cleared by [`RunManager::shutdown`] to stop accepting new runs (a fresh `create` then
    /// returns [`CreateError::ShuttingDown`]).
    accepting: Arc<AtomicBool>,
}

impl RunManager {
    /// Build a manager over `runs_dir`, spawning at most `max_concurrency` subprocesses concurrently
    /// (clamped to ≥1).
    pub fn new(
        runs_dir: std::path::PathBuf,
        spawner: Arc<dyn JobSpawner>,
        max_concurrency: usize,
    ) -> Self {
        Self {
            store: RunStore::new(runs_dir),
            spawner,
            permits: Arc::new(Semaphore::new(max_concurrency.max(1))),
            evolve_permits: Arc::new(Semaphore::new(DEFAULT_MAX_EVOLVE_CONCURRENCY.max(1))),
            flow_permits: Arc::new(Semaphore::new(DEFAULT_MAX_FLOW_CONCURRENCY.max(1))),
            run_deadline: Duration::from_secs(DEFAULT_MAX_RUN_SECS),
            index_lock: Arc::new(Mutex::new(())),
            registry: Arc::new(Mutex::new(HashMap::new())),
            accepting: Arc::new(AtomicBool::new(true)),
        }
    }

    /// QE-454 §13.10: set the bound on concurrently-running evolve campaigns (clamped to ≥1). A real deploy
    /// resolves this from `QE_SERVER_MAX_EVOLVE_CONCURRENCY`; tests set it directly.
    #[must_use]
    pub fn with_evolve_concurrency(mut self, max_evolve: usize) -> Self {
        self.evolve_permits = Arc::new(Semaphore::new(max_evolve.max(1)));
        self
    }

    /// QE-461 §5.3: set the bound on concurrently-running composite flows (clamped to ≥1), mirroring
    /// [`Self::with_evolve_concurrency`]. A real deploy resolves this from `QE_SERVER_MAX_FLOW_CONCURRENCY`;
    /// tests set it directly.
    #[must_use]
    pub fn with_flow_concurrency(mut self, max_flow: usize) -> Self {
        self.flow_permits = Arc::new(Semaphore::new(max_flow.max(1)));
        self
    }

    /// QE-454 §13.10: set the per-run wall-clock ceiling (tests use a tiny value to exercise the abort).
    #[must_use]
    pub fn with_run_deadline(mut self, deadline: Duration) -> Self {
        self.run_deadline = deadline;
        self
    }

    /// The underlying store (for read handlers).
    pub fn store(&self) -> &RunStore {
        &self.store
    }

    /// Validate + create a run: write `meta.json` (`queued`), append `index.json`, and spawn a
    /// supervisor task registered in the in-flight [`Self::registry`]. Returns the new run id.
    ///
    /// # Errors
    /// [`CreateError::Validation`] on a bad request; [`CreateError::ShuttingDown`] if the manager has
    /// begun shutting down; [`CreateError::Io`] on a persistence failure.
    pub async fn create(&self, req: CreateRunRequest) -> Result<String, CreateError> {
        // QE-407: refuse new work once shutdown has begun so the drain set can't grow under our feet.
        if !self.accepting.load(Ordering::SeqCst) {
            return Err(CreateError::ShuttingDown);
        }
        let spec = build_spec(&req)?;
        let id = uuid::Uuid::new_v4().to_string();
        let created_ms = now_ms();
        let run_type = spec.run_type().to_owned();
        let meta = RunMeta {
            id: id.clone(),
            run_type: run_type.clone(),
            status: RunStatus::Queued,
            params: spec.params_value(),
            progress: Progress::default(),
            train: None,
            flow: None,
            created_ms,
            started_ms: None,
            finished_ms: None,
            exit: None,
            error: None,
            artifacts: Vec::new(),
        };
        // QE-411: `init_run` is blocking `std::fs` (create dir, touch `stdout.log`, write `meta.json`) —
        // run it off the async executor before taking the index lock, awaited to completion so the
        // init-then-index-append ordering is unchanged.
        {
            let store = self.store.clone();
            let meta = meta.clone();
            tokio::task::spawn_blocking(move || store.init_run(&meta))
                .await
                .map_err(std::io::Error::other)??;
        }

        // Append to the discovery index under the lock (serialises concurrent creates). QE-411: the
        // blocking index read-modify-write runs inside `spawn_blocking` so it never parks the async
        // executor thread; the async `index_lock` is still held across the await, preserving the
        // serialisation of concurrent creates.
        {
            let _guard = self.index_lock.lock().await;
            let store = self.store.clone();
            let entry = IndexEntry {
                id: id.clone(),
                run_type,
                created_ms,
                label: spec.label(),
            };
            tokio::task::spawn_blocking(move || {
                let mut index = store.read_index()?;
                index.push(entry);
                store.write_index(&index)
            })
            .await
            .map_err(std::io::Error::other)??;
        }

        // Supervisor task: acquires a pool permit (blocking here keeps the run `queued`), then runs +
        // tails the subprocess. Registered in `registry` so shutdown can drain/cancel it (QE-407).
        let store = self.store.clone();
        let spawner = Arc::clone(&self.spawner);
        let permits = Arc::clone(&self.permits);
        let evolve_permits = Arc::clone(&self.evolve_permits);
        let flow_permits = Arc::clone(&self.flow_permits);
        let run_deadline = self.run_deadline;
        let registry = Arc::clone(&self.registry);
        let task_id = id.clone();
        // Hold the registry lock across `spawn` + `insert` so the task's self-deregister (which also
        // takes this lock, only at the very end of `supervise`) can never run before the insert — even
        // for an instantly-finishing job. This guarantees insert-before-remove: no leaked live entry,
        // no double-remove.
        let mut reg = self.registry.lock().await;
        let handle = tokio::spawn(async move {
            supervise(
                store,
                spawner,
                permits,
                evolve_permits,
                flow_permits,
                run_deadline,
                meta,
                spec,
            )
            .await;
            registry.lock().await.remove(&task_id);
        });
        reg.insert(id.clone(), handle);
        drop(reg);

        Ok(id)
    }

    /// QE-407 — the startup reconciler (widens QE-263). Any run whose `meta.json` is still
    /// `Queued`/`Running` in a freshly-booted process was orphaned by a hard kill (no supervisor can be
    /// alive), so mark it `failed`. Terminal runs (`Succeeded`/`Failed`) are left untouched. Returns
    /// how many runs were reconciled.
    ///
    /// # Errors
    /// A filesystem/parse error reading `index.json` or a run's `meta.json`.
    pub fn reconcile_orphans(&self) -> std::io::Result<usize> {
        let index = self.store.read_index()?;
        let mut reconciled = 0;
        for entry in &index {
            if let Some(mut meta) = self.store.read_meta(&entry.id)? {
                if matches!(meta.status, RunStatus::Queued | RunStatus::Running) {
                    // QE-461 §5.3: a RESUMABLE flow (vintage sealed ∧ backtest incomplete) is NOT dead — it
                    // resumes from its sealed-vintage checkpoint, so leave it for [`Self::resume_orphaned_flows`]
                    // to re-spawn the backtest phase. Every other non-terminal orphan is a dead run: fail it.
                    if matches!(
                        classify_orphan(&self.store, &meta),
                        OrphanAction::ResumeBacktest
                    ) {
                        continue;
                    }
                    let exit = meta.exit;
                    finish_failed(&self.store, &mut meta, exit, RECONCILE_REASON.to_owned());
                    reconciled += 1;
                }
            }
        }
        Ok(reconciled)
    }

    /// QE-461 §5.3 — the flow-resume half of startup reconciliation. Scans the index and, for each orphaned
    /// **resumable** flow ([`classify_orphan`] ⇒ [`OrphanAction::ResumeBacktest`]: vintage sealed ∧ backtest
    /// incomplete), re-spawns a registry-tracked supervisor that runs **only** the backtest phase from the
    /// sealed-vintage checkpoint — never the expensive search. Call after [`Self::reconcile_orphans`] (which
    /// has already failed the dead runs) and before serving. Returns how many flows were resumed.
    ///
    /// # Errors
    /// A filesystem/parse error reading `index.json` or a run's `meta.json`.
    pub async fn resume_orphaned_flows(&self) -> std::io::Result<usize> {
        let index = self.store.read_index()?;
        let mut resumed = 0;
        for entry in &index {
            if let Some(meta) = self.store.read_meta(&entry.id)? {
                if matches!(meta.status, RunStatus::Queued | RunStatus::Running)
                    && matches!(
                        classify_orphan(&self.store, &meta),
                        OrphanAction::ResumeBacktest
                    )
                {
                    self.spawn_flow_backtest_resume(meta).await;
                    resumed += 1;
                }
            }
        }
        Ok(resumed)
    }

    /// Spawn a registry-tracked supervisor that resumes a flow's backtest phase (mirrors [`Self::create`]'s
    /// task-spawn + insert-before-remove registry discipline, so shutdown/halt can drain it). The caller
    /// guarantees `meta` is a resumable flow with a recorded `train.vintage`.
    async fn spawn_flow_backtest_resume(&self, meta: RunMeta) {
        // `classify_orphan` guarantees a recorded `train.vintage`; be defensive rather than panic.
        let Some(vintage) = meta.train.as_ref().and_then(|t| t.vintage.clone()) else {
            return;
        };
        let store = self.store.clone();
        let spawner = Arc::clone(&self.spawner);
        let permits = Arc::clone(&self.permits);
        let flow_permits = Arc::clone(&self.flow_permits);
        let run_deadline = self.run_deadline;
        let registry = Arc::clone(&self.registry);
        let id = meta.id.clone();
        let task_id = meta.id.clone();
        // Hold the registry lock across spawn + insert so the self-deregister can never precede the insert.
        let mut reg = self.registry.lock().await;
        let handle = tokio::spawn(async move {
            supervise_flow_resume(
                store,
                spawner,
                permits,
                flow_permits,
                run_deadline,
                meta,
                vintage,
            )
            .await;
            registry.lock().await.remove(&task_id);
        });
        reg.insert(id, handle);
        drop(reg);
    }

    /// QE-407 — graceful shutdown. Stop accepting new runs, then drain in-flight supervisors within a
    /// bounded window: handles that finish naturally write their own terminal `meta.json`; any handle
    /// still live at the deadline is aborted (dropping its `Child` fires `kill_on_drop`) and its run is
    /// terminally marked `failed`, so no `running` `meta.json` survives a clean shutdown.
    pub async fn shutdown(&self, drain: Duration) {
        self.accepting.store(false, Ordering::SeqCst);
        // Wake any *queued* supervisor blocked on `acquire()`: the existing `Err` arm fails it cleanly
        // ("worker pool closed") so it drains promptly rather than being force-aborted below.
        self.permits.close();

        let handles: Vec<(String, JoinHandle<()>)> = {
            let mut reg = self.registry.lock().await;
            reg.drain().collect()
        };

        let deadline = Instant::now() + drain;
        for (id, mut handle) in handles {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let drained =
                !remaining.is_zero() && tokio::time::timeout(remaining, &mut handle).await.is_ok();
            if !drained {
                handle.abort();
                // Await the cancellation to fully settle (Child dropped → killed) before writing the
                // terminal record, so the aborted task can't interleave a later `meta.json` write.
                let _ = handle.await;
                self.terminally_mark_interrupted(&id, SHUTDOWN_DRAIN_REASON);
            }
        }
    }

    /// QE-452 Phase B — cooperatively **halt** run `id` (`POST /api/runs/{id}/halt`). Reuses the QE-407
    /// shutdown-drain machinery verbatim (no new kill path): pull the supervisor `JoinHandle` out of the
    /// in-flight [`Self::registry`], `abort()` it (dropping its `Child` fires the existing
    /// `kill_on_drop(true)`), `await` the cancellation to settle, then terminally mark the run.
    ///
    /// Because `RunStatus` stays 4-state (design §13.12 AC5 — no new wire variant), a halted run is
    /// recorded as terminal [`RunStatus::Failed`] with the [`HALT_REASON`] error, which distinguishes an
    /// operator halt from an ordinary failure. A run with no live supervisor is either unknown
    /// ([`HaltOutcome::NotFound`]) or already terminal ([`HaltOutcome::AlreadyTerminal`], never re-halted).
    pub async fn halt(&self, id: &str) -> HaltOutcome {
        // Take the live supervisor handle (mirrors `shutdown`'s drain). Present ⇒ the run is in-flight.
        let handle = { self.registry.lock().await.remove(id) };
        if let Some(handle) = handle {
            handle.abort();
            // Await the cancellation to fully settle (Child dropped → killed) before writing the terminal
            // record, so the aborted task can't interleave a later `meta.json` write.
            let _ = handle.await;
            return self.mark_halted(id);
        }
        // No live supervisor: the run is unknown, or already terminal (can't be halted).
        match self.store.read_meta(id) {
            Ok(Some(meta)) => HaltOutcome::AlreadyTerminal(meta.status),
            Ok(None) => HaltOutcome::NotFound,
            // A meta read error is surfaced as not-found for the halt path (best-effort operator action).
            Err(_) => HaltOutcome::NotFound,
        }
    }

    /// Terminally mark run `id` as halted (`Failed` + [`HALT_REASON`]) if it is still non-terminal, then
    /// report the resulting status. A run that finished during the abort keeps its real terminal outcome.
    fn mark_halted(&self, id: &str) -> HaltOutcome {
        match self.store.read_meta(id) {
            Ok(Some(mut meta)) => {
                if matches!(meta.status, RunStatus::Queued | RunStatus::Running) {
                    let exit = meta.exit;
                    finish_failed(&self.store, &mut meta, exit, HALT_REASON.to_owned());
                }
                HaltOutcome::Halted(meta.status)
            }
            _ => HaltOutcome::NotFound,
        }
    }

    /// Mark a run `failed` with `reason`, but only if it is still non-terminal (`Queued`/`Running`) —
    /// a run that finished during the drain keeps its real terminal outcome.
    fn terminally_mark_interrupted(&self, id: &str, reason: &str) {
        if let Ok(Some(mut meta)) = self.store.read_meta(id) {
            if matches!(meta.status, RunStatus::Queued | RunStatus::Running) {
                let exit = meta.exit;
                finish_failed(&self.store, &mut meta, exit, reason.to_owned());
            }
        }
    }
}

/// Build the typed [`RunSpec`] from a create-run request: dispatch on `type`, deserialize the opaque
/// `params` into the run type's typed struct (lenient — every field defaults), then validate required
/// fields. Every failure is a uniform [`CreateError::Validation`] (→ `400`), never a serde `422`.
fn build_spec(req: &CreateRunRequest) -> Result<RunSpec, CreateError> {
    // A missing / `null` params object still parses into the all-default struct so required-ness is
    // enforced uniformly below (an empty body 400s on the first missing field, not a serde reject).
    let params = if req.params.is_null() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        req.params.clone()
    };
    match req.run_type.as_str() {
        "backtest" => {
            let p: BacktestParams = serde_json::from_value(params)
                .map_err(|e| CreateError::Validation(format!("invalid backtest params: {e}")))?;
            validate_backtest(&p)?;
            Ok(RunSpec::Backtest(p))
        }
        "train" => {
            let p: TrainParams = serde_json::from_value(params)
                .map_err(|e| CreateError::Validation(format!("invalid train params: {e}")))?;
            validate_train(&p)?;
            Ok(RunSpec::Train(p))
        }
        "evolve" => {
            // `seed` is REQUIRED (no serde default), so a body missing it fails here with a clear
            // message — an evolve approval must stay byte-reproducible off the recorded seed. A bad
            // `mode` string likewise fails serde here (uniform `400`, never a `422`).
            let p: EvolveParams = serde_json::from_value(params)
                .map_err(|e| CreateError::Validation(format!("invalid evolve params: {e}")))?;
            validate_evolve(&p)?;
            Ok(RunSpec::Evolve(p))
        }
        "flow" => {
            // QE-460: `seed` is REQUIRED (no serde default, mirroring `evolve`), so a body missing it fails
            // here with a clear message — a flow verdict must stay byte-reproducible off the recorded seed.
            let p: FlowParams = serde_json::from_value(params)
                .map_err(|e| CreateError::Validation(format!("invalid flow params: {e}")))?;
            validate_flow(&p)?;
            Ok(RunSpec::Flow(p))
        }
        other => Err(CreateError::Validation(format!(
            "unsupported run type `{other}` (expected `backtest`, `train`, `evolve`, or `flow`)"
        ))),
    }
}

/// Enforce a non-empty required string field.
fn require(name: &str, value: &str) -> Result<(), CreateError> {
    if value.trim().is_empty() {
        return Err(CreateError::Validation(format!("`{name}` is required")));
    }
    Ok(())
}

/// Validate backtest params (QE-255 semantics, unchanged).
fn validate_backtest(p: &BacktestParams) -> Result<(), CreateError> {
    require("vintage", &p.vintage)?;
    require("start", &p.start)?;
    require("end", &p.end)?;
    require("resolution", &p.resolution)?;
    if p.universe.is_empty() || p.universe.iter().all(|s| s.trim().is_empty()) {
        return Err(CreateError::Validation(
            "`universe` must contain at least one instrument".to_owned(),
        ));
    }
    Ok(())
}

/// Validate train params (QE-261 + QE-458): the training window is required; the budget/config are optional
/// (the `qe train` CLI supplies its own defaults) and the universe is config-derived.
///
/// QE-458 (design §6.2): the whitelisted steer knobs (budget, indicator subset, windows/folds) are accepted
/// leniently, but the **blocklist** — everything the G1 gate's decision rides on — carries a compiled floor
/// that is rejected (`400`) when a request tries to set it *below* the floor, and the **regime-coverage
/// invariant** rejects a window/fold count that would shrink OOS/regime coverage below its floor. The G1
/// thresholds themselves (`G1Criteria`, `DEFLATION_BASIS_VERSION`) stay server-side and are never edited.
fn validate_train(p: &TrainParams) -> Result<(), CreateError> {
    require("start", &p.start)?;
    require("end", &p.end)?;
    require("resolution", &p.resolution)?;

    // (§6.2) Blocklist. Everything the G1 gate's own DECISION rides on is off the whitelist and is not
    // steerable in ANY direction — a request that so much as *names* one is a `400`. Rejecting outright (not
    // merely "below a floor") is the fail-safe reading: it closes the hole where a cap/ceiling-style knob
    // (`max_turnover_frac`, `pbo_cutoff`, `ic_fdr_threshold`) could be *raised* to RELAX the gate — exactly
    // the overfitting this ticket exists to kill. The compiled floors (`qe_validation::*_FLOOR`) name the
    // safe value each is pinned at server-side; no request field edits them.
    reject_if_present(
        "cost_stress_multiplier",
        p.cost_stress_multiplier.map(|_| ()),
    )?;
    reject_if_present("max_turnover_frac", p.max_turnover_frac.map(|_| ()))?;
    reject_if_present("capacity_floor_usd", p.capacity_floor_usd.map(|_| ()))?;
    reject_if_present("dsr_cutoff", p.dsr_cutoff.map(|_| ()))?;
    reject_if_present("pbo_cutoff", p.pbo_cutoff.map(|_| ()))?;
    reject_if_present("ic_fdr_threshold", p.ic_fdr_threshold.map(|_| ()))?;

    // Holdout / embargo / purge PRE-EXIST as legitimate knobs (QE-261) — the frozen holdout is *floored, not
    // tuned* (design §4): they may be RAISED (a safe tighten) but never dropped below their compiled floor.
    floor_usize("holdout", p.holdout, qe_validation::HOLDOUT_FLOOR)?;
    floor_usize("embargo", p.embargo, qe_validation::EMBARGO_FLOOR)?;
    floor_usize("purge", p.purge, qe_validation::PURGE_FLOOR)?;

    // (§6.1a d) Regime-coverage invariant: the window/fold knobs cannot shrink OOS/regime coverage below the
    // floor. Fewer windows ⇒ a smaller total OOS span ⇒ weaker regime coverage, so a below-floor count is a
    // `400` — regime coverage is invariant to steering.
    floor_usize("windows", p.windows, qe_validation::MIN_WFO_WINDOWS)?;
    floor_usize("folds", p.folds, qe_validation::MIN_WFO_FOLDS)?;

    // QE-458: evolved-pool-as-indicator steering is not yet applied by the live train search — including a
    // sealed GP formula as a strategy indicator requires a QE-402-safe feature-space extension (a follow-up).
    // Reject it rather than accept-and-silently-ignore, so a steered request naming an evolved pool errors
    // instead of running an un-steered full-catalogue search. (Indicator-subset / window / fold / budget
    // steering IS applied live by `run_train_job`.)
    if p.evolved_pool.is_some() || p.evolved_formulas.is_some() {
        return Err(CreateError::Validation(
            "evolved-pool-as-indicator steering (`evolved_pool` / `evolved_formulas`) is not yet \
             supported on the live train search — use `indicator_subset` / `windows` / `folds` / budget \
             steering (a QE-402-safe feature-space extension is a follow-up)"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Validate composite-flow params (QE-460, design §5.2/§4): the flow window + a serde-required `seed`, then
/// the **exact same** QE-458 steer-whitelist / blocklist / holdout-embargo-floor enforcement `validate_train`
/// applies — **reused** via [`FlowParams::to_train_params`], never duplicated or diverged (so a flow can
/// never steer past a gate floor a `train` could not). A uniform `400` on any violation.
fn validate_flow(p: &FlowParams) -> Result<(), CreateError> {
    // The train sub-run derived from the flow carries the required window + the steer block; validating it
    // reuses the whitelist/blocklist + the holdout/embargo/purge/windows/folds floors (design §4/§6.2) with
    // zero divergence. `seed` is already enforced-present by serde in `build_spec` (a missing seed is a
    // `400`), so a flow is byte-reproducible off its recorded seed.
    validate_train(&p.to_train_params())
}

/// Reject a non-steerable (blocklist) gate-decision knob that appears in the request at all (a `400`) —
/// these carry compiled floors and are never client-editable in any direction (design §6.2).
fn reject_if_present(name: &str, value: Option<()>) -> Result<(), CreateError> {
    if value.is_some() {
        return Err(CreateError::Validation(format!(
            "`{name}` is not steerable — it rides the G1 gate decision and stays pinned at its compiled \
             floor server-side; a request cannot set it"
        )));
    }
    Ok(())
}

/// Reject a `usize` floored knob (holdout/embargo/purge/windows/folds) set below its compiled floor (`400`).
fn floor_usize(name: &str, value: Option<usize>, floor: usize) -> Result<(), CreateError> {
    if let Some(v) = value {
        if v < floor {
            return Err(CreateError::Validation(format!(
                "`{name}` cannot be set below its compiled floor {floor} (got {v})"
            )));
        }
    }
    Ok(())
}

/// Validate evolve params (QE-452 §13.2/§13.4): the window is required (needed to scan bars), and every
/// declared cap must lie within the compiled guardrails — `depth ≤ 4`, `nodes ≤ 16`, `lookback ≤ 200`,
/// windows on the `{5,10,20,50,100}` lattice, and the frozen-pool size `K ≤ 16`. `seed`-present and a
/// valid `mode` are already enforced by serde in `build_spec` (a missing seed / unknown mode is a
/// `400`). An out-of-cap request is rejected with a clear message so a crafted client cannot launch a
/// leakage-inviting campaign.
fn validate_evolve(p: &EvolveParams) -> Result<(), CreateError> {
    require("start", &p.start)?;
    require("end", &p.end)?;
    require("resolution", &p.resolution)?;
    // QE-454 §13.6 barrier 1: a `production` campaign cannot even be LAUNCHED unless the compiled
    // `DEFLATION_BASIS_VERSION` carries every prerequisite bit — a tampered client is blocked at the door
    // (400), long before any seal. The const is server-side + non-editable; no request field can flip it.
    validate_evolve_basis(p.mode, qe_validation::DEFLATION_BASIS_VERSION)?;
    cap("depth", p.depth, EVOLVE_MAX_DEPTH)?;
    cap("nodes", p.nodes, EVOLVE_MAX_NODES)?;
    cap("lookback", p.lookback, EVOLVE_MAX_LOOKBACK)?;
    cap("k", p.k, EVOLVE_MAX_POOL)?;
    if let Some(windows) = &p.windows {
        for &w in windows {
            if !EVOLVE_WINDOW_LATTICE.contains(&w) {
                return Err(CreateError::Validation(format!(
                    "`windows` entry `{w}` is off the lattice (allowed: {EVOLVE_WINDOW_LATTICE:?})"
                )));
            }
        }
    }
    Ok(())
}

/// QE-454 §13.6 barrier 1 — the pure production-launch prerequisite gate (testable at any `basis_version`,
/// since the compiled [`qe_validation::DEFLATION_BASIS_VERSION`] is currently satisfied). A `production`
/// campaign is refused (`400`) unless `basis_version` carries every [`qe_validation::REQUIRED_DEFLATION_BASIS`]
/// bit; `sandbox` is never gated on the basis.
fn validate_evolve_basis(mode: EvolveMode, basis_version: u32) -> Result<(), CreateError> {
    if mode == EvolveMode::Production && !qe_validation::basis_satisfied(basis_version) {
        let missing = qe_validation::missing_basis_prereqs(basis_version);
        return Err(CreateError::Validation(format!(
            "production evolve campaigns are gated on the compiled deflation-basis prerequisites \
             (DEFLATION_BASIS_VERSION); missing: {missing:?}"
        )));
    }
    Ok(())
}

/// Reject an optional numeric cap that exceeds `max` (an omitted cap defers to the engine default).
fn cap(name: &str, value: Option<usize>, max: usize) -> Result<(), CreateError> {
    if let Some(v) = value {
        if v > max {
            return Err(CreateError::Validation(format!(
                "`{name}` must be ≤ {max} (got {v})"
            )));
        }
    }
    Ok(())
}

/// Milliseconds since the Unix epoch (operational timestamp for `meta.json`).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Supervise one run end-to-end: acquire a pool slot, spawn the subprocess, tail stdout progress
/// into `meta.json` + `stdout.log`, capture a stderr tail, and record the terminal outcome.
#[allow(clippy::too_many_arguments)] // the supervisor's store/spawner/three permits/deadline/meta/spec are all independent inputs
async fn supervise(
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    evolve_permits: Arc<Semaphore>,
    flow_permits: Arc<Semaphore>,
    run_deadline: Duration,
    mut meta: RunMeta,
    spec: RunSpec,
) {
    // Block here until a worker-pool slot is free — the run stays `queued` meanwhile. The permit is
    // released when `_permit` drops at the end of this task.
    let _permit = match permits.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            // Semaphore closed (shutdown) — mark failed and bail.
            finish_failed(&store, &mut meta, None, "worker pool closed".to_owned());
            return;
        }
    };

    // QE-454 §13.10: an evolve campaign ALSO acquires a separate evolve permit (default 1) so a multi-hour
    // campaign serialises against other campaigns without starving interactive backtests. Held for the run.
    let _evolve_permit = if matches!(spec, RunSpec::Evolve(_)) {
        match evolve_permits.acquire().await {
            Ok(p) => Some(p),
            Err(_) => {
                finish_failed(
                    &store,
                    &mut meta,
                    None,
                    "evolve worker pool closed".to_owned(),
                );
                return;
            }
        }
    } else {
        None
    };

    // QE-461 §5.3: a composite flow ALSO acquires a separate flow permit (default 1) so a multi-hour flow
    // serialises against other flows without starving interactive backtests — a byte-for-byte mirror of the
    // evolve permit above. Held for the whole sequence.
    let _flow_permit = if matches!(spec, RunSpec::Flow(_)) {
        match flow_permits.acquire().await {
            Ok(p) => Some(p),
            Err(_) => {
                finish_failed(
                    &store,
                    &mut meta,
                    None,
                    "flow worker pool closed".to_owned(),
                );
                return;
            }
        }
    } else {
        None
    };

    meta.status = RunStatus::Running;
    meta.started_ms = Some(now_ms());
    let _ = store.write_meta(&meta);

    // QE-460: a composite flow is not a single subprocess — the supervisor SEQUENCES the existing `train`
    // and `backtest` CLI sub-jobs under one run-store row. Delegate to the flow supervisor (which holds the
    // pool permit `_permit` for the whole sequence) and return.
    if let RunSpec::Flow(params) = &spec {
        supervise_flow(&store, spawner.as_ref(), run_deadline, &mut meta, params).await;
        return;
    }

    let run_dir = store.run_dir(&meta.id);
    let mut child = match spawner.spawn(&run_dir, &spec) {
        Ok(child) => child,
        Err(e) => {
            finish_failed(&store, &mut meta, None, format!("failed to spawn job: {e}"));
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Drain stdout (progress → meta + stdout.log) and stderr (tail) concurrently to avoid a pipe
    // deadlock if the child writes a lot to both. QE-454 §13.10: the whole drain is wrapped in a per-run
    // wall-clock deadline — a run that has not terminated by `run_deadline` is aborted (the child is killed
    // via `kill_on_drop` + an explicit `start_kill`) and terminally failed, so a runaway campaign can never
    // hold a supervisor open forever.
    let drain = async {
        let stdout_fut = drain_stdout(stdout, &store, &mut meta, &spec);
        let stderr_fut = drain_stderr_tail(stderr);
        tokio::join!(stdout_fut, stderr_fut)
    };
    let (done_seen, err_tail, deadline_exceeded) =
        match tokio::time::timeout(run_deadline, drain).await {
            Ok((done_seen, err_tail)) => (done_seen, err_tail, false),
            Err(_) => {
                // Ceiling hit: kill the child so its `wait()` returns promptly (drop also fires
                // `kill_on_drop`), then fall through to the terminal-mark below.
                let _ = child.start_kill();
                (false, String::new(), true)
            }
        };

    let exit = child.wait().await.ok().and_then(|s| s.code());
    meta.exit = exit;
    meta.finished_ms = Some(now_ms());

    if deadline_exceeded {
        finish_failed(&store, &mut meta, exit, RUN_DEADLINE_REASON.to_owned());
    } else if done_seen && exit == Some(0) && store.result_path(&meta.id).exists() {
        meta.status = RunStatus::Succeeded;
        meta.error = None;
        meta.artifacts = vec!["result.json".to_owned()];
        // A succeeded train/evolve run should read 100% — its last coarse stage was the gate/seal line.
        // The backtest job reports its own terminal `report` pct, so leave backtest progress unchanged.
        match spec {
            RunSpec::Train(_) => {
                meta.progress = Progress {
                    pct: 100,
                    stage: "done".to_owned(),
                    msg: "training complete".to_owned(),
                };
            }
            RunSpec::Evolve(_) => {
                meta.progress = Progress {
                    pct: 100,
                    stage: "done".to_owned(),
                    msg: "evolution complete".to_owned(),
                };
            }
            RunSpec::Backtest(_) => {}
            // A flow is sequenced by `supervise_flow` (returned early above) and never reaches this
            // single-child terminal path.
            RunSpec::Flow(_) => {}
        }
        let _ = store.write_meta(&meta);
    } else if done_seen && exit == Some(0) {
        // QE-407 (honest success): the job reported `done` and exited 0 but wrote no `result.json`, so
        // `GET /runs/{id}/result` would 409 on a run the UI showed green. Report the truth: `failed`.
        finish_failed(
            &store,
            &mut meta,
            exit,
            "job reported done but wrote no result.json".to_owned(),
        );
    } else {
        let msg = if err_tail.trim().is_empty() {
            format!("job exited with status {exit:?} without a `done` line")
        } else {
            err_tail
        };
        finish_failed(&store, &mut meta, exit, msg);
    }
}

/// The slippage-model label the flow's holdout backtest re-costs with — the default (`square-root-impact`)
/// selection cost model the train gate priced with. The impact/slippage model itself is content-addressed
/// from the sealed vintage (QE-431), so this label just names the model the backtest job defaults to (no
/// `--reporting-impact` override ⇒ the selection model); the taker FEE, by contrast, is not sealed, so it is
/// carried explicitly from the gate via the handoff (see [`TrainHandoff::gate_taker_fee_bps`]).
const FLOW_SLIPPAGE_MODEL: &str = "square-root-impact";
/// QE-460 (a): the flow's backtest re-surfaces the gate's holdout verdict — it is the SINGLE recorded
/// consultation, not a fresh OOS look — so it confers no independent deflation credit.
const FLOW_SINGLE_CONSULTATION_NOTE: &str =
    "flow backtest evaluates ON the frozen holdout (single recorded consultation, no independent credit)";

/// Build the flow's holdout-backtest params, **pinned to the sealed gate cost model** (QE-460 (d), maxdama
/// #6): the vintage handoff (content hash), the frozen holdout window `[start,end)`, the config-derived
/// instrument, and — crucially — the **taker fee the G1 gate actually priced the holdout with**, carried from
/// the train sub-run (`gate_taker_fee_bps`) rather than a standalone default. The operator cannot choose the
/// backtest window or a friendlier friction model — both are server-derived from the gate.
fn flow_backtest_params(
    vintage: String,
    start: String,
    end: String,
    resolution: String,
    instrument: String,
    gate_taker_fee_bps: f64,
) -> BacktestParams {
    BacktestParams {
        vintage,
        strategy: None,
        start,
        end,
        resolution,
        universe: vec![instrument],
        // The EXACT fee the gate used (from the handoff) — never a divergent default.
        taker_fee_bps: gate_taker_fee_bps,
        slippage_model: FLOW_SLIPPAGE_MODEL.to_owned(),
    }
}

/// Cost-parity guard (QE-460 (d), maxdama #6): the flow's holdout backtest must re-cost under the **same**
/// cost calibration the train gate used — the gate's own taker fee (`gate_taker_fee_bps`, carried from the
/// train sub-run) and the sealed selection slippage model. Returns `false` if the backtest params carry a
/// friendlier (or any different) friction model than the gate's, which fails the flow rather than letting the
/// backtest re-cost the holdout cheaper than the gate.
fn flow_cost_parity_ok(bp: &BacktestParams, gate_taker_fee_bps: f64) -> bool {
    (bp.taker_fee_bps - gate_taker_fee_bps).abs() < f64::EPSILON
        && bp.slippage_model == FLOW_SLIPPAGE_MODEL
}

/// The train→backtest handoff the flow supervisor reads from the train sub-run's `result.json` (parsed as an
/// opaque `serde_json::Value`, so no `qe-cli` crate edge is added — the firewall stays green): the frozen
/// holdout window the train sub-job carved, the config-derived instrument it trained over, and the exact
/// taker fee the G1 gate priced the holdout with (so the backtest re-costs under the identical fee).
struct TrainHandoff {
    holdout_start: String,
    holdout_end: String,
    resolution: String,
    instrument: String,
    /// The gate's own taker fee (bps) — the fee the train sub-run priced the G1 holdout evaluation with.
    gate_taker_fee_bps: f64,
}

/// Parse the train sub-run's `result.json` for the frozen-holdout handoff (QE-460): the resolved holdout
/// window (`holdout_window.{start,end,resolution}`) the train phase carved from the pinned snapshot's right
/// edge, the `instrument` it trained over (the config-derived backtest universe), and the `gate_taker_fee_bps`
/// the gate priced the holdout with (so the backtest pins the identical fee — cost parity). `None` if the
/// file is missing/unparseable or lacks any required field (the flow then fails cleanly).
fn read_train_handoff(result_path: &std::path::Path) -> Option<TrainHandoff> {
    let bytes = std::fs::read(result_path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let hw = v.get("holdout_window")?;
    Some(TrainHandoff {
        holdout_start: hw.get("start")?.as_str()?.to_owned(),
        holdout_end: hw.get("end")?.as_str()?.to_owned(),
        resolution: hw.get("resolution")?.as_str()?.to_owned(),
        instrument: v.get("instrument")?.as_str()?.to_owned(),
        gate_taker_fee_bps: v.get("gate_taker_fee_bps")?.as_f64()?,
    })
}

/// Spawn + tail one already-spawned sub-job `Child` to completion under the per-run wall-clock deadline,
/// folding its stdout progress into the flow `meta` (shared coarse progress + `stdout.log`) exactly like the
/// single-child supervisor. Returns `(done_seen, exit_code, stderr_tail, deadline_exceeded)`.
async fn drain_child(
    mut child: tokio::process::Child,
    store: &RunStore,
    meta: &mut RunMeta,
    spec: &RunSpec,
    run_deadline: Duration,
) -> (bool, Option<i32>, String, bool) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let drain = async {
        let stdout_fut = drain_stdout(stdout, store, meta, spec);
        let stderr_fut = drain_stderr_tail(stderr);
        tokio::join!(stdout_fut, stderr_fut)
    };
    let (done_seen, err_tail, deadline_exceeded) =
        match tokio::time::timeout(run_deadline, drain).await {
            Ok((done_seen, err_tail)) => (done_seen, err_tail, false),
            Err(_) => {
                let _ = child.start_kill();
                (false, String::new(), true)
            }
        };
    let exit = child.wait().await.ok().and_then(|s| s.code());
    (done_seen, exit, err_tail, deadline_exceeded)
}

/// QE-461 §5.3 — the startup reconciler's decision for a single non-terminal (orphaned) run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanAction {
    /// A dead run (non-flow orphan, or a flow with no sealed-vintage checkpoint / no readable train handoff,
    /// or a flow whose backtest already produced its artefact): terminally mark `failed`, never re-search.
    Fail,
    /// A **resumable** flow — vintage sealed ∧ backtest incomplete: re-spawn ONLY the backtest phase from the
    /// sealed-vintage checkpoint.
    ResumeBacktest,
}

/// QE-461 §5.3 — **the resume predicate.** A non-terminal orphaned flow is [`OrphanAction::ResumeBacktest`]
/// iff **all** hold; anything else (and every non-flow orphan) is [`OrphanAction::Fail`]:
/// - it is a `flow` run;
/// - **vintage sealed**: the recorded `meta.train.vintage` is present **and** the train sub-run's
///   `train/result.json` exists — the durable, content-addressed checkpoint plus its readable frozen-holdout
///   handoff (needed to rebuild the deterministic backtest);
/// - **backtest incomplete**: the backtest sub-run's `backtest/result.json` does **not** exist.
///
/// This is a **pure** classifier (only `meta` + on-disk artefact existence) so it is directly unit-testable
/// and shared by both [`RunManager::reconcile_orphans`] (skip resumable) and
/// [`RunManager::resume_orphaned_flows`] (act on resumable).
fn classify_orphan(store: &RunStore, meta: &RunMeta) -> OrphanAction {
    if meta.run_type != "flow" {
        return OrphanAction::Fail;
    }
    let flow_dir = store.run_dir(&meta.id);
    let vintage_sealed = meta
        .train
        .as_ref()
        .and_then(|t| t.vintage.as_ref())
        .is_some()
        && flow_dir.join("train").join("result.json").exists();
    let backtest_complete = flow_dir.join("backtest").join("result.json").exists();
    if vintage_sealed && !backtest_complete {
        OrphanAction::ResumeBacktest
    } else {
        OrphanAction::Fail
    }
}

/// QE-461 §5.3 — resume an orphaned flow from its sealed-vintage checkpoint by running **only** the backtest
/// phase. Acquires the shared pool permit **and** the flow-lane permit (a resumed flow respects the same lane
/// as a fresh one), rebuilds the [`super::model::FlowProgress`] from the recorded `meta.flow`, then hands off
/// to the shared [`flow_backtest_phase`] — which rebuilds the deterministic backtest from the recorded
/// vintage + train handoff. The search is **never** re-run.
async fn supervise_flow_resume(
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    flow_permits: Arc<Semaphore>,
    run_deadline: Duration,
    mut meta: RunMeta,
    vintage: String,
) {
    let exit = meta.exit;
    let _permit = match permits.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            finish_failed(&store, &mut meta, exit, "worker pool closed".to_owned());
            return;
        }
    };
    let _flow_permit = match flow_permits.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            finish_failed(
                &store,
                &mut meta,
                exit,
                "flow worker pool closed".to_owned(),
            );
            return;
        }
    };

    // Fresh per-flow clock for the resumed backtest (it gets the full ceiling — the prior wall-clock is gone).
    let flow_start = Instant::now();
    let flow = meta.flow.clone().unwrap_or_default();
    meta.status = RunStatus::Running;
    meta.progress = Progress {
        pct: 88,
        stage: "flow-resume".to_owned(),
        msg: "flow: resuming backtest from the sealed-vintage checkpoint".to_owned(),
    };
    let _ = store.write_meta(&meta);

    flow_backtest_phase(
        &store,
        spawner.as_ref(),
        run_deadline,
        flow_start,
        &mut meta,
        flow,
        vintage,
    )
    .await;
}

/// QE-460 — supervise a composite flow: sequence the `train` sub-job (sealing a vintage over the frozen,
/// regime-stratified OOS holdout carved once) then the `backtest` sub-job over that same holdout with the
/// just-sealed vintage id. **Atomic**: the flow succeeds only if both sub-runs succeed and a vintage sealed;
/// a train that fails G1 seals nothing and runs **no** backtest (design §5.2). One run-store row, one
/// status; the sub-run ids + the frozen holdout are recorded in `meta.flow`.
async fn supervise_flow(
    store: &RunStore,
    spawner: &dyn JobSpawner,
    run_deadline: Duration,
    meta: &mut RunMeta,
    params: &FlowParams,
) {
    use super::model::FlowProgress;

    // QE-461 §5.3: a single per-flow wall-clock clock. Each sub-phase drains under the *remaining* budget
    // (`run_deadline − elapsed`), so the ceiling bounds the whole train→backtest flow rather than each phase
    // independently — reusing the existing timeout→`start_kill`→`RUN_DEADLINE_REASON` abort pattern.
    let flow_start = Instant::now();

    let flow_dir = store.run_dir(&meta.id);
    let train_dir = flow_dir.join("train");
    if let Err(e) = std::fs::create_dir_all(&train_dir) {
        finish_failed(
            store,
            meta,
            None,
            format!("failed to create flow train dir: {e}"),
        );
        return;
    }
    let mut flow = FlowProgress {
        train_run: Some("train".to_owned()),
        ..FlowProgress::default()
    };
    meta.flow = Some(flow.clone());
    meta.progress = Progress {
        pct: 5,
        stage: "flow-train".to_owned(),
        msg: "flow: training over the frozen holdout".to_owned(),
    };
    let _ = store.write_meta(meta);

    // ---- train phase (carves + seals over the frozen holdout, records the lineage) ------------------
    let train_params = params.to_train_params();
    let child = match spawner.spawn_flow_train(&train_dir, &train_params) {
        Ok(child) => child,
        Err(e) => {
            finish_failed(
                store,
                meta,
                None,
                format!("failed to spawn flow train: {e}"),
            );
            return;
        }
    };
    // `drain_stdout` folds the train sub-run's terminal `done` vintage + its G1 gate verdict into
    // `meta.train`.
    let train_spec = RunSpec::Train(train_params);
    let (done, exit, err_tail, deadline) = drain_child(
        child,
        store,
        meta,
        &train_spec,
        run_deadline.saturating_sub(flow_start.elapsed()),
    )
    .await;
    let vintage = meta.train.as_ref().and_then(|t| t.vintage.clone());
    // The G1 verdict the train sub-run emitted (`gate.promoted`). The flow's atomic verdict rides it: a
    // train that fails G1 fails the flow and runs NO backtest (design §5.2), even though the CLI seal itself
    // is untouched (a train always seals its vintage; the flow — not the seal — enforces the gate here).
    let promoted = meta
        .train
        .as_ref()
        .and_then(|t| t.gate.as_ref())
        .map(|g| g.promoted)
        .unwrap_or(false);
    let sealed_ok = !deadline
        && done
        && exit == Some(0)
        && train_dir.join("result.json").exists()
        && vintage.is_some();
    if !sealed_ok || !promoted {
        let reason = if deadline {
            RUN_DEADLINE_REASON.to_owned()
        } else if sealed_ok && !promoted {
            "flow train failed the G1 gate — nothing promoted, no backtest run".to_owned()
        } else if done && exit == Some(0) && vintage.is_none() {
            "flow train sealed no vintage — no backtest run".to_owned()
        } else if !err_tail.trim().is_empty() {
            err_tail
        } else {
            format!("flow train sub-job failed (exit {exit:?})")
        };
        finish_failed(store, meta, exit, reason);
        return;
    }
    let vintage = vintage.expect("checked Some above");
    flow.vintage = Some(vintage.clone());

    // The sealed vintage is the checkpoint. Hand off to the shared backtest phase (also the QE-461 resume
    // entry point), which rebuilds the deterministic holdout backtest from the recorded handoff.
    flow_backtest_phase(
        store,
        spawner,
        run_deadline,
        flow_start,
        meta,
        flow,
        vintage,
    )
    .await;
}

/// The **backtest phase** of a composite flow, shared by the initial [`supervise_flow`] and the QE-461
/// resume path ([`supervise_flow_resume`]). Reads the train sub-run's frozen-holdout handoff, pins the
/// backtest to the sealed gate cost model (cost-parity guard), spawns + drains the holdout backtest under
/// the *remaining* per-flow deadline, and records the terminal outcome. Because it rebuilds the backtest
/// params from the **recorded** vintage + handoff (frozen holdout window + gate taker fee), a resumed run
/// rides the SAME sealed checkpoint deterministically — no search re-runs.
async fn flow_backtest_phase(
    store: &RunStore,
    spawner: &dyn JobSpawner,
    run_deadline: Duration,
    flow_start: Instant,
    meta: &mut RunMeta,
    mut flow: super::model::FlowProgress,
    vintage: String,
) {
    let flow_dir = store.run_dir(&meta.id);
    let train_dir = flow_dir.join("train");

    // ---- read the frozen holdout window + instrument the train phase resolved -----------------------
    let handoff = match read_train_handoff(&train_dir.join("result.json")) {
        Some(h) => h,
        None => {
            finish_failed(
                store,
                meta,
                meta.exit,
                "flow train result.json missing the holdout handoff (holdout_window/instrument)"
                    .to_owned(),
            );
            return;
        }
    };
    flow.holdout_start = Some(handoff.holdout_start.clone());
    flow.holdout_end = Some(handoff.holdout_end.clone());

    // ---- cost-parity guard (maxdama #6): pin the backtest to the GATE'S OWN taker fee + slippage ----
    let bp = flow_backtest_params(
        vintage.clone(),
        handoff.holdout_start.clone(),
        handoff.holdout_end.clone(),
        handoff.resolution.clone(),
        handoff.instrument.clone(),
        handoff.gate_taker_fee_bps,
    );
    if !flow_cost_parity_ok(&bp, handoff.gate_taker_fee_bps) {
        finish_failed(
            store,
            meta,
            meta.exit,
            "flow backtest cost model diverged from the sealed gate calibration (cost-parity breach)"
                .to_owned(),
        );
        return;
    }

    // ---- backtest phase over the FROZEN holdout (the single recorded consultation) ------------------
    let backtest_dir = flow_dir.join("backtest");
    if let Err(e) = std::fs::create_dir_all(&backtest_dir) {
        finish_failed(
            store,
            meta,
            meta.exit,
            format!("failed to create flow backtest dir: {e}"),
        );
        return;
    }
    flow.backtest_run = Some("backtest".to_owned());
    meta.flow = Some(flow.clone());
    meta.progress = Progress {
        pct: 90,
        stage: "flow-backtest".to_owned(),
        msg: format!(
            "flow: backtesting sealed vintage on the holdout ({FLOW_SINGLE_CONSULTATION_NOTE})"
        ),
    };
    let _ = store.write_meta(meta);

    let bt_spec = RunSpec::Backtest(bp);
    let child = match spawner.spawn(&backtest_dir, &bt_spec) {
        Ok(child) => child,
        Err(e) => {
            finish_failed(
                store,
                meta,
                meta.exit,
                format!("failed to spawn flow backtest: {e}"),
            );
            return;
        }
    };
    let (bt_done, bt_exit, bt_err, bt_deadline) = drain_child(
        child,
        store,
        meta,
        &bt_spec,
        run_deadline.saturating_sub(flow_start.elapsed()),
    )
    .await;
    let bt_ok =
        !bt_deadline && bt_done && bt_exit == Some(0) && backtest_dir.join("result.json").exists();
    if !bt_ok {
        let reason = if bt_deadline {
            RUN_DEADLINE_REASON.to_owned()
        } else if !bt_err.trim().is_empty() {
            bt_err
        } else {
            format!("flow backtest sub-job failed (exit {bt_exit:?})")
        };
        finish_failed(store, meta, bt_exit, reason);
        return;
    }

    // ---- atomic success: both sub-runs succeeded and the vintage sealed ------------------------------
    meta.status = RunStatus::Succeeded;
    meta.error = None;
    meta.exit = Some(0);
    meta.finished_ms = Some(now_ms());
    meta.artifacts = vec![
        "train/result.json".to_owned(),
        "backtest/result.json".to_owned(),
    ];
    meta.progress = Progress {
        pct: 100,
        stage: "done".to_owned(),
        msg: "flow complete: train sealed + holdout backtest".to_owned(),
    };
    meta.flow = Some(flow);
    let _ = store.write_meta(meta);
}

/// Read subprocess stdout line by line: append each raw line to `stdout.log` and, on a `progress`
/// line, update `meta.progress` and persist. Returns whether a terminal `done` line was seen.
async fn drain_stdout(
    stdout: Option<tokio::process::ChildStdout>,
    store: &RunStore,
    meta: &mut RunMeta,
    spec: &RunSpec,
) -> bool {
    let Some(stdout) = stdout else { return false };
    let log_path = store.stdout_path(&meta.id);
    let mut lines = BufReader::new(stdout).lines();
    let mut done_seen = false;
    while let Ok(Some(line)) = lines.next_line().await {
        // Append the raw line to the captured log (best-effort).
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{line}");
        }
        match serde_json::from_str::<ProgressLine>(&line) {
            Ok(ProgressLine::Progress { pct, stage, msg }) => {
                meta.progress = Progress { pct, stage, msg };
                let _ = store.write_meta(meta);
            }
            Ok(ProgressLine::Gen {
                pct,
                // `stage` is fixed (`"search"`) on this variant — the server derives its own coarse
                // stage label below, so the emitted one is intentionally ignored.
                stage: _,
                generation,
                generations,
                coverage,
                coverage_long,
                coverage_short,
                best_fitness,
            }) => {
                meta.progress = Progress {
                    pct,
                    stage: "search".to_owned(),
                    msg: format!("generation {generation}/{generations}"),
                };
                train_mut(meta).generation = Some(GenSnapshot {
                    generation,
                    generations,
                    coverage,
                    coverage_long,
                    coverage_short,
                    best_fitness,
                });
                let _ = store.write_meta(meta);
            }
            Ok(ProgressLine::Ensemble {
                pct,
                stage: _,
                folds,
                members,
                score,
            }) => {
                meta.progress = Progress {
                    pct,
                    stage: "ensemble".to_owned(),
                    msg: format!("ensemble: {members} members over {folds} folds"),
                };
                train_mut(meta).ensemble = Some(EnsembleSnapshot {
                    folds,
                    members,
                    score,
                });
                let _ = store.write_meta(meta);
            }
            Ok(ProgressLine::Gate {
                pct,
                stage: _,
                promoted,
                failed,
                in_sample_sharpe,
                holdout_sharpe,
                dsr,
                spa_pvalue,
                n_trials,
                uncensored_pbo,
                variance_trials,
                distinct_evaluations,
            }) => {
                meta.progress = Progress {
                    pct,
                    stage: "gate".to_owned(),
                    msg: format!("G1 {}", if promoted { "passed" } else { "failed" }),
                };
                // QE-454 Phase B: the three GP-deflation fields pass through absent-by-default — the normal
                // train `gate` line carries `None`, so `GateSnapshot`/`meta.json` stay byte-identical.
                train_mut(meta).gate = Some(GateSnapshot {
                    promoted,
                    failed,
                    in_sample_sharpe,
                    holdout_sharpe,
                    dsr,
                    spa_pvalue,
                    n_trials,
                    uncensored_pbo,
                    variance_trials,
                    distinct_evaluations,
                });
                let _ = store.write_meta(meta);
            }
            Ok(ProgressLine::Done {
                protocol_version,
                vintage,
                pool,
                ..
            }) => {
                done_seen = true;
                // QE-406: the terminal line carries the run-protocol version. On mismatch we log and
                // continue (never reject) — dropping a completed run's terminal line would regress live
                // monitoring; a warning gives the operability signal without any behaviour loss. A
                // legacy `done` with no version deserializes to `0`, which trips this too.
                if protocol_version != PROTOCOL_VERSION {
                    tracing::warn!(
                        run_id = %meta.id,
                        emitted = protocol_version,
                        expected = PROTOCOL_VERSION,
                        "run subprocess emitted a mismatched run-protocol version; \
                         continuing (progress may be interpreted on a best-effort basis)"
                    );
                }
                if matches!(spec, RunSpec::Evolve(_)) {
                    // QE-452 §13.3 load-bearing invariant: an evolve run produces a **pool**, NEVER a
                    // vintage. Assert the two lifecycles stay separated at the terminal line — a stray
                    // vintage from an evolve subprocess is a protocol breach; refuse to record it.
                    debug_assert!(
                        vintage.is_none(),
                        "evolve run's terminal `done` carried a vintage — lifecycle-separation breach"
                    );
                    if vintage.is_some() {
                        tracing::warn!(
                            run_id = %meta.id,
                            "evolve run's terminal `done` carried a vintage; ignoring — an evolve run \
                             never writes a vintage (QE-452 §13.3)"
                        );
                    }
                    if let Some(pool) = pool {
                        train_mut(meta).pool = Some(pool);
                        let _ = store.write_meta(meta);
                    }
                } else if let Some(vintage) = vintage {
                    train_mut(meta).vintage = Some(vintage);
                    let _ = store.write_meta(meta);
                }
            }
            Ok(ProgressLine::Error { .. }) | Err(_) => {}
        }
    }
    done_seen
}

/// Mutable access to the run's [`TrainProgress`], created on first use. Backtest runs never emit the
/// train variants, so `meta.train` stays `None` and their `meta.json` shape is unchanged.
fn train_mut(meta: &mut RunMeta) -> &mut TrainProgress {
    meta.train.get_or_insert_with(TrainProgress::default)
}

/// Read subprocess stderr fully, returning the last [`STDERR_TAIL_BYTES`] as a lossy-UTF-8 string.
async fn drain_stderr_tail(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await;
    let start = buf.len().saturating_sub(STDERR_TAIL_BYTES);
    String::from_utf8_lossy(&buf[start..]).trim().to_owned()
}

/// Record a terminal `failed` outcome with an error message.
fn finish_failed(store: &RunStore, meta: &mut RunMeta, exit: Option<i32>, error: String) {
    meta.status = RunStatus::Failed;
    meta.exit = exit;
    if meta.finished_ms.is_none() {
        meta.finished_ms = Some(now_ms());
    }
    meta.error = Some(error);
    let _ = store.write_meta(meta);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A create-run request for an `evolve` campaign with the given `params` object.
    fn evolve_req(params: serde_json::Value) -> CreateRunRequest {
        CreateRunRequest {
            run_type: "evolve".to_owned(),
            params,
        }
    }

    /// A valid baseline evolve params object (seed + window present, no cap violations).
    fn valid_evolve_params() -> serde_json::Value {
        json!({ "seed": 7, "start": "2021-01-01", "end": "2021-01-10", "resolution": "1h" })
    }

    #[test]
    fn build_spec_accepts_a_valid_evolve_request() {
        let spec = build_spec(&evolve_req(valid_evolve_params())).expect("valid evolve spec");
        match spec {
            RunSpec::Evolve(p) => {
                assert_eq!(p.seed, 7);
                assert_eq!(p.mode, qe_run_protocol::EvolveMode::Sandbox);
            }
            other => panic!("expected Evolve, got {other:?}"),
        }
    }

    /// Drive `build_spec` with a params object built from the valid baseline plus an override, asserting
    /// it is rejected as a `Validation` error whose message names `needle`.
    fn assert_rejected(
        mut params: serde_json::Value,
        key: &str,
        value: serde_json::Value,
        needle: &str,
    ) {
        params[key] = value;
        let err = build_spec(&evolve_req(params)).expect_err("must reject");
        match err {
            CreateError::Validation(msg) => assert!(
                msg.contains(needle),
                "expected rejection mentioning `{needle}`, got: {msg}"
            ),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn validate_evolve_rejects_depth_over_cap() {
        assert_rejected(valid_evolve_params(), "depth", json!(5), "depth");
    }

    #[test]
    fn validate_evolve_rejects_nodes_over_cap() {
        assert_rejected(valid_evolve_params(), "nodes", json!(17), "nodes");
    }

    #[test]
    fn validate_evolve_rejects_lookback_over_cap() {
        assert_rejected(valid_evolve_params(), "lookback", json!(201), "lookback");
    }

    #[test]
    fn validate_evolve_rejects_k_over_cap() {
        assert_rejected(valid_evolve_params(), "k", json!(17), "k");
    }

    #[test]
    fn validate_evolve_rejects_off_lattice_window() {
        assert_rejected(valid_evolve_params(), "windows", json!([5, 7]), "lattice");
    }

    #[test]
    fn validate_evolve_rejects_missing_seed() {
        // Drop the seed entirely — a serde reject wrapped as a clear `400`.
        let err = build_spec(&evolve_req(
            json!({ "start": "2021-01-01", "end": "2021-01-10", "resolution": "1h" }),
        ))
        .expect_err("missing seed must reject");
        match err {
            CreateError::Validation(msg) => assert!(msg.contains("seed"), "message: {msg}"),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn validate_evolve_rejects_bad_mode() {
        assert_rejected(
            valid_evolve_params(),
            "mode",
            json!("prod"),
            "evolve params",
        );
    }

    #[test]
    fn validate_evolve_rejects_missing_window() {
        let err =
            build_spec(&evolve_req(json!({ "seed": 7 }))).expect_err("missing window rejects");
        assert!(matches!(err, CreateError::Validation(_)));
    }

    #[test]
    fn production_launch_is_refused_when_the_basis_is_unsatisfied() {
        use qe_run_protocol::EvolveMode;
        // The gate is pure/injectable so the `const < REQUIRED` case is testable even though the compiled
        // const is satisfied. An unsatisfied basis (0) refuses a PRODUCTION launch with a 400 naming the
        // missing prereqs — a tampered client cannot even launch a production campaign.
        let err = validate_evolve_basis(EvolveMode::Production, 0)
            .expect_err("production must be refused when the basis is unsatisfied");
        match err {
            CreateError::Validation(msg) => {
                assert!(msg.contains("DEFLATION_BASIS_VERSION"), "message: {msg}");
                assert!(msg.contains("QE-439"), "must name a missing prereq: {msg}");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
        // Sandbox is NEVER gated on the basis (research can always launch).
        assert!(validate_evolve_basis(EvolveMode::Sandbox, 0).is_ok());
        // A fully-satisfied basis lets production launch.
        assert!(validate_evolve_basis(
            EvolveMode::Production,
            qe_validation::REQUIRED_DEFLATION_BASIS
        )
        .is_ok());
        // And the compiled const is satisfied (prereqs merged), so a real production launch is allowed.
        assert!(qe_validation::deflation_basis_satisfied());
    }

    #[test]
    fn evolve_spec_never_writes_a_vintage() {
        // The load-bearing lifecycle predicate: an evolve spec is not a vintage-writing run.
        let spec = build_spec(&evolve_req(valid_evolve_params())).unwrap();
        assert!(!spec.writes_vintage(), "evolve must never write a vintage");
        assert_eq!(spec.run_type(), "evolve");
    }

    // ---- QE-458 steer whitelist / blocklist / regime-coverage invariant (validate_train) --------------

    /// A valid baseline train params object (window present; no steer knobs).
    fn valid_train_params() -> serde_json::Value {
        json!({ "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h" })
    }

    fn train_req(params: serde_json::Value) -> CreateRunRequest {
        CreateRunRequest {
            run_type: "train".to_owned(),
            params,
        }
    }

    /// Drive `build_spec` with a train params override, asserting a `Validation` `400` naming `needle`.
    fn assert_train_rejected(key: &str, value: serde_json::Value, needle: &str) {
        let mut params = valid_train_params();
        params[key] = value;
        let err = build_spec(&train_req(params)).expect_err("train request must reject");
        match err {
            CreateError::Validation(msg) => assert!(
                msg.contains(needle),
                "expected rejection mentioning `{needle}`, got: {msg}"
            ),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    fn assert_train_accepted(params: serde_json::Value) {
        build_spec(&train_req(params)).expect("train request must be accepted");
    }

    #[test]
    fn validate_train_accepts_the_whitelisted_steer_knobs() {
        // Budget + indicator subset + windows/folds at/above their floors is a clean accept (these are
        // applied live by `run_train_job`).
        assert_train_accepted(json!({
            "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h",
            "generations": 80, "population": 24,
            "indicator_subset": ["rsi_14", "atr_pct"],
            "windows": 6, "folds": 4
        }));
    }

    #[test]
    fn validate_train_rejects_evolved_pool_as_not_yet_supported_not_silently_ignored() {
        // Evolved-pool-as-indicator steering is not yet applied on the live search, so it is REJECTED
        // (never accepted-then-silently-ignored) — item 4 non-negotiable.
        assert_train_rejected("evolved_pool", json!("pool-abc"), "evolved-pool");
        assert_train_rejected("evolved_formulas", json!(["aa", "bb"]), "evolved-pool");
    }

    #[test]
    fn validate_train_rejects_gate_decision_knobs_in_any_direction() {
        // The six gate-decision knobs are NOT steerable — a request that names one is a `400` regardless of
        // value. This closes the hole where a cap/ceiling knob could be RAISED to relax the gate: e.g. a
        // high PBO cutoff (0.9) would loosen the primary GP gate, so it must be rejected, not accepted.
        for (key, relaxing_value) in [
            ("cost_stress_multiplier", json!(0.5)), // below 1× — would soften the cost stress
            ("max_turnover_frac", json!(0.9)),      // ABOVE the cap — would loosen turnover
            ("capacity_floor_usd", json!(1_000)), // below floor — would shrink capacity discipline
            ("dsr_cutoff", json!(0.5)),           // below floor — would lower the DSR bar
            ("pbo_cutoff", json!(0.9)), // ABOVE floor — would loosen the PBO gate (the hole)
            ("ic_fdr_threshold", json!(0.9)), // any set — not editable
        ] {
            assert_train_rejected(key, relaxing_value, key);
        }
    }

    #[test]
    fn validate_train_rejects_floored_holdout_embargo_purge_below_floor() {
        assert_train_rejected("holdout", json!(10), "holdout");
        assert_train_rejected("embargo", json!(0), "embargo");
        assert_train_rejected("purge", json!(0), "purge");
    }

    #[test]
    fn validate_train_floored_knobs_may_be_raised_a_safe_tighten() {
        // Holdout/embargo/purge PRE-EXIST (QE-261) and may be RAISED above their floor — the safe direction.
        assert_train_accepted(json!({
            "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h",
            "holdout": 500, "embargo": 24, "purge": 5
        }));
    }

    #[test]
    fn validate_train_regime_invariant_rejects_thin_windows_and_folds() {
        // §6.1a(d): window/fold counts below the floor shrink OOS/regime coverage → `400`.
        assert_train_rejected("windows", json!(2), "windows");
        assert_train_rejected("folds", json!(1), "folds");
        // At/above the floor regime coverage is preserved → accepted.
        assert_train_accepted(json!({
            "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h",
            "windows": 4, "folds": 2
        }));
    }

    // ---- QE-460 validate_flow (reuses the QE-458 whitelist via `to_train_params`) ---------------------

    fn flow_req(params: serde_json::Value) -> CreateRunRequest {
        CreateRunRequest {
            run_type: "flow".to_owned(),
            params,
        }
    }

    fn valid_flow_params() -> serde_json::Value {
        json!({ "seed": 7, "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h" })
    }

    fn assert_flow_rejected(key: &str, value: serde_json::Value, needle: &str) {
        let mut params = valid_flow_params();
        params[key] = value;
        let err = build_spec(&flow_req(params)).expect_err("flow request must reject");
        match err {
            CreateError::Validation(msg) => assert!(
                msg.contains(needle),
                "expected rejection mentioning `{needle}`, got: {msg}"
            ),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn build_spec_accepts_a_valid_flow_request() {
        let spec = build_spec(&flow_req(valid_flow_params())).expect("valid flow spec");
        match spec {
            RunSpec::Flow(p) => {
                assert_eq!(p.seed, 7);
                assert_eq!(p.start, "2021-01-01");
                // A flow always writes a vintage (its train phase seals one).
                assert!(RunSpec::Flow(p).writes_vintage());
            }
            other => panic!("expected Flow, got {other:?}"),
        }
    }

    #[test]
    fn validate_flow_requires_seed() {
        // Missing seed is a serde reject wrapped as a clear `400` (a flow must be byte-reproducible).
        let err = build_spec(&flow_req(
            json!({ "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h" }),
        ))
        .expect_err("missing seed must reject");
        match err {
            CreateError::Validation(msg) => assert!(msg.contains("seed"), "message: {msg}"),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn validate_flow_requires_the_window() {
        assert_flow_rejected("start", json!(""), "start");
        assert_flow_rejected("end", json!(""), "end");
        assert_flow_rejected("resolution", json!(""), "resolution");
    }

    #[test]
    fn validate_flow_accepts_the_whitelisted_steer_knobs() {
        build_spec(&flow_req(json!({
            "seed": 7, "start": "2021-01-01", "end": "2021-06-01", "resolution": "1h",
            "generations": 80, "population": 24,
            "indicator_subset": ["rsi_14", "atr_pct"],
            "windows": 6, "folds": 4, "holdout": 300, "embargo": 24
        })))
        .expect("whitelisted steer knobs at/above their floors must be accepted");
    }

    #[test]
    fn validate_flow_rejects_gate_decision_knobs_and_sub_floor_holdout_embargo() {
        // The blocklist + floors are enforced verbatim by the reused `validate_train` — a flow can NEVER
        // steer past a gate floor a `train` could not (design §4/§6.2).
        assert_flow_rejected("pbo_cutoff", json!(0.9), "pbo_cutoff");
        assert_flow_rejected(
            "cost_stress_multiplier",
            json!(0.5),
            "cost_stress_multiplier",
        );
        assert_flow_rejected("holdout", json!(10), "holdout");
        assert_flow_rejected("embargo", json!(0), "embargo");
        assert_flow_rejected("windows", json!(2), "windows");
        assert_flow_rejected("evolved_pool", json!("pool-abc"), "evolved-pool");
    }

    // ---- QE-460 (d) cost-parity guard ----------------------------------------------------------------

    /// The gate's actual taker fee in bps — `FeeSchedule::default().taker = 0.0005` = 5 bps. Mirrored here
    /// (the server cannot import `qe-wfo` — firewall); the drift-proof end-to-end check that this equals the
    /// gate's real fee lives in `qe-cli` (`flow_records_the_gate_taker_fee_and_it_equals_the_gate_default`),
    /// which CAN see `FeeSchedule::default()`.
    const GATE_FEE_BPS: f64 = 5.0;

    #[test]
    fn flow_backtest_params_pin_the_gate_fee_and_pass_parity() {
        // The flow pins the backtest to the GATE'S OWN taker fee (carried from the train handoff), the frozen
        // holdout window, and the config-derived instrument — never a standalone default.
        let bp = flow_backtest_params(
            "vint-abc".to_owned(),
            "2021-05-01".to_owned(),
            "2021-06-01".to_owned(),
            "1h".to_owned(),
            "BTCUSDT".to_owned(),
            GATE_FEE_BPS,
        );
        assert_eq!(
            bp.taker_fee_bps, GATE_FEE_BPS,
            "the backtest fee is the gate's fee"
        );
        assert_eq!(bp.slippage_model, FLOW_SLIPPAGE_MODEL);
        assert_eq!(bp.universe, vec!["BTCUSDT".to_owned()]);
        assert_eq!(bp.vintage, "vint-abc");
        assert!(
            flow_cost_parity_ok(&bp, GATE_FEE_BPS),
            "the gate-fee-pinned model must pass the parity guard"
        );
    }

    #[test]
    fn flow_cost_parity_rejects_a_friendlier_friction_model_than_the_gate() {
        // A backtest re-costing the holdout under a cheaper fee (or a different slippage model) than the GATE
        // used is a cost-parity breach (maxdama #6). This is the check that would have caught the 2-vs-5-bps
        // bug: the OLD 2.0 bps literal is friendlier than the gate's 5.0 bps and now FAILS parity.
        let mut bp = flow_backtest_params(
            "v".to_owned(),
            "a".to_owned(),
            "b".to_owned(),
            "1h".to_owned(),
            "BTCUSDT".to_owned(),
            GATE_FEE_BPS,
        );
        bp.taker_fee_bps = 2.0; // the previously-shipped (wrong) literal — friendlier than the gate's 5 bps
        assert!(
            !flow_cost_parity_ok(&bp, GATE_FEE_BPS),
            "2 bps is friendlier than the gate's 5 bps — must fail parity"
        );
        // Any drift from the gate fee fails, in either direction.
        bp.taker_fee_bps = 5.5;
        assert!(!flow_cost_parity_ok(&bp, GATE_FEE_BPS));
        // A tampered slippage model also fails.
        let mut bp2 = flow_backtest_params(
            "v".to_owned(),
            "a".to_owned(),
            "b".to_owned(),
            "1h".to_owned(),
            "BTCUSDT".to_owned(),
            GATE_FEE_BPS,
        );
        bp2.slippage_model = "zero-impact".to_owned();
        assert!(!flow_cost_parity_ok(&bp2, GATE_FEE_BPS));
    }

    #[test]
    fn read_train_handoff_parses_window_instrument_and_gate_fee() {
        let dir = std::env::temp_dir().join(format!("qe460-handoff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("result.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "instrument": "BTCUSDT",
                "gate_taker_fee_bps": 5.0,
                "holdout_window": { "start": "2021-05-20", "end": "2021-06-01", "resolution": "1h" }
            }))
            .unwrap(),
        )
        .unwrap();
        let h = read_train_handoff(&path).expect("handoff parses");
        assert_eq!(h.instrument, "BTCUSDT");
        assert_eq!(h.holdout_start, "2021-05-20");
        assert_eq!(h.holdout_end, "2021-06-01");
        assert_eq!(h.resolution, "1h");
        assert_eq!(
            h.gate_taker_fee_bps, 5.0,
            "the gate fee is carried in the handoff"
        );
        // A result.json missing the gate fee (or the window) yields None — the flow then fails cleanly rather
        // than guessing a fee and breaking cost parity.
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "instrument": "X",
                "holdout_window": { "start": "a", "end": "b", "resolution": "1h" }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            read_train_handoff(&path).is_none(),
            "missing gate fee ⇒ no handoff"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- QE-461 §5.3 resume predicate (classify_orphan) + no-status-leak -----------------------------

    /// Build a flow `RunMeta` for `id` with the given recorded `train.vintage` (the sealed-vintage marker).
    fn flow_meta(id: &str, train_vintage: Option<&str>) -> RunMeta {
        RunMeta {
            id: id.to_owned(),
            run_type: "flow".to_owned(),
            status: RunStatus::Running,
            params: serde_json::Value::Null,
            progress: Progress::default(),
            train: train_vintage.map(|v| TrainProgress {
                vintage: Some(v.to_owned()),
                ..TrainProgress::default()
            }),
            flow: Some(crate::runs::model::FlowProgress {
                train_run: Some("train".to_owned()),
                ..crate::runs::model::FlowProgress::default()
            }),
            created_ms: 1,
            started_ms: Some(2),
            finished_ms: None,
            exit: None,
            error: None,
            artifacts: Vec::new(),
        }
    }

    /// Touch `<run>/<sub>/result.json` under the store so `classify_orphan`'s existence checks see it.
    fn touch_sub_result(store: &RunStore, id: &str, sub: &str) {
        let dir = store.run_dir(id).join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("result.json"), b"{}").unwrap();
    }

    #[test]
    fn classify_orphan_resumes_a_sealed_flow_with_an_incomplete_backtest() {
        let tmp = std::env::temp_dir().join(format!("qe461-cls-{}", uuid::Uuid::new_v4()));
        let store = RunStore::new(tmp.clone());
        let meta = flow_meta("flow-1", Some("vint-1"));
        // Vintage sealed: train.vintage recorded AND train/result.json present; backtest NOT yet written.
        touch_sub_result(&store, "flow-1", "train");
        assert_eq!(
            classify_orphan(&store, &meta),
            OrphanAction::ResumeBacktest,
            "sealed vintage ∧ incomplete backtest ⇒ resume the backtest phase"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn classify_orphan_fails_a_flow_with_no_sealed_vintage() {
        let tmp = std::env::temp_dir().join(format!("qe461-cls-{}", uuid::Uuid::new_v4()));
        let store = RunStore::new(tmp.clone());
        // No recorded train.vintage (and no train/result.json) — the search never sealed ⇒ dead, never resumed.
        let meta = flow_meta("flow-2", None);
        assert_eq!(classify_orphan(&store, &meta), OrphanAction::Fail);
        // Even a recorded vintage is NOT resumable without the readable train handoff (result.json).
        let meta_no_handoff = flow_meta("flow-2b", Some("vint-x"));
        assert_eq!(
            classify_orphan(&store, &meta_no_handoff),
            OrphanAction::Fail,
            "a recorded vintage without train/result.json is not a readable checkpoint ⇒ dead"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn classify_orphan_fails_a_flow_whose_backtest_already_completed() {
        let tmp = std::env::temp_dir().join(format!("qe461-cls-{}", uuid::Uuid::new_v4()));
        let store = RunStore::new(tmp.clone());
        let meta = flow_meta("flow-3", Some("vint-3"));
        touch_sub_result(&store, "flow-3", "train");
        touch_sub_result(&store, "flow-3", "backtest"); // backtest terminal ⇒ NOT resumable
        assert_eq!(
            classify_orphan(&store, &meta),
            OrphanAction::Fail,
            "a completed backtest ⇒ dead (nothing to resume), never re-searched"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn classify_orphan_never_resumes_a_non_flow_run() {
        let tmp = std::env::temp_dir().join(format!("qe461-cls-{}", uuid::Uuid::new_v4()));
        let store = RunStore::new(tmp.clone());
        let mut meta = flow_meta("bt-1", Some("vint-1"));
        meta.run_type = "backtest".to_owned();
        touch_sub_result(&store, "bt-1", "train");
        assert_eq!(
            classify_orphan(&store, &meta),
            OrphanAction::Fail,
            "resume is a flow-only supervision concern"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// AC5 guard: flow supervision (resume/halt) rides the terminal 4-state `RunStatus` — this exhaustive
    /// match will fail to compile if a 5th variant is ever added, and asserts exactly the four wire strings.
    #[test]
    fn flow_supervision_adds_no_run_status_variant() {
        for s in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Succeeded,
            RunStatus::Failed,
        ] {
            // Exhaustive: a new variant would make this match non-exhaustive (compile error).
            let wire = match s {
                RunStatus::Queued => "queued",
                RunStatus::Running => "running",
                RunStatus::Succeeded => "succeeded",
                RunStatus::Failed => "failed",
            };
            assert_eq!(serde_json::to_value(s).unwrap(), serde_json::json!(wire));
        }
        // The operator-halt outcome is `Failed` + a reason, not a new status.
        assert_eq!(serde_json::to_value(RunStatus::Failed).unwrap(), "failed");
        assert!(HALT_REASON.contains("halt"));
    }
}
