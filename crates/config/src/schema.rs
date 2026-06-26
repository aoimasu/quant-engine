//! Typed configuration schema.
//!
//! Representative-but-minimal: later tickets extend these structs. The durable contract is that
//! the config is built from `Vec`/scalar fields only (no `HashMap`) so serialisation — and thus
//! [`crate::Config::content_hash`] — is deterministic across runs and machines.

use serde::{Deserialize, Serialize};

/// Operating profile. Selects which pipeline the config drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// Offline training pipeline (Diagram A).
    Train,
    /// Runtime pipeline against the simulator / paper mode.
    RuntimeSim,
    /// Runtime pipeline against live venue + capital.
    RuntimeLive,
}

impl Profile {
    /// Kebab-case string form, matching the serde representation and overlay-file suffix.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Train => "train",
            Profile::RuntimeSim => "runtime-sim",
            Profile::RuntimeLive => "runtime-live",
        }
    }
}

/// Multi-resolution bar settings: a base resolution plus coarser reconstructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarsConfig {
    /// Finest ingested resolution (e.g. `5m`).
    pub base: String,
    /// Coarser resolutions reconstructed from `base` (e.g. `30m`, `4h`).
    #[serde(default)]
    pub reconstructed: Vec<String>,
}

impl Default for BarsConfig {
    fn default() -> Self {
        Self {
            base: "5m".to_owned(),
            reconstructed: vec!["30m".to_owned(), "4h".to_owned()],
        }
    }
}

/// History-window settings for the training corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryConfig {
    /// Use each instrument's full point-in-time history from listing.
    #[serde(default = "default_true")]
    pub max_available: bool,
    /// Explicit ISO start date; required when `max_available` is false.
    #[serde(default)]
    pub start: Option<String>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_available: true,
            start: None,
        }
    }
}

/// Storage directories. Relative + volume-friendly (no hard-coded absolutes), per QE-013.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    /// LMDB market-data store directory.
    pub market_dir: String,
    /// LMDB synthetic-data (indicator/bars) store directory.
    pub synthetic_dir: String,
    /// Vintage artefact output directory.
    pub artifacts_dir: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            market_dir: "data/lmdb/market".to_owned(),
            synthetic_dir: "data/lmdb/synthetic".to_owned(),
            artifacts_dir: "data/artifacts".to_owned(),
        }
    }
}

/// Determinism settings; the seed is plumbed through all stochastic stages (QE-006).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeterminismConfig {
    /// Master RNG seed.
    pub seed: u64,
}

/// Top-level resolved configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Operating profile.
    #[serde(default = "default_profile")]
    pub profile: Profile,
    /// Instrument universe (default `BTCUSDT`, `ETHUSDT`).
    #[serde(default = "default_instruments")]
    pub instruments: Vec<String>,
    /// Multi-resolution bar settings.
    #[serde(default)]
    pub bars: BarsConfig,
    /// History-window settings.
    #[serde(default)]
    pub history: HistoryConfig,
    /// Storage directories.
    #[serde(default)]
    pub storage: StorageConfig,
    /// Determinism settings.
    #[serde(default)]
    pub determinism: DeterminismConfig,
}

fn default_true() -> bool {
    true
}

fn default_profile() -> Profile {
    Profile::Train
}

fn default_instruments() -> Vec<String> {
    vec!["BTCUSDT".to_owned(), "ETHUSDT".to_owned()]
}

/// Resolution → minutes, for the known resolution ladder. `None` for unknown strings.
pub(crate) fn resolution_minutes(res: &str) -> Option<u64> {
    match res {
        "1m" => Some(1),
        "5m" => Some(5),
        "15m" => Some(15),
        "30m" => Some(30),
        "1h" => Some(60),
        "4h" => Some(240),
        "1d" => Some(1440),
        _ => None,
    }
}
