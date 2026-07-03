//! quant-engine CLI entry point.
//!
//! Thin dispatcher over [`qe_cli`]: parse args, run the command, print a result or a usage error.
//! All logic lives in the library so it stays testable (QE-013).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qe_cli::jobs::backtest::{run_backtest, BacktestParams};
use qe_cli::jobs::{emit_done, emit_error, emit_progress};
use qe_cli::{parse_args, run_train, Command};
use qe_config::{Config, Profile};

/// Code provenance folded into the vintage id. Set `QE_CODE_COMMIT` at build/run time (e.g. the git
/// SHA); falls back to the crate version so a vintage is always attributable.
fn code_commit() -> String {
    std::env::var("QE_CODE_COMMIT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    match parse_args(std::env::args().skip(1))? {
        Command::Version => {
            println!("quant-engine {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Train { config, profile } => {
            let cfg = Config::load(profile, &config)?;
            let vintage = run_train(&cfg, &code_commit())?;
            println!(
                "produced vintage {} → {}",
                vintage.id,
                vintage.manifest_path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Backtest {
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
        } => run_backtest_command(BacktestCli {
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
        }),
        Command::Ingest {
            config,
            start,
            end,
            resolution,
        } => run_ingest_command(&config, &start, &end, &resolution),
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
    run_dir: PathBuf,
    json: bool,
}

/// Dispatch `Command::Backtest`: stream JSON-line progress to stdout, write `result.json` into
/// `--run-dir`, and set the exit code. The store path and vintage repository root come from config
/// (`QE_CONFIG` or `config.toml`, `runtime-sim` profile): `storage.market_dir` and
/// `storage.artifacts_dir/vintages`.
fn run_backtest_command(cmd: BacktestCli) -> Result<ExitCode, Box<dyn std::error::Error>> {
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

/// Dispatch `Command::Ingest`: stream a terminal JSON-line outcome on stdout and set the exit code.
///
/// Real market-data decoders live behind the default-off `http` feature (out of scope for QE-253):
/// this binary ships the fully-wired command plus the in-memory-tested [`run_ingest`] job
/// (`qe_cli::jobs::ingest`), but no live `HistoricalSource`, so it reports the missing source as a
/// terminal `{"t":"error"}` line and exits non-zero. Constructing a real source under `http` and
/// calling `run_ingest` here is the future-work seam.
fn run_ingest_command(
    _config: &Path,
    start: &str,
    end: &str,
    resolution: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let detail = format!("window {start}..{end} at {resolution}");
    #[cfg(feature = "http")]
    let msg = format!(
        "ingest ({detail}): the `http` market-data decoders are not yet implemented \
         — QE-253 ships the scaffold + in-memory-tested run_ingest; real ingestion is future work"
    );
    #[cfg(not(feature = "http"))]
    let msg = format!(
        "ingest ({detail}): real market-data ingestion requires the `http` feature \
         (out of scope for QE-253 — run_ingest is exercised with an in-memory source in tests)"
    );
    let mut out = io::stdout().lock();
    emit_error(&mut out, &msg)?;
    out.flush()?;
    Ok(ExitCode::FAILURE)
}
