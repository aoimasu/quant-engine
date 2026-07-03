//! quant-engine CLI library — the composition root for the runnable pipelines.
//!
//! QE-260 wires the **real training-search job**: load config (QE-002), resolve the point-in-time
//! universe (QE-012), ensure the configurable state directories exist, build a real
//! [`qe_determinism::Lineage`] (QE-006), and run the MAP-Elites search → ensemble → validation → G1 gate
//! → **seal a vintage** pipeline ([`jobs::train`]). This module keeps the config/universe/lineage/dir
//! responsibilities and delegates the deterministic pipeline to the job (mirroring how the backtest
//! command builds `BacktestParams`).
//!
//! The logic lives here (not in `main.rs`) so it is unit- and integration-testable.

use std::path::{Path, PathBuf};

pub mod jobs;

use qe_config::Config;
use qe_determinism::Lineage;
use thiserror::Error;

use jobs::train::{run_train_job, TrainParams};
pub use jobs::train::{TrainOutcome, TrainResultDoc};
use jobs::{ProgressLine, RunError};

/// Errors from a CLI run.
#[derive(Debug, Error)]
pub enum CliError {
    /// Bad command-line usage (unknown flag, missing value, unknown command).
    #[error("usage error: {0}")]
    Usage(String),

    /// Config load/validation failure.
    #[error(transparent)]
    Config(#[from] qe_config::ConfigError),

    /// The config carried no instruments to train over.
    #[error("config has no instruments to train over")]
    EmptyUniverse,

    /// Filesystem error creating a state directory.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },

    /// A training-job runtime failure (surfaced as the terminal `{"t":"error"}` line + non-zero exit).
    #[error(transparent)]
    Run(#[from] RunError),
}

/// The tunable inputs to a training run, parsed from the `train` command flags. The window + budget the
/// [`run_train`] job needs on top of the config-derived store/universe/lineage.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainOptions {
    /// Inclusive window start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive window end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution (`1h`, …).
    pub resolution: String,
    /// Master search seed override; `None` uses the config seed (`determinism.seed`).
    pub seed: Option<u64>,
    /// MAP-Elites search generations (small-budget default).
    pub generations: usize,
    /// Variation steps per direction per generation.
    pub population: usize,
    /// Number of final bars reserved as the untouched G1 holdout.
    pub holdout: usize,
    /// Embargo bars purged between the train window and the holdout.
    pub embargo: usize,
}

/// Run the training-search pipeline for `cfg`, sealing a vintage under the configured artefacts
/// directory and streaming structured [`ProgressLine`]s through `emit`. `code_commit` is the build's
/// code provenance (folded into the lineage / vintage id), passed in so the result is deterministic and
/// testable.
///
/// Keeps the config/universe/lineage/dir responsibilities of the old QE-013 skeleton, then delegates the
/// real search → ensemble → validation → G1 → seal pipeline to [`jobs::train::run_train_job`].
///
/// # Errors
/// [`CliError`] on config/universe validation, an empty instrument list, directory creation, or a
/// training-job runtime failure ([`RunError`]).
pub fn run_train(
    cfg: &Config,
    opts: &TrainOptions,
    code_commit: &str,
    emit: &mut dyn FnMut(ProgressLine),
) -> Result<TrainOutcome, CliError> {
    // 1. Resolve + validate the point-in-time universe (listing/delisting windows). v1 trains over the
    //    first configured instrument (single-instrument, mirroring the QE-251 backtest job).
    let _universe = cfg.universe()?;
    let instrument = cfg
        .instruments
        .first()
        .cloned()
        .ok_or(CliError::EmptyUniverse)?;

    // 2. Ensure every *configurable* persistent-state directory exists. All paths come from config;
    //    none are absolute or hard-coded here.
    for dir in [
        &cfg.storage.market_dir,
        &cfg.storage.synthetic_dir,
        &cfg.storage.artifacts_dir,
    ] {
        create_dir(dir)?;
    }

    // 3. Build the vintage lineage from real inputs (config hash + seed + commit). No input snapshot yet
    //    (the ingest stages are P1), so the snapshot id is empty. The seed is the search master seed, so
    //    the sealed vintage id (= lineage id) is deterministic for a fixed seed.
    let seed = opts.seed.unwrap_or(cfg.determinism.seed);
    let lineage = Lineage::from_config(cfg, "", code_commit, vec![seed])?;

    // 4. Build the job params from config + options and run the pipeline.
    let params = TrainParams {
        store_path: PathBuf::from(&cfg.storage.market_dir),
        map_size: qe_storage::DEFAULT_MAP_SIZE,
        vintage_root: PathBuf::from(&cfg.storage.artifacts_dir).join("vintages"),
        instrument,
        start: opts.start.clone(),
        end: opts.end.clone(),
        resolution: opts.resolution.clone(),
        seed,
        generations: opts.generations,
        population: opts.population,
        holdout: opts.holdout,
        embargo: opts.embargo,
        lineage,
        profile: cfg.profile.as_str().to_owned(),
    };

    Ok(run_train_job(&params, emit)?)
}

fn create_dir(path: impl AsRef<Path>) -> Result<(), CliError> {
    let path = path.as_ref();
    std::fs::create_dir_all(path).map_err(|source| CliError::Io {
        path: path.display().to_string(),
        source,
    })
}

// ---- command-line parsing ------------------------------------------------------------------------

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Print the version and exit (the bare invocation).
    Version,
    /// Run the training-search pipeline (QE-260): MAP-Elites search → ensemble → validation → G1 gate →
    /// seal a vintage.
    Train {
        /// Config file path.
        config: PathBuf,
        /// Operating profile.
        profile: qe_config::Profile,
        /// Run directory the job writes `result.json` into.
        run_dir: PathBuf,
        /// Emit JSON-line progress on stdout.
        json: bool,
        /// Inclusive training window start (`YYYY-MM-DD`).
        start: String,
        /// Exclusive training window end (`YYYY-MM-DD`).
        end: String,
        /// Bar resolution (`1h`, …).
        resolution: String,
        /// Master search seed override (`None` ⇒ the config seed).
        seed: Option<u64>,
        /// MAP-Elites search generations.
        generations: usize,
        /// Variation steps per direction per generation.
        population: usize,
        /// Final bars reserved as the untouched G1 holdout.
        holdout: usize,
        /// Embargo bars purged between the train window and the holdout.
        embargo: usize,
    },
    /// Backtest a sealed vintage over a window (QE-251).
    Backtest {
        /// Vintage id to load from the repository.
        vintage: String,
        /// Optional single-chromosome selector (unset ⇒ the whole ensemble).
        strategy: Option<String>,
        /// Inclusive window start (`YYYY-MM-DD`).
        start: String,
        /// Exclusive window end (`YYYY-MM-DD`).
        end: String,
        /// Bar resolution (`1h`, `5m`, …).
        resolution: String,
        /// Instrument symbols to backtest (v1 uses the first).
        universe: Vec<String>,
        /// Taker fee, in basis points of notional.
        taker_fee_bps: f64,
        /// Slippage-model label (recorded in the result contract).
        slippage_model: String,
        /// Run directory the job writes `result.json` into.
        run_dir: PathBuf,
        /// Emit JSON-line progress on stdout.
        json: bool,
    },
    /// Ingest market data into the store from the configured source (QE-253).
    ///
    /// Real network decoders live behind the default-off `http` feature; the window is bounded by
    /// `--start`/`--end` at `--resolution`, and the store path + universe come from `--config`.
    Ingest {
        /// Config file path (supplies the store path + universe).
        config: PathBuf,
        /// Inclusive window start (`YYYY-MM-DD`).
        start: String,
        /// Exclusive window end (`YYYY-MM-DD`).
        end: String,
        /// Bar resolution to ingest (`1h`, `5m`, …).
        resolution: String,
    },
}

/// Parse CLI arguments (excluding `argv[0]`).
///
/// `qe` → [`Command::Version`]; `qe train [--config <p>] [--profile <p>]` → [`Command::Train`].
///
/// # Errors
/// [`CliError::Usage`] on an unknown command/flag, a missing flag value, or an unknown profile.
pub fn parse_args<I, S>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut it = args.into_iter().map(|s| s.as_ref().to_owned());
    let Some(cmd) = it.next() else {
        return Ok(Command::Version);
    };
    match cmd.as_str() {
        "version" | "--version" | "-V" => Ok(Command::Version),
        "train" => {
            let mut config = PathBuf::from("config.toml");
            let mut profile = qe_config::Profile::Train;
            let mut run_dir = PathBuf::new();
            let mut json = false;
            let mut start = String::new();
            let mut end = String::new();
            let mut resolution = "1h".to_owned();
            let mut seed: Option<u64> = None;
            let mut generations = DEFAULT_TRAIN_GENERATIONS;
            let mut population = DEFAULT_TRAIN_POPULATION;
            let mut holdout = DEFAULT_TRAIN_HOLDOUT;
            let mut embargo = DEFAULT_TRAIN_EMBARGO;
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--config" => config = PathBuf::from(value(&mut it, "--config")?),
                    "--profile" => profile = parse_profile(&value(&mut it, "--profile")?)?,
                    "--run-dir" => run_dir = PathBuf::from(value(&mut it, "--run-dir")?),
                    "--json" => json = true,
                    "--start" => start = value(&mut it, "--start")?,
                    "--end" => end = value(&mut it, "--end")?,
                    "--resolution" => resolution = value(&mut it, "--resolution")?,
                    "--seed" => seed = Some(parse_usize_flag(&mut it, "--seed")? as u64),
                    "--generations" => generations = parse_usize_flag(&mut it, "--generations")?,
                    "--population" => population = parse_usize_flag(&mut it, "--population")?,
                    "--holdout" => holdout = parse_usize_flag(&mut it, "--holdout")?,
                    "--embargo" => embargo = parse_usize_flag(&mut it, "--embargo")?,
                    other => {
                        return Err(CliError::Usage(format!("unknown flag `{other}`")));
                    }
                }
            }
            Ok(Command::Train {
                config,
                profile,
                run_dir,
                json,
                start,
                end,
                resolution,
                seed,
                generations,
                population,
                holdout,
                embargo,
            })
        }
        "backtest" => {
            let mut vintage: Option<String> = None;
            let mut strategy: Option<String> = None;
            let mut start = String::new();
            let mut end = String::new();
            let mut resolution = String::new();
            let mut universe: Vec<String> = Vec::new();
            let mut taker_fee_bps = 2.0_f64;
            let mut slippage_model = "square-root-impact".to_owned();
            let mut run_dir = PathBuf::new();
            let mut json = false;
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--vintage" => vintage = Some(value(&mut it, "--vintage")?),
                    "--strategy" => strategy = Some(value(&mut it, "--strategy")?),
                    "--start" => start = value(&mut it, "--start")?,
                    "--end" => end = value(&mut it, "--end")?,
                    "--resolution" => resolution = value(&mut it, "--resolution")?,
                    "--universe" => {
                        // Comma- or repeat-separated; accept both for ergonomics.
                        let raw = value(&mut it, "--universe")?;
                        universe.extend(
                            raw.split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned),
                        );
                    }
                    "--taker-fee-bps" => {
                        let v = value(&mut it, "--taker-fee-bps")?;
                        taker_fee_bps = v.parse().map_err(|_| {
                            CliError::Usage(format!("--taker-fee-bps expects a number, got `{v}`"))
                        })?;
                    }
                    "--slippage-model" => slippage_model = value(&mut it, "--slippage-model")?,
                    "--run-dir" => run_dir = PathBuf::from(value(&mut it, "--run-dir")?),
                    "--json" => json = true,
                    other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
                }
            }
            let vintage =
                vintage.ok_or_else(|| CliError::Usage("--vintage is required".to_owned()))?;
            Ok(Command::Backtest {
                vintage,
                strategy,
                start,
                end,
                resolution,
                universe,
                taker_fee_bps,
                slippage_model,
                run_dir,
                json,
            })
        }
        "ingest" => {
            let mut config = PathBuf::from("config.toml");
            let mut start = String::new();
            let mut end = String::new();
            let mut resolution = String::new();
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--config" => config = PathBuf::from(value(&mut it, "--config")?),
                    "--start" => start = value(&mut it, "--start")?,
                    "--end" => end = value(&mut it, "--end")?,
                    "--resolution" => resolution = value(&mut it, "--resolution")?,
                    other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
                }
            }
            Ok(Command::Ingest {
                config,
                start,
                end,
                resolution,
            })
        }
        other => Err(CliError::Usage(format!("unknown command `{other}`"))),
    }
}

/// Default MAP-Elites search generations for `train` (small budget — a fixture run is sub-second).
pub const DEFAULT_TRAIN_GENERATIONS: usize = 8;
/// Default variation steps per direction per generation for `train`.
pub const DEFAULT_TRAIN_POPULATION: usize = 24;
/// Default number of final bars reserved as the untouched G1 holdout for `train`. A backtest over `N`
/// bars yields `N − 1` returns, so 31 holdout bars give 30 holdout **returns** — meeting G1's default
/// `min_holdout_samples = 30`, so the holdout-samples criterion is satisfiable at the default budget
/// (30 holdout bars would give only 29 returns and could never pass it).
pub const DEFAULT_TRAIN_HOLDOUT: usize = 31;
/// Default embargo bars purged between the train window and the holdout for `train`.
pub const DEFAULT_TRAIN_EMBARGO: usize = 2;

/// Pull the value that must follow a flag, or a `Usage` error naming the flag.
fn value<I>(it: &mut I, flag: &str) -> Result<String, CliError>
where
    I: Iterator<Item = String>,
{
    it.next()
        .ok_or_else(|| CliError::Usage(format!("{flag} needs a value")))
}

/// Pull and parse a non-negative integer flag value (`--generations`, `--seed`, …).
fn parse_usize_flag<I>(it: &mut I, flag: &str) -> Result<usize, CliError>
where
    I: Iterator<Item = String>,
{
    let v = value(it, flag)?;
    v.parse()
        .map_err(|_| CliError::Usage(format!("{flag} expects a non-negative integer, got `{v}`")))
}

fn parse_profile(s: &str) -> Result<qe_config::Profile, CliError> {
    match s {
        "train" => Ok(qe_config::Profile::Train),
        "runtime-sim" => Ok(qe_config::Profile::RuntimeSim),
        "runtime-live" => Ok(qe_config::Profile::RuntimeLive),
        other => Err(CliError::Usage(format!(
            "unknown profile `{other}` (train|runtime-sim|runtime-live)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_invocation_is_version() {
        assert_eq!(parse_args(Vec::<String>::new()).unwrap(), Command::Version);
        assert_eq!(parse_args(["--version"]).unwrap(), Command::Version);
    }

    #[test]
    fn train_parses_flags_and_defaults() {
        // Bare `train`: config + budget defaults, empty window (supplied at runtime), `1h` resolution.
        let cmd = parse_args(["train"]).unwrap();
        assert_eq!(
            cmd,
            Command::Train {
                config: PathBuf::from("config.toml"),
                profile: qe_config::Profile::Train,
                run_dir: PathBuf::new(),
                json: false,
                start: String::new(),
                end: String::new(),
                resolution: "1h".to_owned(),
                seed: None,
                generations: DEFAULT_TRAIN_GENERATIONS,
                population: DEFAULT_TRAIN_POPULATION,
                holdout: DEFAULT_TRAIN_HOLDOUT,
                embargo: DEFAULT_TRAIN_EMBARGO,
            }
        );
        // Every flag overridden.
        let cmd = parse_args([
            "train",
            "--config",
            "my.toml",
            "--profile",
            "runtime-sim",
            "--run-dir",
            "/tmp/r",
            "--json",
            "--start",
            "2021-01-01",
            "--end",
            "2021-01-10",
            "--resolution",
            "5m",
            "--seed",
            "7",
            "--generations",
            "3",
            "--population",
            "10",
            "--holdout",
            "12",
            "--embargo",
            "1",
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Command::Train {
                config: PathBuf::from("my.toml"),
                profile: qe_config::Profile::RuntimeSim,
                run_dir: PathBuf::from("/tmp/r"),
                json: true,
                start: "2021-01-01".to_owned(),
                end: "2021-01-10".to_owned(),
                resolution: "5m".to_owned(),
                seed: Some(7),
                generations: 3,
                population: 10,
                holdout: 12,
                embargo: 1,
            }
        );
    }

    #[test]
    fn rejects_unknown_flag_command_and_profile() {
        assert!(matches!(
            parse_args(["train", "--nope"]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            parse_args(["frobnicate"]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            parse_args(["train", "--profile", "bogus"]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            parse_args(["train", "--config"]),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn backtest_parses_required_and_optional_flags() {
        let cmd = parse_args([
            "backtest",
            "--vintage",
            "v-2026-07",
            "--start",
            "2021-01-01",
            "--end",
            "2024-12-31",
            "--resolution",
            "1h",
            "--run-dir",
            "/tmp/r",
            "--json",
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Command::Backtest {
                vintage: "v-2026-07".into(),
                strategy: None,
                start: "2021-01-01".into(),
                end: "2024-12-31".into(),
                resolution: "1h".into(),
                universe: vec![],
                taker_fee_bps: 2.0,
                slippage_model: "square-root-impact".into(),
                run_dir: PathBuf::from("/tmp/r"),
                json: true,
            }
        );
    }

    #[test]
    fn backtest_overrides_costs_universe_and_strategy() {
        let cmd = parse_args([
            "backtest",
            "--vintage",
            "v1",
            "--strategy",
            "#3",
            "--start",
            "2021-01-01",
            "--end",
            "2021-02-01",
            "--resolution",
            "5m",
            "--universe",
            "BTCUSDT,ETHUSDT",
            "--taker-fee-bps",
            "5",
            "--slippage-model",
            "linear",
            "--run-dir",
            "/tmp/r",
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Command::Backtest {
                vintage: "v1".into(),
                strategy: Some("#3".into()),
                start: "2021-01-01".into(),
                end: "2021-02-01".into(),
                resolution: "5m".into(),
                universe: vec!["BTCUSDT".into(), "ETHUSDT".into()],
                taker_fee_bps: 5.0,
                slippage_model: "linear".into(),
                run_dir: PathBuf::from("/tmp/r"),
                json: false,
            }
        );
    }

    #[test]
    fn backtest_requires_vintage() {
        assert!(matches!(
            parse_args(["backtest", "--start", "2021-01-01"]),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn ingest_parses_flags_and_defaults() {
        let cmd = parse_args([
            "ingest",
            "--config",
            "my.toml",
            "--start",
            "2021-01-01",
            "--end",
            "2021-02-01",
            "--resolution",
            "1h",
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Command::Ingest {
                config: PathBuf::from("my.toml"),
                start: "2021-01-01".into(),
                end: "2021-02-01".into(),
                resolution: "1h".into(),
            }
        );
        // Bare `ingest` defaults the config path and leaves the window empty.
        assert_eq!(
            parse_args(["ingest"]).unwrap(),
            Command::Ingest {
                config: PathBuf::from("config.toml"),
                start: String::new(),
                end: String::new(),
                resolution: String::new(),
            }
        );
    }

    #[test]
    fn ingest_rejects_unknown_flag() {
        assert!(matches!(
            parse_args(["ingest", "--nope"]),
            Err(CliError::Usage(_))
        ));
    }
}
