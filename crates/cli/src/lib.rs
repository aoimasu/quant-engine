//! quant-engine CLI library — the composition root for the runnable pipelines.
//!
//! QE-013 wires the **training-run skeleton**: load config (QE-002), resolve the point-in-time
//! universe (QE-012), ensure the configurable state directories exist, and emit a content-addressed
//! **vintage manifest** built from a real [`qe_determinism::Lineage`] (QE-006). The training *stages*
//! (QE-101+) hang off [`run_train`]; this already produces a resolvable vintage from real inputs.
//!
//! The logic lives here (not in `main.rs`) so it is unit- and integration-testable.

use std::path::{Path, PathBuf};

use qe_config::Config;
use qe_determinism::Lineage;
use qe_domain::VintageHash;
use serde::Serialize;
use thiserror::Error;

/// The vintage-manifest schema version. Bump on an incompatible manifest-shape change.
pub const VINTAGE_MANIFEST_SCHEMA: u32 = 1;

/// Errors from a CLI run.
#[derive(Debug, Error)]
pub enum CliError {
    /// Bad command-line usage (unknown flag, missing value, unknown command).
    #[error("usage error: {0}")]
    Usage(String),

    /// Config load/validation failure.
    #[error(transparent)]
    Config(#[from] qe_config::ConfigError),

    /// Lineage hashing failure.
    #[error(transparent)]
    Lineage(#[from] qe_determinism::LineageError),

    /// The lineage id was not a valid vintage hash (should never happen — it's a SHA-256).
    #[error("invalid vintage hash: {0}")]
    Vintage(qe_domain::DomainError),

    /// Filesystem error creating a state directory or writing the manifest.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },

    /// The manifest could not be serialised.
    #[error("failed to serialise vintage manifest: {0}")]
    Serialize(String),
}

/// A produced vintage: its content-addressed id and the manifest path on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vintage {
    /// The vintage id — a 64-hex SHA-256 over the lineage (the artefact primary key).
    pub id: VintageHash,
    /// Where the manifest was written (`<artifacts_dir>/vintages/<id>/manifest.json`).
    pub manifest_path: PathBuf,
}

/// One instrument's point-in-time window, as recorded in the manifest.
#[derive(Debug, Clone, Serialize)]
struct InstrumentRecord {
    instrument: String,
    listed_ms: i64,
    delisted_ms: Option<i64>,
}

/// The on-disk vintage manifest — content-addressed, reproducible (no wall-clock).
#[derive(Debug, Clone, Serialize)]
struct VintageManifest {
    schema: u32,
    vintage_id: String,
    profile: String,
    lineage: Lineage,
    /// The full universe roster, including delisted symbols (no survivorship drop).
    universe: Vec<InstrumentRecord>,
}

/// Run the training pipeline for `cfg`, producing a vintage manifest under the configured
/// artefacts directory. `code_commit` is the build's code provenance (folded into the vintage id),
/// passed in so the result is deterministic and testable.
///
/// # Errors
/// [`CliError`] on config/universe validation, directory creation, lineage hashing, or manifest IO.
pub fn run_train(cfg: &Config, code_commit: &str) -> Result<Vintage, CliError> {
    // 1. Resolve the point-in-time universe (validates listing/delisting windows).
    let universe = cfg.universe()?;

    // 2. Ensure every *configurable* persistent-state directory exists. All paths come from config;
    //    none are absolute or hard-coded here.
    for dir in [
        &cfg.storage.market_dir,
        &cfg.storage.synthetic_dir,
        &cfg.storage.artifacts_dir,
    ] {
        create_dir(dir)?;
    }

    // 3. Build the vintage lineage from real inputs (config hash + seed). No input snapshot yet
    //    (the ingest stages are P1), so the snapshot id is empty.
    let lineage = Lineage::from_config(cfg, "", code_commit, vec![cfg.determinism.seed])?;
    let id = VintageHash::new(lineage.id()?).map_err(CliError::Vintage)?;

    // 4. Write the manifest to <artifacts_dir>/vintages/<id>/manifest.json.
    let manifest = VintageManifest {
        schema: VINTAGE_MANIFEST_SCHEMA,
        vintage_id: id.as_str().to_owned(),
        profile: cfg.profile.as_str().to_owned(),
        lineage,
        universe: universe
            .all_known()
            .iter()
            .map(|l| InstrumentRecord {
                instrument: l.instrument().as_str().to_owned(),
                listed_ms: l.listed().millis(),
                delisted_ms: l.delisted().map(|d| d.millis()),
            })
            .collect(),
    };

    let vintage_dir = Path::new(&cfg.storage.artifacts_dir)
        .join("vintages")
        .join(id.as_str());
    create_dir(&vintage_dir)?;
    let manifest_path = vintage_dir.join("manifest.json");
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| CliError::Serialize(e.to_string()))?;
    write_file(&manifest_path, &bytes)?;

    Ok(Vintage { id, manifest_path })
}

fn create_dir(path: impl AsRef<Path>) -> Result<(), CliError> {
    let path = path.as_ref();
    std::fs::create_dir_all(path).map_err(|source| CliError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    std::fs::write(path, bytes).map_err(|source| CliError::Io {
        path: path.display().to_string(),
        source,
    })
}

// ---- command-line parsing ------------------------------------------------------------------------

/// A parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Print the version and exit (the bare invocation).
    Version,
    /// Run the training pipeline.
    Train {
        /// Config file path.
        config: PathBuf,
        /// Operating profile.
        profile: qe_config::Profile,
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
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--config" => {
                        let v = it
                            .next()
                            .ok_or_else(|| CliError::Usage("--config needs a value".to_owned()))?;
                        config = PathBuf::from(v);
                    }
                    "--profile" => {
                        let v = it
                            .next()
                            .ok_or_else(|| CliError::Usage("--profile needs a value".to_owned()))?;
                        profile = parse_profile(&v)?;
                    }
                    other => {
                        return Err(CliError::Usage(format!("unknown flag `{other}`")));
                    }
                }
            }
            Ok(Command::Train { config, profile })
        }
        other => Err(CliError::Usage(format!("unknown command `{other}`"))),
    }
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
        let cmd = parse_args(["train"]).unwrap();
        assert_eq!(
            cmd,
            Command::Train {
                config: PathBuf::from("config.toml"),
                profile: qe_config::Profile::Train,
            }
        );
        let cmd = parse_args(["train", "--config", "my.toml", "--profile", "runtime-sim"]).unwrap();
        assert_eq!(
            cmd,
            Command::Train {
                config: PathBuf::from("my.toml"),
                profile: qe_config::Profile::RuntimeSim,
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
}
