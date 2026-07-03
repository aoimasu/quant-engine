//! Run lifecycle manager (ADR D4c): validates + creates runs, appends the index, and drives a
//! bounded worker pool of supervised subprocesses.
//!
//! Concurrency: a [`Semaphore`] with `max_concurrency` permits bounds how many subprocesses run at
//! once; runs beyond the cap block on `acquire` and remain observably `queued` until a slot frees. A
//! [`Mutex`] serialises `index.json` read-modify-write. `meta.json` is the authoritative per-run
//! record, written atomically by the supervisor on every transition/progress update.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, Semaphore};

use super::model::{CreateRunRequest, IndexEntry, Progress, RunMeta, RunStatus};
use super::spawn::JobSpawner;
use super::store::RunStore;

/// How many trailing bytes of subprocess stderr to keep as the failure message.
const STDERR_TAIL_BYTES: usize = 4096;

/// A create-run failure.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// The request failed validation (missing/empty required field, unsupported type).
    #[error("invalid run request: {0}")]
    Validation(String),
    /// A filesystem error persisting the new run.
    #[error("failed to persist run: {0}")]
    Io(#[from] std::io::Error),
}

/// Owns the run store, the spawn seam, and the worker-pool bound. Wrapped in an `Arc` and shared as
/// axum state.
pub struct RunManager {
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    index_lock: Arc<Mutex<()>>,
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
            index_lock: Arc::new(Mutex::new(())),
        }
    }

    /// The underlying store (for read handlers).
    pub fn store(&self) -> &RunStore {
        &self.store
    }

    /// Validate + create a run: write `meta.json` (`queued`), append `index.json`, and spawn a
    /// detached supervisor task. Returns the new run id.
    ///
    /// # Errors
    /// [`CreateError::Validation`] on a bad request; [`CreateError::Io`] on a persistence failure.
    pub async fn create(&self, req: CreateRunRequest) -> Result<String, CreateError> {
        validate(&req)?;
        let id = uuid::Uuid::new_v4().to_string();
        let created_ms = now_ms();
        let meta = RunMeta {
            id: id.clone(),
            run_type: req.run_type.clone(),
            status: RunStatus::Queued,
            params: req.params.clone(),
            progress: Progress::default(),
            created_ms,
            started_ms: None,
            finished_ms: None,
            exit: None,
            error: None,
            artifacts: Vec::new(),
        };
        self.store.init_run(&meta)?;

        // Append to the discovery index under the lock (serialises concurrent creates).
        {
            let _guard = self.index_lock.lock().await;
            let mut index = self.store.read_index()?;
            index.push(IndexEntry {
                id: id.clone(),
                run_type: req.run_type,
                created_ms,
                label: req.params.vintage.clone(),
            });
            self.store.write_index(&index)?;
        }

        // Detached supervisor: acquires a pool permit (blocking here keeps the run `queued`), then
        // runs + tails the subprocess.
        let store = self.store.clone();
        let spawner = Arc::clone(&self.spawner);
        let permits = Arc::clone(&self.permits);
        tokio::spawn(async move {
            supervise(store, spawner, permits, meta).await;
        });

        Ok(id)
    }
}

/// Validate a create-run request.
fn validate(req: &CreateRunRequest) -> Result<(), CreateError> {
    if req.run_type != "backtest" {
        return Err(CreateError::Validation(format!(
            "unsupported run type `{}` (only `backtest` in v1)",
            req.run_type
        )));
    }
    let p = &req.params;
    for (name, value) in [
        ("vintage", &p.vintage),
        ("start", &p.start),
        ("end", &p.end),
        ("resolution", &p.resolution),
    ] {
        if value.trim().is_empty() {
            return Err(CreateError::Validation(format!("`{name}` is required")));
        }
    }
    if p.universe.is_empty() || p.universe.iter().all(|s| s.trim().is_empty()) {
        return Err(CreateError::Validation(
            "`universe` must contain at least one instrument".to_owned(),
        ));
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

/// One subprocess progress line (mirror of the `qe-cli` `§5.3` protocol — mirrored here rather than
/// depending on `qe-cli`, which would risk a firewall edge and pull the whole training tree).
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ProgressLine {
    Progress {
        pct: u8,
        stage: String,
        msg: String,
    },
    Done {
        #[allow(dead_code)]
        result: String,
    },
    Error {
        #[allow(dead_code)]
        msg: String,
    },
}

/// Supervise one run end-to-end: acquire a pool slot, spawn the subprocess, tail stdout progress
/// into `meta.json` + `stdout.log`, capture a stderr tail, and record the terminal outcome.
async fn supervise(
    store: RunStore,
    spawner: Arc<dyn JobSpawner>,
    permits: Arc<Semaphore>,
    mut meta: RunMeta,
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

    meta.status = RunStatus::Running;
    meta.started_ms = Some(now_ms());
    let _ = store.write_meta(&meta);

    let run_dir = store.run_dir(&meta.id);
    let mut child = match spawner.spawn(&run_dir, &meta.params) {
        Ok(child) => child,
        Err(e) => {
            finish_failed(&store, &mut meta, None, format!("failed to spawn job: {e}"));
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Drain stdout (progress → meta + stdout.log) and stderr (tail) concurrently to avoid a pipe
    // deadlock if the child writes a lot to both.
    let stdout_fut = drain_stdout(stdout, &store, &mut meta);
    let stderr_fut = drain_stderr_tail(stderr);
    let (done_seen, err_tail) = tokio::join!(stdout_fut, stderr_fut);

    let exit = child.wait().await.ok().and_then(|s| s.code());
    meta.exit = exit;
    meta.finished_ms = Some(now_ms());

    if done_seen && exit == Some(0) {
        // TODO(QE-follow-up): a misbehaving job that emits `done` + exits 0 but writes no
        // `result.json` is currently classified `succeeded` (with empty `artifacts`), so
        // `GET /result` then returns 409. Consider treating a missing result artefact as `failed`.
        meta.status = RunStatus::Succeeded;
        meta.error = None;
        if store.result_path(&meta.id).exists() {
            meta.artifacts = vec!["result.json".to_owned()];
        }
        let _ = store.write_meta(&meta);
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
            Ok(ProgressLine::Done { .. }) => done_seen = true,
            Ok(ProgressLine::Error { .. }) | Err(_) => {}
        }
    }
    done_seen
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
