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
            flow,
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
                flow,
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
            instruments,
            fetch_all,
            synthetic,
            run_dir,
            json,
        } => run_ingest_command(IngestCommand {
            config: &config,
            start: &start,
            end: &end,
            resolution: &resolution,
            instruments: &instruments,
            fetch_all,
            synthetic,
            run_dir: run_dir.as_deref(),
            json,
        }),
    }
}

/// The parsed `ingest` command, one-to-one with [`Command::Ingest`] (QE-464).
struct IngestCommand<'a> {
    config: &'a Path,
    start: &'a str,
    end: &'a str,
    resolution: &'a str,
    instruments: &'a [String],
    fetch_all: bool,
    synthetic: bool,
    run_dir: Option<&'a Path>,
    json: bool,
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

/// The `result.json` an ingest run writes into its `--run-dir` (QE-464), so the supervisor records a
/// successful run and the SPA (QE-465) can render what was ingested + its provenance/liquidity flags.
#[derive(Debug, serde::Serialize)]
struct IngestOutcome {
    /// `synthetic` (offline generator) or `real` (the QE-463 decoder).
    mode: &'static str,
    /// The ingest window + resolution.
    start: String,
    end: String,
    resolution: String,
    /// The resolved instruments actually ingested (config order).
    instruments: Vec<String>,
    /// Total bars written across all instruments.
    bars: usize,
    /// `true` when a fetch-all resolved through a universe with no listing dates — the store is flagged
    /// **survivorship-unsafe** rather than trusted as point-in-time (QE-448/QE-464).
    survivorship_unsafe: bool,
    /// Names the liquidity screen flags as NOT admissible as tradable at size (thin or uncalibrated,
    /// QE-440/QE-447) — recorded so fetch-all never silently admits an illiquid alt at the major floor.
    liquidity_flagged: Vec<String>,
}

/// Dispatch `Command::Ingest`: stream a terminal JSON-line outcome on stdout, write `result.json` into
/// the `--run-dir` (so a supervised run reports success), and set the exit code.
///
/// Two modes:
/// * **`--synthetic`**: populate the store from a deterministic **OFFLINE synthetic** generator
///   ([`SyntheticSource`]) over the resolved instrument set + window, reusing the in-memory-tested
///   [`run_ingest`] job (bars tagged `synthetic`). Loudly labelled (stderr warning + `"synthetic":true`
///   in the terminal `done` line).
/// * **default** (real): under the `http` feature, the QE-463 decoder fetches each resolved instrument
///   (bars tagged `real` / `uncalibrated`, the klines-only slice's asserted marker); without it, this
///   reports the missing decoder as a terminal `{"t":"error"}` line and exits non-zero.
fn run_ingest_command(cmd: IngestCommand<'_>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // QE-413: a top-level stage span so a CLI run emits structured telemetry (to stderr).
    let _span = tracing::info_span!(
        "cli.ingest",
        resolution = %cmd.resolution,
        synthetic = cmd.synthetic,
        fetch_all = cmd.fetch_all,
        json = cmd.json,
    )
    .entered();
    tracing::info!(start = %cmd.start, end = %cmd.end, "ingest command started");

    if cmd.synthetic {
        return run_synthetic_ingest(&cmd);
    }
    run_real_ingest(&cmd)
}

/// Resolve the instruments to ingest: `--fetch-all` resolves the whole point-in-time universe (with a
/// survivorship-safety verdict); explicit `--instrument`s are used verbatim; otherwise the config's flat
/// `instruments` roster (still checked for survivorship-safety when it feeds a whole-store ingest).
fn resolve_ingest_instruments(
    cfg: &Config,
    explicit: &[String],
    fetch_all: bool,
) -> Result<(Vec<String>, bool), Box<dyn std::error::Error>> {
    if fetch_all {
        let universe = cfg.universe()?;
        let resolved = qe_ingest::resolve_fetch_all(&universe);
        let instruments = resolved
            .instruments
            .iter()
            .map(|i| i.as_str().to_owned())
            .collect();
        Ok((instruments, resolved.survivorship_unsafe))
    } else if explicit.iter().any(|s| !s.trim().is_empty()) {
        // An explicit, operator-chosen list is not a "fetch all" — survivorship framing does not apply.
        Ok((explicit.to_vec(), false))
    } else {
        let universe = cfg.universe()?;
        let survivorship_unsafe = qe_ingest::resolve_fetch_all(&universe).survivorship_unsafe;
        Ok((cfg.instruments.clone(), survivorship_unsafe))
    }
}

/// Parse + validate the ingest window, returning `(resolution, start_ms, end_ms)`.
fn parse_ingest_window(
    resolution: &str,
    start: &str,
    end: &str,
) -> Result<(Resolution, i64, i64), Box<dyn std::error::Error>> {
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
    Ok((res, start_ms, end_ms))
}

/// Write the ingest `result.json` into `run_dir` (if supervised), so the run is recorded as succeeded.
fn write_ingest_result(
    run_dir: Option<&Path>,
    outcome: &IngestOutcome,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = run_dir {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("result.json"), serde_json::to_vec_pretty(outcome)?)?;
    }
    Ok(())
}

/// Emit the terminal `done` line + the human stderr report shared by both ingest modes.
fn finish_ingest(
    outcome: &IngestOutcome,
    run_dir: Option<&Path>,
    synthetic: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    write_ingest_result(run_dir, outcome)?;
    let mut out = io::stdout().lock();
    let result = if synthetic {
        "synthetic-store"
    } else {
        "real-store"
    };
    emit_ingest_done(&mut out, result, synthetic)?;
    out.flush()?;
    if outcome.survivorship_unsafe {
        eprintln!(
            "WARNING: fetch-all resolved a universe with NO listing dates — the resulting store is \
             flagged SURVIVORSHIP-UNSAFE (add `[[universe]]` listed/delisted dates for a point-in-time \
             roster)."
        );
    }
    if !outcome.liquidity_flagged.is_empty() {
        eprintln!(
            "liquidity: {} name(s) flagged NOT tradable-at-size (thin/uncalibrated — QE-440/QE-447): {}",
            outcome.liquidity_flagged.len(),
            outcome.liquidity_flagged.join(", "),
        );
    }
    eprintln!(
        "ingest: populated {} instrument(s), {} bar(s) total ({})",
        outcome.instruments.len(),
        outcome.bars,
        outcome.mode,
    );
    Ok(ExitCode::SUCCESS)
}

/// Run the offline synthetic ingest: emit the loud warning, populate the store, and stream the terminal
/// line. Errors are surfaced as a terminal `{"t":"error"}` line + non-zero exit (protocol-consistent
/// with the real path), never a panic.
fn run_synthetic_ingest(cmd: &IngestCommand<'_>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    // A loud, honest, impossible-to-miss warning BEFORE any work — this store is not real data.
    eprintln!(
        "WARNING: `qe ingest --synthetic` generates DETERMINISTIC SYNTHETIC market data. \
         The resulting store holds GENERATED bars, NOT real market prices — never treat it as real."
    );

    match generate_synthetic_store(cmd) {
        Ok(outcome) => finish_ingest(&outcome, cmd.run_dir, true),
        Err(e) => {
            let mut out = io::stdout().lock();
            let _ = emit_error(&mut out, &e.to_string());
            let _ = out.flush();
            eprintln!("error: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Build the [`SyntheticSource`] for each resolved instrument over the `[start, end)` window at
/// `resolution` and drive the tested [`run_ingest`] job to persist its bars into the config store.
///
/// The generator is seeded from `config.determinism.seed`, decorrelated per instrument by its index, so
/// the same config + window + resolution always reproduces identical bars.
fn generate_synthetic_store(
    cmd: &IngestCommand<'_>,
) -> Result<IngestOutcome, Box<dyn std::error::Error>> {
    let cfg = Config::load(Profile::RuntimeSim, cmd.config)?;
    let (res, start_ms, end_ms) = parse_ingest_window(cmd.resolution, cmd.start, cmd.end)?;
    let (instruments, survivorship_unsafe) =
        resolve_ingest_instruments(&cfg, cmd.instruments, cmd.fetch_all)?;

    let store_path = PathBuf::from(&cfg.storage.market_dir);
    let mut total_bars = 0usize;
    for (idx, symbol) in instruments.iter().enumerate() {
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
        // `--synthetic` tags every bar `synthetic` / `uncalibrated` so no store reads as real.
        run_ingest(
            &params,
            &mut source,
            qe_storage::Provenance::Synthetic,
            qe_storage::Calibration::Uncalibrated,
            &mut progress,
        )?;
        total_bars += bars;
        eprintln!(
            "synthetic: {symbol}: generated {bars} {cmd_res} bar(s)",
            cmd_res = cmd.resolution
        );
    }

    Ok(IngestOutcome {
        mode: "synthetic",
        start: cmd.start.to_owned(),
        end: cmd.end.to_owned(),
        resolution: cmd.resolution.to_owned(),
        // Synthetic bars carry no measured ADV/impact ⇒ every name is uncalibrated (not tradable at
        // size). Record the flags so a synthetic store never reads as capacity-eligible.
        liquidity_flagged: instruments.clone(),
        instruments,
        bars: total_bars,
        survivorship_unsafe,
    })
}

/// The liquidity flags for a klines-only real ingest: no ADV/impact is measured, so every name screens
/// [`qe_ingest::LiquidityVerdict::Uncalibrated`] — recorded so fetch-all never silently admits an alt as
/// tradable at size (QE-440/QE-447). Deployments that later fit ADV feed real inputs.
#[cfg(feature = "http")]
fn liquidity_flags_uncalibrated(instruments: &[String]) -> Vec<String> {
    let candidates: Vec<qe_ingest::LiquidityInput> = instruments
        .iter()
        .filter_map(|s| {
            qe_domain::InstrumentId::new(s)
                .ok()
                .map(|instrument| qe_ingest::LiquidityInput {
                    instrument,
                    rolling_adv_usd: None, // klines-only ingest measures no ADV (QE-440 is a follow-up)
                })
        })
        .collect();
    let screened = qe_ingest::screen_liquidity(
        &candidates,
        rust_decimal::Decimal::from(qe_ingest::DEFAULT_MIN_ADV_USD),
    );
    screened
        .iter()
        .filter(|s| !s.verdict.is_tradable())
        .map(|s| s.instrument.as_str().to_owned())
        .collect()
}

/// Run the real market-data ingest (the QE-463 Binance USDT-M decoder, behind the `http` feature).
///
/// Resolves the instrument set (explicit / fetch-all via the point-in-time universe), fetches each
/// instrument's missing closed klines + funding, and persists them tagged `real` / `uncalibrated` (the
/// klines-only slice's asserted [`qe_ingest::CalibrationSource::Uncalibrated`] marker). Without the
/// `http` feature the decoder is unavailable, so this reports a terminal error + non-zero exit.
#[cfg(feature = "http")]
fn run_real_ingest(cmd: &IngestCommand<'_>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use qe_cli::jobs::ingest::BinanceHistoricalSource;

    let run = || -> Result<IngestOutcome, Box<dyn std::error::Error>> {
        let cfg = Config::load(Profile::RuntimeSim, cmd.config)?;
        let (res, start_ms, end_ms) = parse_ingest_window(cmd.resolution, cmd.start, cmd.end)?;
        let (instruments, survivorship_unsafe) =
            resolve_ingest_instruments(&cfg, cmd.instruments, cmd.fetch_all)?;
        let store_path = PathBuf::from(&cfg.storage.market_dir);
        // Wall-clock now (ingest is a P1 stage — not determinism-bound), for the closed-window filter.
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX);

        let mut total_bars = 0usize;
        for symbol in &instruments {
            let instrument = qe_domain::InstrumentId::new(symbol)
                .map_err(|e| format!("invalid instrument `{symbol}`: {e}"))?;
            let request = qe_ingest::WindowRequest {
                symbol: instrument.clone(),
                resolution: res,
                from_ms: start_ms,
                to_ms: end_ms,
                now_ms,
                limit: 1000,
            };
            // Fetch the whole window (present-set empty ⇒ the decoder fills gaps); tagged real/uncalibrated.
            let mut source = BinanceHistoricalSource::live(
                qe_ingest::DEFAULT_REST_BASE,
                request,
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
            );
            let params = IngestParams {
                store_path: store_path.clone(),
                map_size: qe_storage::DEFAULT_MAP_SIZE,
                instrument: symbol.clone(),
            };
            let mut progress = |pct: u8, stage: &str, msg: &str| {
                eprintln!("[{pct:>3}%] {symbol} (real) {stage}: {msg}");
            };
            run_ingest(
                &params,
                &mut source,
                qe_storage::Provenance::Real,
                qe_storage::Calibration::Uncalibrated,
                &mut progress,
            )?;
            if let Ok(store) =
                qe_storage::MarketStore::open(&store_path, qe_storage::DEFAULT_MAP_SIZE)
            {
                if let Ok(Some((_, _, bars))) = store.coverage_bounds(&instrument, res) {
                    total_bars = total_bars.max(bars);
                }
            }
        }

        Ok(IngestOutcome {
            mode: "real",
            start: cmd.start.to_owned(),
            end: cmd.end.to_owned(),
            resolution: cmd.resolution.to_owned(),
            liquidity_flagged: liquidity_flags_uncalibrated(&instruments),
            instruments,
            bars: total_bars,
            survivorship_unsafe,
        })
    };

    match run() {
        Ok(outcome) => finish_ingest(&outcome, cmd.run_dir, false),
        Err(e) => {
            let mut out = io::stdout().lock();
            let _ = emit_error(&mut out, &e.to_string());
            let _ = out.flush();
            eprintln!("error: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Without the `http` feature the real decoder is unavailable — report it as a terminal error line +
/// non-zero exit (unchanged from before this ticket); `--synthetic` is the offline path.
#[cfg(not(feature = "http"))]
fn run_real_ingest(cmd: &IngestCommand<'_>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let detail = format!("window {}..{} at {}", cmd.start, cmd.end, cmd.resolution);
    let msg = format!(
        "ingest ({detail}): real market-data ingestion requires the `http` feature \
         (QE-463 decoder); use `--synthetic` for a deterministic offline dev store"
    );
    let mut out = io::stdout().lock();
    emit_error(&mut out, &msg)?;
    out.flush()?;
    Ok(ExitCode::FAILURE)
}
