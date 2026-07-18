//! File-based run store (ADR D4b): `<runs_dir>/{index.json, <id>/{meta.json, result.json, stdout.log}}`.
//!
//! `meta.json` is the single source of truth for a run's status/progress; `index.json` holds only
//! immutable per-run discovery fields for ordering. All JSON writes are **atomic** (temp file in the
//! same directory + `rename`) so a concurrent reader never observes a partial file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::model::{IndexEntry, RunMeta};

/// The run store rooted at the configured `<data_dir>/runs` directory. Cheap to clone (holds a
/// `PathBuf`), so a supervisor task can own a copy.
#[derive(Debug, Clone)]
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    /// Create a store rooted at `runs_dir` (created lazily on first write).
    pub fn new(runs_dir: PathBuf) -> Self {
        Self { root: runs_dir }
    }

    /// The directory for a run.
    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Path to a run's `result.json`.
    pub fn result_path(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("result.json")
    }

    /// Path to a run's captured `stdout.log`.
    pub fn stdout_path(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("stdout.log")
    }

    /// Path to a run's `meta.json`.
    fn meta_path(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("meta.json")
    }

    /// Path to the top-level `index.json`.
    fn index_path(&self) -> PathBuf {
        self.root.join("index.json")
    }

    /// Create the run directory (+ an empty `stdout.log`) and write the initial `meta.json`.
    ///
    /// # Errors
    /// Any filesystem error creating the directory or writing the files.
    pub fn init_run(&self, meta: &RunMeta) -> io::Result<()> {
        let dir = self.run_dir(&meta.id);
        fs::create_dir_all(&dir)?;
        // Touch stdout.log so it exists even if the child produces nothing before failing.
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stdout_path(&meta.id))?;
        self.write_meta(meta)
    }

    /// Atomically (over)write a run's `meta.json`.
    ///
    /// # Errors
    /// Any serialisation or filesystem error.
    pub fn write_meta(&self, meta: &RunMeta) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(meta).map_err(io::Error::other)?;
        atomic_write(&self.meta_path(&meta.id), &bytes)
    }

    /// Read a run's `meta.json`. Returns `Ok(None)` when the run does not exist.
    ///
    /// # Errors
    /// A filesystem error other than "not found", or a JSON parse error.
    pub fn read_meta(&self, id: &str) -> io::Result<Option<RunMeta>> {
        match fs::read(self.meta_path(id)) {
            Ok(bytes) => {
                let meta = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read a run's `result.json` bytes.
    ///
    /// A thin wrapper over `fs::read(result_path)` so the run-store's blocking filesystem primitives all
    /// live here (QE-411): the read handler runs this inside `spawn_blocking` and treats any error as
    /// "result artefact missing", exactly as the previous inline `std::fs::read` did.
    ///
    /// # Errors
    /// Any filesystem error reading the artefact (including "not found").
    pub fn read_result(&self, id: &str) -> io::Result<Vec<u8>> {
        fs::read(self.result_path(id))
    }

    /// Read `index.json`. A missing index is an empty list.
    ///
    /// # Errors
    /// A filesystem error other than "not found", or a JSON parse error.
    pub fn read_index(&self) -> io::Result<Vec<IndexEntry>> {
        match fs::read(self.index_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Atomically (over)write `index.json`.
    ///
    /// # Errors
    /// Any serialisation or filesystem error.
    pub fn write_index(&self, entries: &[IndexEntry]) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec_pretty(entries).map_err(io::Error::other)?;
        atomic_write(&self.index_path(), &bytes)
    }
}

/// Write `bytes` to `path` atomically: write a sibling temp file, then `rename` over `path`. The
/// temp file shares `path`'s parent so the rename is a same-filesystem move.
///
/// `pub(crate)` so the QE-454 tamper-evident audit log (`crate::audit`) reuses the **same** atomic-write
/// discipline as the run store rather than forking a second implementation.
///
/// # Errors
/// Any filesystem error creating the parent, writing the temp file, or renaming.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    // Best-effort cleanup of the temp file on any failure after creation.
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::model::{BacktestParams, Progress, RunStatus};

    fn sample_meta(id: &str) -> RunMeta {
        RunMeta {
            id: id.to_owned(),
            run_type: "backtest".to_owned(),
            status: RunStatus::Queued,
            params: serde_json::to_value(BacktestParams {
                vintage: "v".to_owned(),
                strategy: None,
                start: "2021-01-01".to_owned(),
                end: "2021-02-01".to_owned(),
                resolution: "1h".to_owned(),
                universe: vec!["BTCUSDT".to_owned()],
                taker_fee_bps: 2.0,
                slippage_model: "square-root-impact".to_owned(),
            })
            .expect("serialize backtest params"),
            progress: Progress::default(),
            train: None,
            created_ms: 123,
            started_ms: None,
            finished_ms: None,
            exit: None,
            error: None,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn meta_round_trips_and_unknown_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        let meta = sample_meta("abc");
        store.init_run(&meta).unwrap();
        assert_eq!(store.read_meta("abc").unwrap().as_ref(), Some(&meta));
        assert!(store.read_meta("nope").unwrap().is_none());
        // stdout.log was touched.
        assert!(store.stdout_path("abc").exists());
    }

    #[test]
    fn read_result_round_trips_and_absent_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        let meta = sample_meta("res");
        store.init_run(&meta).unwrap();
        // No result artefact yet ⇒ an error (the read handler maps this to a 409).
        assert!(store.read_result("res").is_err());
        // Once written, the exact bytes round-trip.
        fs::write(store.result_path("res"), b"{\"ok\":true}").unwrap();
        assert_eq!(store.read_result("res").unwrap(), b"{\"ok\":true}");
    }

    #[test]
    fn index_round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        assert!(store.read_index().unwrap().is_empty());
        let entries = vec![IndexEntry {
            id: "abc".to_owned(),
            run_type: "backtest".to_owned(),
            created_ms: 1,
            label: "v".to_owned(),
        }];
        store.write_index(&entries).unwrap();
        assert_eq!(store.read_index().unwrap(), entries);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.json");
        atomic_write(&target, b"one").unwrap();
        atomic_write(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        // No leftover `.<uuid>.tmp` files.
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
