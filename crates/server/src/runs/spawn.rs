//! Subprocess spawn seam (ADR D4c). Production spawns the `qe-cli backtest` binary; tests inject a
//! [`CliJobSpawner`] pointed at a fake job script (or a mock [`JobSpawner`]) so the lifecycle can be
//! driven hermetically — no globally-installed binary, no building `qe-cli`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

use super::model::{BacktestParams, RunSpec, TrainParams};

/// Environment variable naming the `qe-cli` binary to spawn.
pub const ENV_CLI_BIN: &str = "QE_SERVER_CLI_BIN";

/// Spawns a run subprocess. The seam that lets tests substitute a controllable fake job while
/// production runs the real `qe-cli backtest` / `qe-cli train`.
pub trait JobSpawner: Send + Sync + 'static {
    /// Spawn the job for `spec`, instructing it to write artefacts into `run_dir`. The returned
    /// [`Child`] must have its `stdout` and `stderr` piped so the supervisor can tail progress and
    /// capture an error tail.
    ///
    /// # Errors
    /// Any OS error spawning the process (e.g. the binary does not exist).
    fn spawn(&self, run_dir: &Path, spec: &RunSpec) -> std::io::Result<Child>;
}

/// Production spawner: builds `<bin> <subcommand> … --run-dir <dir> --json` and spawns it with stdout +
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
    fn spawn(&self, run_dir: &Path, spec: &RunSpec) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.bin);
        match spec {
            RunSpec::Backtest(params) => backtest_args(&mut cmd, params, run_dir),
            RunSpec::Train(params) => train_args(&mut cmd, params, run_dir),
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Reap the child if the supervising task is dropped (e.g. server shutdown) so we never
            // leak a runaway job.
            .kill_on_drop(true);
        cmd.spawn()
    }
}

/// Build the `backtest … --run-dir <dir> --json` argv (QE-255, unchanged).
fn backtest_args(cmd: &mut Command, params: &BacktestParams, run_dir: &Path) {
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
}

/// Build the `train … --run-dir <dir> --json` argv (QE-260/QE-261). The instrument/universe + store +
/// artefacts roots come from the config file (`--config`, defaulted by the CLI), not flags; the budget
/// knobs are only passed when the request set them, so the CLI's own defaults otherwise apply.
fn train_args(cmd: &mut Command, params: &TrainParams, run_dir: &Path) {
    cmd.arg("train");
    if let Some(config) = &params.config {
        cmd.arg("--config").arg(config);
    }
    if let Some(profile) = &params.profile {
        cmd.arg("--profile").arg(profile);
    }
    cmd.arg("--start")
        .arg(&params.start)
        .arg("--end")
        .arg(&params.end)
        .arg("--resolution")
        .arg(&params.resolution);
    if let Some(seed) = params.seed {
        cmd.arg("--seed").arg(seed.to_string());
    }
    if let Some(generations) = params.generations {
        cmd.arg("--generations").arg(generations.to_string());
    }
    if let Some(population) = params.population {
        cmd.arg("--population").arg(population.to_string());
    }
    if let Some(holdout) = params.holdout {
        cmd.arg("--holdout").arg(holdout.to_string());
    }
    if let Some(embargo) = params.embargo {
        cmd.arg("--embargo").arg(embargo.to_string());
    }
    cmd.arg("--run-dir").arg(run_dir).arg("--json");
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
