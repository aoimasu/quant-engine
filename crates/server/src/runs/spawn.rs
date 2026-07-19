//! Subprocess spawn seam (ADR D4c). Production spawns the `qe-cli backtest` binary; tests inject a
//! [`CliJobSpawner`] pointed at a fake job script (or a mock [`JobSpawner`]) so the lifecycle can be
//! driven hermetically — no globally-installed binary, no building `qe-cli`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

use super::model::{BacktestParams, EvolveParams, IngestParams, RunSpec, TrainParams};

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

    /// QE-460: spawn the **`train` sub-job of a composite flow** — identical to a `train` spawn but with the
    /// `--flow` marker, so the CLI seal populates QE-467's frozen-holdout lineage (holdout split + regime
    /// composition + overlap-keyed consultation count). A plain `train` run leaves the marker off and seals
    /// byte-identically. The flow supervisor calls this for the train phase and then sequences a normal
    /// [`Self::spawn`] `backtest` over the frozen holdout.
    ///
    /// # Errors
    /// Any OS error spawning the process.
    fn spawn_flow_train(&self, run_dir: &Path, params: &TrainParams) -> std::io::Result<Child>;
}

/// Production spawner: builds `<bin> <subcommand> … --run-dir <dir> --json` and spawns it with stdout +
/// stderr piped. The arg-building here is exercised verbatim by the tests (which only swap `bin`).
#[derive(Debug, Clone)]
pub struct CliJobSpawner {
    /// Path to the `qe-cli` (`qe`) binary.
    bin: PathBuf,
    /// QE-419: optional `qe-config` path pinned onto the child via `QE_CONFIG`, so the spawned CLI
    /// reads the exact same config the server loaded and boot-guarded — the storage-dir single source
    /// of truth is airtight even against CWD drift. `None` (the default, and every test) leaves the
    /// child to inherit the parent's `QE_CONFIG`/CWD unchanged.
    config_path: Option<PathBuf>,
}

impl CliJobSpawner {
    /// Spawner that runs the binary at `bin`, with no config pin (the child inherits the environment).
    pub fn new(bin: PathBuf) -> Self {
        Self {
            bin,
            config_path: None,
        }
    }

    /// QE-419: pin the child's `qe-config` to `config_path` (sets `QE_CONFIG` on the spawned process),
    /// so the CLI resolves the same `[storage]` dirs the server guarded at boot.
    #[must_use]
    pub fn with_config_path(mut self, config_path: PathBuf) -> Self {
        self.config_path = Some(config_path);
        self
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
            RunSpec::Train(params) => train_args(&mut cmd, params, run_dir, false),
            RunSpec::Evolve(params) => evolve_args(&mut cmd, params, run_dir),
            RunSpec::Ingest(params) => ingest_args(&mut cmd, params, run_dir),
            // A composite flow is never spawned as a single process — the supervisor sequences its `train`
            // (via `spawn_flow_train`) and `backtest` sub-jobs. A direct spawn is a programming error.
            RunSpec::Flow(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a composite `flow` run is sequenced by the supervisor, not spawned as one process",
                ));
            }
        }
        self.finish_spawn(cmd)
    }

    fn spawn_flow_train(&self, run_dir: &Path, params: &TrainParams) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.bin);
        // The `--flow` marker makes the CLI seal record the frozen-holdout lineage (QE-460); the argv is
        // otherwise identical to a plain `train` spawn.
        train_args(&mut cmd, params, run_dir, true);
        self.finish_spawn(cmd)
    }
}

impl CliJobSpawner {
    /// Apply the shared child wiring (QE-419 config pin + piped stdio + `kill_on_drop`) and spawn.
    fn finish_spawn(&self, mut cmd: Command) -> std::io::Result<Child> {
        // QE-419: pin the child to the server's config file so it reads the same `[storage]` dirs.
        if let Some(config_path) = &self.config_path {
            cmd.env(crate::config::ENV_CONFIG, config_path);
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
fn train_args(cmd: &mut Command, params: &TrainParams, run_dir: &Path, flow: bool) {
    cmd.arg("train");
    // QE-460: the composite flow's train sub-job carries `--flow` so the seal records the frozen-holdout
    // lineage (holdout split + regime composition + overlap-keyed consultation count). A plain train run
    // passes `false` and its argv + sealed vintage are byte-identical to pre-QE-460.
    if flow {
        cmd.arg("--flow");
    }
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
    // QE-458 steer knobs: only emitted when set (un-steered runs pass none ⇒ the CLI seals byte-identically).
    // `evolved_pool`/`evolved_formulas` are intentionally NOT forwarded — `validate_train` rejects them as
    // not-yet-supported on the live train search, so a steered request never reaches the CLI as a silent no-op.
    for id in params.indicator_subset.iter().flatten() {
        cmd.arg("--indicator").arg(id);
    }
    if let Some(windows) = params.windows {
        cmd.arg("--windows").arg(windows.to_string());
    }
    if let Some(folds) = params.folds {
        cmd.arg("--folds").arg(folds.to_string());
    }
    cmd.arg("--run-dir").arg(run_dir).arg("--json");
}

/// Build the `evolve … --run-dir <dir> --json` argv (QE-452). Reuses the same shape as `train_args`
/// (config/profile from the pinned config; the window + seed; optional budget/cap knobs only when set)
/// plus the campaign `--mode`. The QE-419 config pin + `kill_on_drop` are applied by the caller exactly
/// as for train/backtest.
fn evolve_args(cmd: &mut Command, params: &EvolveParams, run_dir: &Path) {
    cmd.arg("evolve");
    if let Some(config) = &params.config {
        cmd.arg("--config").arg(config);
    }
    if let Some(profile) = &params.profile {
        cmd.arg("--profile").arg(profile);
    }
    cmd.arg("--mode")
        .arg(params.mode.as_str())
        .arg("--start")
        .arg(&params.start)
        .arg("--end")
        .arg(&params.end)
        .arg("--resolution")
        .arg(&params.resolution)
        .arg("--seed")
        .arg(params.seed.to_string());
    if let Some(generations) = params.generations {
        cmd.arg("--generations").arg(generations.to_string());
    }
    if let Some(offspring) = params.offspring {
        cmd.arg("--offspring").arg(offspring.to_string());
    }
    if let Some(states) = params.states {
        cmd.arg("--states").arg(states.to_string());
    }
    if let Some(k) = params.k {
        cmd.arg("--k").arg(k.to_string());
    }
    cmd.arg("--run-dir").arg(run_dir).arg("--json");
}

/// Build the `ingest … --run-dir <dir> --json` argv (QE-464). The store path + universe come from the
/// config file (`--config`, pinned via `QE_CONFIG`), not flags: `--instrument` (repeated) names explicit
/// symbols, `--fetch-all` resolves the whole point-in-time universe, and `--synthetic` selects the
/// offline generator. The window is `--start`/`--end`/`--resolution`.
fn ingest_args(cmd: &mut Command, params: &IngestParams, run_dir: &Path) {
    cmd.arg("ingest")
        .arg("--start")
        .arg(&params.start)
        .arg("--end")
        .arg(&params.end)
        .arg("--resolution")
        .arg(&params.resolution);
    for symbol in &params.instruments {
        cmd.arg("--instrument").arg(symbol);
    }
    if params.fetch_all {
        cmd.arg("--fetch-all");
    }
    if params.synthetic {
        cmd.arg("--synthetic");
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
