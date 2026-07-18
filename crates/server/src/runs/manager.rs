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
    BacktestParams, CreateRunRequest, EnsembleSnapshot, EvolveParams, GateSnapshot, GenSnapshot,
    IndexEntry, Progress, RunMeta, RunSpec, RunStatus, TrainParams, TrainProgress,
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
                    let exit = meta.exit;
                    finish_failed(&self.store, &mut meta, exit, RECONCILE_REASON.to_owned());
                    reconciled += 1;
                }
            }
        }
        Ok(reconciled)
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
        other => Err(CreateError::Validation(format!(
            "unsupported run type `{other}` (expected `backtest`, `train`, or `evolve`)"
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

/// Validate train params (QE-261): the training window is required; the budget/config are optional
/// (the `qe train` CLI supplies its own defaults) and the universe is config-derived.
fn validate_train(p: &TrainParams) -> Result<(), CreateError> {
    require("start", &p.start)?;
    require("end", &p.end)?;
    require("resolution", &p.resolution)?;
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
async fn supervise(
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    evolve_permits: Arc<Semaphore>,
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

    meta.status = RunStatus::Running;
    meta.started_ms = Some(now_ms());
    let _ = store.write_meta(&meta);

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
}
