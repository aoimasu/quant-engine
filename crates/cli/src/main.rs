//! quant-engine CLI entry point.
//!
//! Thin dispatcher over [`qe_cli`]: parse args, run the command, print a result or a usage error.
//! All logic lives in the library so it stays testable (QE-013).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qe_cli::jobs::backtest::{run_backtest, BacktestParams};
use qe_cli::jobs::datetime::parse_ymd_to_millis;
use qe_cli::jobs::ingest::{run_ingest, IngestParams, SyntheticSource};
use qe_cli::jobs::{
    emit_done, emit_error, emit_evolve_done, emit_ingest_done, emit_progress, emit_train_done,
    ProgressLine,
};
use qe_cli::{parse_args, run_evolve, run_train, Command, EvolveOptions, TrainOptions};
use qe_config::{Config, Profile};
use qe_domain::Resolution;
use qe_run_protocol::EvolveMode;
use qe_telemetry::{init as init_telemetry, OutputStream, TelemetryConfig, TelemetryGuard};

/// Code provenance folded into the vintage id (QE-420). Resolution precedence:
///
/// 1. `QE_CODE_COMMIT` runtime override, when set and non-empty (lets a build pipeline or container
///    stamp an explicit commit — see the `Dockerfile` ARG);
/// 2. `QE_BUILD_GIT_SHA` — the real git short SHA captured at compile time by `build.rs`
///    (`<sha>` or `<sha>-dirty`);
/// 3. the crate version as a last-resort sentinel, when the build had no git available
///    (`QE_BUILD_GIT_SHA` is empty or `"unknown"`).
///
/// So two binaries built from different commits carry different `code_commit`s with no env var set,
/// while the explicit override keeps working unchanged.
fn code_commit() -> String {
    if let Ok(explicit) = std::env::var("QE_CODE_COMMIT") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    let build_sha = env!("QE_BUILD_GIT_SHA");
    if build_sha.is_empty() || build_sha == "unknown" {
        return env!("CARGO_PKG_VERSION").to_owned();
    }
    build_sha.to_owned()
}

fn main() -> ExitCode {
    // QE-413: install telemetry ONCE, before dispatch, so the job pipeline's `tracing` spans are
    // recorded. Held for the whole run; the guard flushes on drop.
    let _telemetry = init_cli_telemetry();

    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Initialise telemetry for the CLI, env-driven ([`TelemetryConfig::from_env`]) but forced onto
/// **stderr**: the parent `qe-server` reads this process's **stdout** as the `ProgressLine` JSON run
/// protocol, so telemetry must never touch stdout. Init failure (e.g. a bad `RUST_LOG` directive, or
/// a subscriber already installed) is non-fatal — the CLI logs a note to stderr and runs without
/// telemetry rather than aborting a user's job.
fn init_cli_telemetry() -> Option<TelemetryGuard> {
    let cfg = TelemetryConfig {
        writer: OutputStream::Stderr,
        ..TelemetryConfig::from_env()
    };
    match init_telemetry(&cfg) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!("warning: telemetry disabled: {e}");
            None
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    match parse_args(std::env::args().skip(1))? {
        Command::Version => {
            println!("quant-engine {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Train {
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
            indicator_subset,
            windows,
            folds,
        } => run_train_command(TrainCli {
            config,
            profile,
            run_dir,
            json,
            opts: TrainOptions {
                start,
                end,
                resolution,
                seed,
                generations,
                population,
                holdout,
                embargo,
                indicator_subset,
                windows,
                folds,
            },
        }),
        Command::Evolve {
            config,
            profile,
            run_dir,
            json,
            mode,
            start,
            end,
            resolution,
            seed,
            generations,
            offspring,
            states,
            k,
        } => run_evolve_command(EvolveCli {
            config,
            profile,
            run_dir,
            json,
            opts: EvolveOptions {
                mode,
                start,
                end,
                resolution,
                seed,
                generations,
                offspring,
                states,
                k,
            },
        }),
        Command::Backtest {
            vintage,
            strategy,
            start,
            end,
            resolution,
            universe,
            taker_fee_bps,
            slippage_model,
            reporting_impact,
            run_dir,
            json,
        } => run_backtest_command(BacktestCli {
            vintage,
            strategy,
            start,
            end,
            resolution,
            universe,
            taker_fee_bps,
            slippage_model,
            reporting_impact,
            run_dir,
            json,
        }),
        Command::Ingest {
            config,
            start,
            end,
            resolution,
            synthetic,
        } => run_ingest_command(&config, &start, &end, &resolution, synthetic),
    }
}

/// The parsed `backtest` command, one-to-one with [`Command::Backtest`].
struct BacktestCli {
    vintage: String,
    strategy: Option<String>,
    start: String,
    end: String,
    resolution: String,
    universe: Vec<String>,
    taker_fee_bps: f64,
    slippage_model: String,
    reporting_impact: Option<rust_decimal::Decimal>,
    run_dir: PathBuf,
    json: bool,
}

/// Dispatch `Command::Backtest`: stream JSON-line progress to stdout, write `result.json` into
/// `--run-dir`, and set the exit code. The store path and vintage repository root come from config
/// (`QE_CONFIG` or `config.toml`, `runtime-sim` profile): `storage.market_dir` and
/// `storage.artifacts_dir/vintages`.
fn run_backtest_command(cmd: BacktestCli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // QE-413: a top-level stage span so a CLI run emits structured telemetry (to stderr — never
    // stdout, which carries the ProgressLine protocol the server parses).
    let _span = tracing::info_span!(
        "cli.backtest",
        vintage = %cmd.vintage,
        resolution = %cmd.resolution,
    )
    .entered();
    tracing::info!(start = %cmd.start, end = %cmd.end, "backtest command started");

    let config_path = std::env::var("QE_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let cfg = Config::load(Profile::RuntimeSim, &PathBuf::from(config_path))?;

    let params = BacktestParams {
        store_path: PathBuf::from(&cfg.storage.market_dir),
        map_size: qe_storage::DEFAULT_MAP_SIZE,
        vintage_root: PathBuf::from(&cfg.storage.artifacts_dir).join("vintages"),
        vintage_id: cmd.vintage,
        strategy: cmd.strategy,
        start: cmd.start,
        end: cmd.end,
        resolution: cmd.resolution,
        universe: cmd.universe,
        taker_fee_bps: cmd.taker_fee_bps,
        slippage_model: cmd.slippage_model,
        reporting_impact: cmd.reporting_impact,
    };

    // Progress sink: JSON lines on stdout when `--json`, else a terse human line on stderr.
    let json = cmd.json;
    let mut progress = |pct: u8, stage: &str, msg: &str| {
        if json {
            let _ = emit_progress(&mut io::stdout().lock(), pct, stage, msg);
        } else {
            eprintln!("[{pct:>3}%] {stage}: {msg}");
        }
    };

    match run_backtest(&params, &mut progress) {
        Ok(doc) => {
            std::fs::create_dir_all(&cmd.run_dir)?;
            let out_path = cmd.run_dir.join("result.json");
            let bytes = serde_json::to_vec_pretty(&doc)?;
            std::fs::write(&out_path, &bytes)?;
            if json {
                let mut out = io::stdout().lock();
                emit_done(&mut out, "result.json")?;
                out.flush()?;
            } else {
                println!("wrote {}", out_path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            if json {
                let mut out = io::stdout().lock();
                let _ = emit_error(&mut out, &e.to_string());
                let _ = out.flush();
            } else {
                eprintln!("error: {e}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

/// The parsed `train` command, one-to-one with [`Command::Train`].
struct TrainCli {
    config: PathBuf,
    profile: Profile,
    run_dir: PathBuf,
    json: bool,
    opts: TrainOptions,
}

/// Dispatch `Command::Train` (QE-260): load config, run the search → ensemble → validation → G1 → seal
/// pipeline, stream JSON-line progress to stdout, write `result.json` into `--run-dir`, and set the exit
/// code. The terminal `{"t":"done",...}` names the sealed vintage id.
fn run_train_command(cmd: TrainCli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // QE-413: a top-level stage span so a CLI run emits structured telemetry (to stderr).
    let _span = tracing::info_span!("cli.train", profile = ?cmd.profile).entered();
    tracing::info!(run_dir = %cmd.run_dir.display(), "train command started");

    let cfg = Config::load(cmd.profile, &cmd.config)?;

    // Progress sink: JSON lines on stdout when `--json`, else a terse human line on stderr.
    let json = cmd.json;
    let mut emit = |line: ProgressLine| {
        if json {
            if let Ok(s) = serde_json::to_string(&line) {
                println!("{s}");
            }
        } else {
            eprintln!("{}", describe(&line));
        }
    };

    match run_train(&cfg, &cmd.opts, &code_commit(), &mut emit) {
        Ok(outcome) => {
            std::fs::create_dir_all(&cmd.run_dir)?;
            let out_path = cmd.run_dir.join("result.json");
            let bytes = serde_json::to_vec_pretty(&outcome.result)?;
            std::fs::write(&out_path, &bytes)?;
            if json {
                let mut out = io::stdout().lock();
                emit_train_done(&mut out, "result.json", &outcome.vintage_id)?;
                out.flush()?;
            } else {
                println!(
                    "sealed vintage {} → {}",
                    outcome.vintage_id,
                    outcome.vintage_path.display()
                );
                println!("wrote {}", out_path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            if json {
                let mut out = io::stdout().lock();
                let _ = emit_error(&mut out, &e.to_string());
                let _ = out.flush();
            } else {
                eprintln!("error: {e}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

/// The parsed `evolve` command, one-to-one with [`Command::Evolve`].
struct EvolveCli {
    config: PathBuf,
    profile: Profile,
    run_dir: PathBuf,
    json: bool,
    opts: EvolveOptions,
}

/// Dispatch `Command::Evolve` (QE-452): load config, run the illuminate → deflation → freeze → **seal a
/// formula pool** pipeline, stream JSON-line progress to stdout, write `result.json` into `--run-dir`, and
/// set the exit code. The terminal `{"t":"done",...}` names the sealed **pool** id — **never** a vintage.
fn run_evolve_command(cmd: EvolveCli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mode_label: &str = match cmd.opts.mode {
        EvolveMode::Sandbox => "sandbox",
        EvolveMode::Production => "production",
    };
    let _span =
        tracing::info_span!("cli.evolve", profile = ?cmd.profile, mode = mode_label).entered();
    tracing::info!(run_dir = %cmd.run_dir.display(), "evolve command started");

    let cfg = Config::load(cmd.profile, &cmd.config)?;

    let json = cmd.json;
    let mut emit = |line: ProgressLine| {
        if json {
            if let Ok(s) = serde_json::to_string(&line) {
                println!("{s}");
            }
        } else {
            eprintln!("{}", describe(&line));
        }
    };

    match run_evolve(&cfg, &cmd.opts, &code_commit(), &mut emit) {
        Ok(outcome) => {
            std::fs::create_dir_all(&cmd.run_dir)?;
            let out_path = cmd.run_dir.join("result.json");
            let bytes = serde_json::to_vec_pretty(&outcome.result)?;
            std::fs::write(&out_path, &bytes)?;
            // QE-452 Phase B: the MAP-Elites archive snapshot the `GET /api/runs/{id}/archive` route serves
            // (heatmap cells + trial-basis). A sidecar next to `result.json`; not a hashed artefact.
            let archive_path = cmd.run_dir.join("archive.json");
            std::fs::write(&archive_path, serde_json::to_vec_pretty(&outcome.archive)?)?;
            if json {
                let mut out = io::stdout().lock();
                emit_evolve_done(&mut out, "result.json", &outcome.pool_id)?;
                out.flush()?;
            } else {
                println!(
                    "sealed formula pool {} → {}",
                    outcome.pool_id,
                    outcome.pool_path.display()
                );
                println!("wrote {}", out_path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            if json {
                let mut out = io::stdout().lock();
                let _ = emit_error(&mut out, &e.to_string());
                let _ = out.flush();
            } else {
                eprintln!("error: {e}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Format an `Option<f64>` protocol field for the terse human line: `6`-dp when finite, `n/a` when the
/// value was non-finite on the wire (`null` → `None`).
fn fmt_opt_f64(v: Option<f64>) -> String {
    v.map_or_else(|| "n/a".to_owned(), |x| format!("{x:.6}"))
}

/// A terse human line for a train [`ProgressLine`] (the non-`--json` path).
fn describe(line: &ProgressLine) -> String {
    match line {
        ProgressLine::Progress { pct, stage, msg } => format!("[{pct:>3}%] {stage}: {msg}"),
        ProgressLine::Gen {
            pct,
            generation,
            generations,
            coverage,
            best_fitness,
            ..
        } => format!(
            "[{pct:>3}%] search: gen {generation}/{generations} coverage={coverage} \
             best_fitness={}",
            fmt_opt_f64(*best_fitness)
        ),
        ProgressLine::Ensemble {
            pct,
            folds,
            members,
            score,
            ..
        } => format!(
            "[{pct:>3}%] ensemble: {members} members, {folds} folds, score={}",
            fmt_opt_f64(*score)
        ),
        ProgressLine::Gate {
            pct,
            promoted,
            failed,
            ..
        } => format!(
            "[{pct:>3}%] gate: G1 {} (failed: {})",
            if *promoted { "PASS" } else { "FAIL" },
            if failed.is_empty() {
                "none".to_owned()
            } else {
                failed.join(", ")
            }
        ),
        ProgressLine::Done {
            result,
            vintage,
            pool,
            ..
        } => match (vintage, pool) {
            (Some(v), _) => format!("done: {result} (vintage {v})"),
            (None, Some(p)) => format!("done: {result} (pool {p})"),
            (None, None) => format!("done: {result}"),
        },
        ProgressLine::Error { msg } => format!("error: {msg}"),
    }
}

/// Dispatch `Command::Ingest`: stream a terminal JSON-line outcome on stdout and set the exit code.
///
/// Two modes:
/// * **default** (no `--synthetic`): real market-data decoders live behind the default-off `http`
///   feature, so with no live `HistoricalSource` this reports the missing source as a terminal
///   `{"t":"error"}` line and exits non-zero — unchanged from before the flag.
/// * **`--synthetic`**: populate the store from a deterministic **OFFLINE synthetic** generator
///   ([`SyntheticSource`]) over the config universe + window, reusing the in-memory-tested
///   [`run_ingest`] job. The data is **GENERATED, NOT real market data**, so the run is loudly
///   labelled: a stderr warning plus a `"synthetic":true` marker in the terminal `done` line.
fn run_ingest_command(
    config: &Path,
    start: &str,
    end: &str,
    resolution: &str,
    synthetic: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // QE-413: a top-level stage span so a CLI run emits structured telemetry (to stderr).
    let _span = tracing::info_span!("cli.ingest", resolution = %resolution, synthetic).entered();
    tracing::info!(start = %start, end = %end, "ingest command started");

    if synthetic {
        return run_synthetic_ingest(config, start, end, resolution);
    }

    let detail = format!("window {start}..{end} at {resolution}");
    #[cfg(feature = "http")]
    let msg = format!(
        "ingest ({detail}): the `http` market-data decoders are not yet implemented \
         — QE-253 ships the scaffold + in-memory-tested run_ingest; real ingestion is future work \
         (use `--synthetic` for a deterministic offline dev store)"
    );
    #[cfg(not(feature = "http"))]
    let msg = format!(
        "ingest ({detail}): real market-data ingestion requires the `http` feature \
         (out of scope for QE-253 — run_ingest is exercised with an in-memory source in tests); \
         use `--synthetic` for a deterministic offline dev store"
    );
    let mut out = io::stdout().lock();
    emit_error(&mut out, &msg)?;
    out.flush()?;
    Ok(ExitCode::FAILURE)
}

/// Outcome summary of a synthetic ingest — for the human stderr report only.
struct SyntheticSummary {
    store_path: PathBuf,
    instruments: usize,
    bars: usize,
}

/// Run the offline synthetic ingest: emit the loud warning, populate the store, and stream the terminal
/// line. Errors are surfaced as a terminal `{"t":"error"}` line + non-zero exit (protocol-consistent
/// with the real path), never a panic.
fn run_synthetic_ingest(
    config: &Path,
    start: &str,
    end: &str,
    resolution: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // A loud, honest, impossible-to-miss warning BEFORE any work — this store is not real data.
    eprintln!(
        "WARNING: `qe ingest --synthetic` generates DETERMINISTIC SYNTHETIC market data. \
         The resulting store holds GENERATED bars, NOT real market prices — never treat it as real."
    );

    match generate_synthetic_store(config, start, end, resolution) {
        Ok(summary) => {
            // Terminal JSON-line outcome carries the loud `"synthetic":true` marker.
            let mut out = io::stdout().lock();
            emit_ingest_done(&mut out, "synthetic-store", true)?;
            out.flush()?;
            eprintln!(
                "synthetic: populated {} with {} instrument(s), {} bar(s) total — \
                 SYNTHETIC DATA (not real market prices)",
                summary.store_path.display(),
                summary.instruments,
                summary.bars,
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            let mut out = io::stdout().lock();
            let _ = emit_error(&mut out, &e.to_string());
            let _ = out.flush();
            eprintln!("error: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Build the [`SyntheticSource`] for each config-universe instrument over the `[start, end)` window at
/// `resolution` and drive the tested [`run_ingest`] job to persist its bars into the config store.
///
/// The generator is seeded from `config.determinism.seed`, decorrelated per instrument by its index, so
/// the same config + window + resolution always reproduces identical bars.
fn generate_synthetic_store(
    config: &Path,
    start: &str,
    end: &str,
    resolution: &str,
) -> Result<SyntheticSummary, Box<dyn std::error::Error>> {
    let cfg = Config::load(Profile::RuntimeSim, config)?;

    let res: Resolution = resolution.parse().map_err(|_| {
        format!("invalid --resolution `{resolution}` (expected 1m/5m/15m/1h/4h/1d)")
    })?;
    let start_ms = parse_ymd_to_millis(start)
        .ok_or_else(|| format!("invalid --start `{start}` (expected YYYY-MM-DD)"))?;
    let end_ms = parse_ymd_to_millis(end)
        .ok_or_else(|| format!("invalid --end `{end}` (expected YYYY-MM-DD)"))?;
    if end_ms <= start_ms {
        return Err(
            format!("empty window: --start {start} must be strictly before --end {end}").into(),
        );
    }

    let store_path = PathBuf::from(&cfg.storage.market_dir);
    let mut total_bars = 0usize;
    for (idx, symbol) in cfg.instruments.iter().enumerate() {
        let mut source =
            SyntheticSource::new(res, start_ms, end_ms, cfg.determinism.seed, idx as u64);
        let bars = source.bar_count();
        let params = IngestParams {
            store_path: store_path.clone(),
            map_size: qe_storage::DEFAULT_MAP_SIZE,
            instrument: symbol.clone(),
        };
        let mut progress = |pct: u8, stage: &str, msg: &str| {
            eprintln!("[{pct:>3}%] {symbol} (synthetic) {stage}: {msg}");
        };
        run_ingest(&params, &mut source, &mut progress)?;
        total_bars += bars;
        eprintln!("synthetic: {symbol}: generated {bars} {resolution} bar(s)");
    }

    Ok(SyntheticSummary {
        store_path,
        instruments: cfg.instruments.len(),
        bars: total_bars,
    })
}
