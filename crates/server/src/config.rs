//! QE-419: one source of truth for the shared storage dirs across `qe-server` and the spawned `qe-cli`.
//!
//! Before QE-419 the server re-declared `artifacts_dir`/`market_dir` through a parallel `QE_SERVER_*`
//! namespace while the spawned CLI read the same dirs from `qe-config` (`config.toml [storage]`) — the
//! same physical dirs configured **twice with no cross-check**, so a divergence silently pointed the
//! server's read APIs at a different store than training wrote.
//!
//! This module makes `qe-config` `[storage]` the single source both sides read, and provides a pure,
//! unit-testable boot guard ([`check_storage_dirs_match`]) that refuses to boot on any residual
//! divergence — mirroring QE-409's `check_session_secret_policy` pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qe_config::{Config, Profile};
use qe_storage::{MarketStore, StorageError, DEFAULT_MAP_SIZE};
use qe_vintage::VintageRepository;

use crate::ReadState;

/// Environment variable naming the `qe-config` file. **Shared with the CLI**: the spawned `qe-cli`
/// reads the very same `QE_CONFIG` (see `crates/cli/src/main.rs`), so pinning it onto the child ties
/// both sides to one file.
pub const ENV_CONFIG: &str = "QE_CONFIG";

/// Default `qe-config` path when [`ENV_CONFIG`] is unset — matches the CLI's `config.toml` default so
/// server and CLI resolve the same file with no configuration at all.
pub const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// **Deprecated** (QE-419) server-only override for the sealed-vintage artifacts dir. Prefer
/// `config.toml [storage].artifacts_dir` (or the unified `QE_STORAGE__ARTIFACTS_DIR` figment env,
/// which overrides both the server load and the child load identically). When set, it is cross-checked
/// against the qe-config value at boot and a divergence **refuses boot** — so it can no longer make the
/// server and CLI disagree.
pub const ENV_ARTIFACTS_DIR: &str = "QE_SERVER_ARTIFACTS_DIR";

/// **Deprecated** (QE-419) server-only override for the market-store dir. See [`ENV_ARTIFACTS_DIR`].
pub const ENV_MARKET_DIR: &str = "QE_SERVER_MARKET_DIR";

/// The two storage dirs that MUST be identical between the server's read APIs (`/api/vintages`,
/// `/api/market-data/coverage`) and the spawned `qe-cli`. A `PartialEq`/`Eq` pair so the boot guard is
/// a trivial structural compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDirs {
    /// Sealed-vintage artifacts dir the `/api/vintages` endpoint lists (rooted [`VintageRepository`]).
    pub artifacts_dir: PathBuf,
    /// Market-store dir the `/api/market-data/coverage` endpoint scans ([`MarketStore`]).
    pub market_dir: PathBuf,
}

impl StorageDirs {
    /// The dirs the **spawned CLI** reads: taken verbatim from `qe-config` `[storage]`. This is the
    /// single source of truth — the same fields `crates/cli/src/main.rs` resolves for the backtest.
    #[must_use]
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            artifacts_dir: PathBuf::from(&cfg.storage.artifacts_dir),
            market_dir: PathBuf::from(&cfg.storage.market_dir),
        }
    }

    /// Build the QE-257 [`ReadState`]: a [`VintageRepository`] rooted at `artifacts_dir` and the market
    /// store opened **once** at `market_dir` (a single open — concurrent re-open of the same LMDB path
    /// is UB; see [`ReadState`]).
    ///
    /// # Errors
    /// [`StorageError`] if the market store cannot be opened (I/O, LMDB, or a schema mismatch).
    pub fn read_state(&self) -> Result<Arc<ReadState>, StorageError> {
        let market_store = Arc::new(MarketStore::open(&self.market_dir, DEFAULT_MAP_SIZE)?);
        let vintages = VintageRepository::new(self.artifacts_dir.clone());
        Ok(Arc::new(ReadState::new(vintages, market_store)))
    }
}

/// A detected boot-time divergence between the server's read-state dirs and the spawned CLI's dirs.
///
/// The read APIs would scan a different store than training wrote — the silent misconfiguration QE-419
/// closes — so this is surfaced at BOOT (refuse to proceed), never lazily at query time.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "storage-dir mismatch: the server would read artifacts={server_artifacts}, market={server_market} \
     but the spawned qe-cli reads artifacts={cli_artifacts}, market={cli_market} — the read APIs would \
     scan a different store than training wrote. Reconcile config.toml [storage] with any \
     QE_SERVER_ARTIFACTS_DIR/QE_SERVER_MARKET_DIR override (deprecated)."
)]
pub struct StorageDirMismatch {
    /// Server's resolved artifacts dir.
    pub server_artifacts: String,
    /// Server's resolved market dir.
    pub server_market: String,
    /// Spawned CLI's artifacts dir.
    pub cli_artifacts: String,
    /// Spawned CLI's market dir.
    pub cli_market: String,
}

/// QE-419 boot guard: the server's read-state dirs (`server`) and the dirs the spawned CLI reads
/// (`cli`) must be identical. Pure and unit-tested (matching ⇒ `Ok`, divergent ⇒ `Err`), analogous to
/// QE-409's `check_session_secret_policy` — wired at boot so a mismatch is caught before bind, not at
/// query time.
///
/// # Errors
/// Returns [`StorageDirMismatch`] when either dir differs.
pub fn check_storage_dirs_match(
    server: &StorageDirs,
    cli: &StorageDirs,
) -> Result<(), StorageDirMismatch> {
    if server == cli {
        return Ok(());
    }
    Err(StorageDirMismatch {
        server_artifacts: server.artifacts_dir.display().to_string(),
        server_market: server.market_dir.display().to_string(),
        cli_artifacts: cli.artifacts_dir.display().to_string(),
        cli_market: cli.market_dir.display().to_string(),
    })
}

/// Apply the **deprecated** `QE_SERVER_*` storage overrides (if present) to the qe-config-resolved
/// `base`, returning the server's read-state dirs. A set override emits a deprecation `warn!`; the boot
/// guard then rejects any override that diverges from `base`, so an override can only ever agree.
///
/// Pure over its explicit `Option` inputs so it is unit-testable; [`server_storage_dirs`] is the
/// env-reading + logging wrapper.
#[must_use]
pub fn apply_server_overrides(
    base: &StorageDirs,
    artifacts_override: Option<PathBuf>,
    market_override: Option<PathBuf>,
) -> StorageDirs {
    StorageDirs {
        artifacts_dir: artifacts_override.unwrap_or_else(|| base.artifacts_dir.clone()),
        market_dir: market_override.unwrap_or_else(|| base.market_dir.clone()),
    }
}

/// Resolve the server's read-state dirs from the qe-config `base`, honouring (and warning on) the
/// deprecated `QE_SERVER_ARTIFACTS_DIR`/`QE_SERVER_MARKET_DIR` overrides.
#[must_use]
pub fn server_storage_dirs(base: &StorageDirs) -> StorageDirs {
    let artifacts_override = std::env::var(ENV_ARTIFACTS_DIR).ok();
    let market_override = std::env::var(ENV_MARKET_DIR).ok();
    if artifacts_override.is_some() {
        tracing::warn!(
            "{ENV_ARTIFACTS_DIR} is deprecated (QE-419) — set config.toml [storage].artifacts_dir \
             (or QE_STORAGE__ARTIFACTS_DIR) instead; a value diverging from qe-config now refuses boot"
        );
    }
    if market_override.is_some() {
        tracing::warn!(
            "{ENV_MARKET_DIR} is deprecated (QE-419) — set config.toml [storage].market_dir \
             (or QE_STORAGE__MARKET_DIR) instead; a value diverging from qe-config now refuses boot"
        );
    }
    apply_server_overrides(
        base,
        artifacts_override.map(PathBuf::from),
        market_override.map(PathBuf::from),
    )
}

/// Resolve the `qe-config` path the server (and, via the pin, the spawned CLI) uses: [`ENV_CONFIG`] or
/// the [`DEFAULT_CONFIG_PATH`] default — identical to the CLI's own resolution.
#[must_use]
pub fn resolve_config_path() -> PathBuf {
    std::env::var(ENV_CONFIG)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH))
}

/// Load `qe-config` for the server, using the same `RuntimeSim` profile the spawned CLI's backtest
/// path uses (`crates/cli/src/main.rs`), so the resolved `[storage]` dirs match the CLI's exactly.
///
/// # Errors
/// Returns [`qe_config::ConfigError`] if the config cannot be read/parsed or fails validation — fatal
/// at boot, because the spawned CLI would fail identically.
pub fn load_app_config(path: &Path) -> Result<Config, qe_config::ConfigError> {
    Config::load(Profile::RuntimeSim, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(artifacts: &str, market: &str) -> StorageDirs {
        StorageDirs {
            artifacts_dir: PathBuf::from(artifacts),
            market_dir: PathBuf::from(market),
        }
    }

    #[test]
    fn from_config_maps_storage_section_verbatim() {
        // The unification: server-resolved dirs come straight from qe-config `[storage]`, so they equal
        // what the spawned CLI reads from the same source.
        let cfg = Config::from_toml_str(
            r#"
instruments = ["BTCUSDT"]
[storage]
market_dir = "vol/market"
synthetic_dir = "vol/synthetic"
artifacts_dir = "vol/artifacts"
"#,
        )
        .expect("valid config");
        let d = StorageDirs::from_config(&cfg);
        assert_eq!(d.market_dir, PathBuf::from("vol/market"));
        assert_eq!(d.artifacts_dir, PathBuf::from("vol/artifacts"));
    }

    #[test]
    fn matching_dirs_pass_the_guard() {
        let d = dirs("data/artifacts", "data/lmdb/market");
        assert!(check_storage_dirs_match(&d, &d.clone()).is_ok());
    }

    #[test]
    fn divergent_market_dir_is_detected() {
        let server = dirs("data/artifacts", "/vol/a");
        let cli = dirs("data/artifacts", "/vol/b");
        let err = check_storage_dirs_match(&server, &cli).expect_err("divergent market must error");
        assert_eq!(err.server_market, "/vol/a");
        assert_eq!(err.cli_market, "/vol/b");
    }

    #[test]
    fn divergent_artifacts_dir_is_detected() {
        let server = dirs("/vol/a", "data/lmdb/market");
        let cli = dirs("/vol/b", "data/lmdb/market");
        assert!(check_storage_dirs_match(&server, &cli).is_err());
    }

    #[test]
    fn no_override_leaves_dirs_equal_to_config() {
        // Without a deprecated override the server's dirs equal the qe-config dirs, so the guard passes.
        let base = dirs("data/artifacts", "data/lmdb/market");
        let server = apply_server_overrides(&base, None, None);
        assert_eq!(server, base);
        assert!(check_storage_dirs_match(&server, &base).is_ok());
    }

    #[test]
    fn divergent_override_is_rejected_by_the_guard() {
        // A deprecated QE_SERVER_MARKET_DIR override that diverges from qe-config is exactly the silent
        // misconfiguration QE-419 closes — the guard rejects it at boot.
        let base = dirs("data/artifacts", "data/lmdb/market");
        let server = apply_server_overrides(&base, None, Some(PathBuf::from("/somewhere/else")));
        assert_ne!(server, base);
        assert!(check_storage_dirs_match(&server, &base).is_err());
    }

    #[test]
    fn agreeing_override_passes_the_guard() {
        // An override equal to qe-config is redundant but harmless — it agrees, so boot proceeds.
        let base = dirs("data/artifacts", "data/lmdb/market");
        let server = apply_server_overrides(
            &base,
            Some(PathBuf::from("data/artifacts")),
            Some(PathBuf::from("data/lmdb/market")),
        );
        assert_eq!(server, base);
        assert!(check_storage_dirs_match(&server, &base).is_ok());
    }
}
