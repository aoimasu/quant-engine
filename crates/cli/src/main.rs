//! quant-engine CLI entry point.
//!
//! Thin dispatcher over [`qe_cli`]: parse args, run the command, print a result or a usage error.
//! All logic lives in the library so it stays testable (QE-013).

use std::process::ExitCode;

use qe_cli::{parse_args, run_train, Command};
use qe_config::Config;

/// Code provenance folded into the vintage id. Set `QE_CODE_COMMIT` at build/run time (e.g. the git
/// SHA); falls back to the crate version so a vintage is always attributable.
fn code_commit() -> String {
    std::env::var("QE_CODE_COMMIT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match parse_args(std::env::args().skip(1))? {
        Command::Version => {
            println!("quant-engine {}", env!("CARGO_PKG_VERSION"));
        }
        Command::Train { config, profile } => {
            let cfg = Config::load(profile, &config)?;
            let vintage = run_train(&cfg, &code_commit())?;
            println!(
                "produced vintage {} → {}",
                vintage.id,
                vintage.manifest_path.display()
            );
        }
    }
    Ok(())
}
