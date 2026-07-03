//! Subprocess spawn seam (ADR D4c). Production spawns the `qe-cli backtest` binary; tests inject a
//! [`CliJobSpawner`] pointed at a fake job script (or a mock [`JobSpawner`]) so the lifecycle can be
//! driven hermetically — no globally-installed binary, no building `qe-cli`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

use super::model::BacktestParams;

/// Environment variable naming the `qe-cli` binary to spawn.
pub const ENV_CLI_BIN: &str = "QE_SERVER_CLI_BIN";

/// Spawns a backtest subprocess. The seam that lets tests substitute a controllable fake job while
/// production runs the real `qe-cli backtest`.
pub trait JobSpawner: Send + Sync + 'static {
    /// Spawn the job for `params`, instructing it to write artefacts into `run_dir`. The returned
    /// [`Child`] must have its `stdout` and `stderr` piped so the supervisor can tail progress and
    /// capture an error tail.
    ///
    /// # Errors
    /// Any OS error spawning the process (e.g. the binary does not exist).
    fn spawn(&self, run_dir: &Path, params: &BacktestParams) -> std::io::Result<Child>;
}

/// Production spawner: builds `<bin> backtest … --run-dir <dir> --json` and spawns it with stdout +
/// stderr piped. The arg-building here is exercised verbatim by the tests (which only swap `bin`).
#[derive(Debug, Clone)]
pub struct CliJobSpawner {
    /// Path to the `qe-cli` (`qe`) binary.
    bin: PathBuf,
}

impl CliJobSpawner {
    /// Spawner that runs the binary at `bin`.
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// The configured binary path.
    pub fn bin(&self) -> &Path {
        &self.bin
    }
}

impl JobSpawner for CliJobSpawner {
    fn spawn(&self, run_dir: &Path, params: &BacktestParams) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("backtest").arg("--vintage").arg(&params.vintage);
        if let Some(strategy) = &params.strategy {
            cmd.arg("--strategy").arg(strategy);
        }
        cmd.arg("--start")
            .arg(&params.start)
            .arg("--end")
            .arg(&params.end)
            .arg("--resolution")
            .arg(&params.resolution);
        // `--universe` is repeat-separated (the CLI accepts either repeats or a comma list).
        for symbol in &params.universe {
            cmd.arg("--universe").arg(symbol);
        }
        cmd.arg("--taker-fee-bps")
            .arg(params.taker_fee_bps.to_string())
            .arg("--slippage-model")
            .arg(&params.slippage_model)
            .arg("--run-dir")
            .arg(run_dir)
            .arg("--json");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Reap the child if the supervising task is dropped (e.g. server shutdown) so we never
            // leak a runaway backtest.
            .kill_on_drop(true);
        cmd.spawn()
    }
}

/// Resolve the `qe-cli` binary path: `QE_SERVER_CLI_BIN` if set, else a `qe` binary co-located with
/// the running `qe-server` executable (the co-located-deploy convention), else the bare name `qe`
/// (resolved via `PATH`). No globally-installed binary is *assumed* — a real deploy sets the env var.
pub fn resolve_cli_bin() -> PathBuf {
    if let Ok(path) = std::env::var(ENV_CLI_BIN) {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("qe");
        }
    }
    PathBuf::from("qe")
}
