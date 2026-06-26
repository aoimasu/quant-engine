//! qe-config — typed, layered, reproducible configuration.
//!
//! Loading merges, in increasing precedence: a base TOML file, then `QE_`-prefixed environment
//! overrides (nested via `__`). The resolved [`Config`] is validated at load (fail-fast with
//! field-level errors) and exposes a stable [`Config::content_hash`] for vintage lineage.

mod error;
mod schema;

pub use error::ConfigError;
pub use schema::{BarsConfig, Config, DeterminismConfig, HistoryConfig, Profile, StorageConfig};

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use schema::resolution_minutes;
use sha2::{Digest, Sha256};
use std::path::Path;

impl Config {
    /// Load config from a base TOML file plus `QE_`-prefixed environment overrides, then validate.
    ///
    /// # Errors
    /// Returns [`ConfigError::Load`] if sources cannot be read/parsed, or [`ConfigError::Invalid`]
    /// if a field fails validation.
    pub fn load(base_path: &Path) -> Result<Self, ConfigError> {
        let cfg: Self = Figment::new()
            .merge(Toml::file(base_path))
            .merge(Env::prefixed("QE_").split("__"))
            .extract()
            .map_err(|e| ConfigError::Load(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse + validate config from a TOML string (no filesystem; mainly for tests/embedding).
    ///
    /// # Errors
    /// As [`Config::load`].
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let cfg: Self = Figment::new()
            .merge(Toml::string(s))
            .extract()
            .map_err(|e| ConfigError::Load(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Stable content hash (lowercase hex SHA-256) of the resolved config.
    ///
    /// Deterministic across runs and machines: the schema is `Vec`/scalar only, so JSON
    /// serialisation is order-stable. Folded into vintage lineage by QE-006/QE-129.
    ///
    /// # Errors
    /// Returns [`ConfigError::Serialize`] if the config cannot be serialised.
    pub fn content_hash(&self) -> Result<String, ConfigError> {
        let bytes = serde_json::to_vec(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let digest = Sha256::digest(&bytes);
        Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Validate the resolved config, returning a field-level error on the first problem.
    ///
    /// # Errors
    /// Returns [`ConfigError::Invalid`] naming the offending dotted field path.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.instruments.is_empty() {
            return Err(invalid("instruments", "must list at least one instrument"));
        }

        let base = resolution_minutes(&self.bars.base).ok_or_else(|| {
            invalid(
                "bars.base",
                &format!("unknown resolution '{}'", self.bars.base),
            )
        })?;
        for (i, r) in self.bars.reconstructed.iter().enumerate() {
            let field = format!("bars.reconstructed[{i}]");
            let m = resolution_minutes(r)
                .ok_or_else(|| invalid(&field, &format!("unknown resolution '{r}'")))?;
            if m <= base {
                return Err(invalid(
                    &field,
                    &format!(
                        "'{r}' must be strictly coarser than base '{}'",
                        self.bars.base
                    ),
                ));
            }
        }

        if !self.history.max_available && self.history.start.is_none() {
            return Err(invalid(
                "history.start",
                "required when `max_available = false`",
            ));
        }

        for (field, val) in [
            ("storage.market_dir", &self.storage.market_dir),
            ("storage.synthetic_dir", &self.storage.synthetic_dir),
            ("storage.artifacts_dir", &self.storage.artifacts_dir),
        ] {
            if val.trim().is_empty() {
                return Err(invalid(field, "must not be empty"));
            }
        }

        Ok(())
    }
}

fn invalid(field: &str, message: &str) -> ConfigError {
    ConfigError::Invalid {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
profile = "train"
instruments = ["BTCUSDT", "ETHUSDT"]

[bars]
base = "5m"
reconstructed = ["30m", "4h"]

[storage]
market_dir = "data/lmdb/market"
synthetic_dir = "data/lmdb/synthetic"
artifacts_dir = "data/artifacts"

[determinism]
seed = 42
"#;

    #[test]
    fn parses_valid_config() {
        let cfg = Config::from_toml_str(VALID).expect("valid config");
        assert_eq!(cfg.profile, Profile::Train);
        assert_eq!(cfg.bars.base, "5m");
        assert_eq!(cfg.determinism.seed, 42);
    }

    #[test]
    fn defaults_apply_for_minimal_config() {
        // An empty document leaves every field to its serde default.
        let cfg = Config::from_toml_str("").expect("empty doc uses all defaults");
        assert_eq!(cfg.profile, Profile::Train);
        assert_eq!(cfg.instruments, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(cfg.bars.reconstructed, vec!["30m", "4h"]);
        assert!(cfg.history.max_available);
        assert_eq!(cfg.determinism.seed, 0);
    }

    #[test]
    fn hash_is_stable_across_loads() {
        let a = Config::from_toml_str(VALID).unwrap();
        let b = Config::from_toml_str(VALID).unwrap();
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
        assert_eq!(a.content_hash().unwrap().len(), 64); // sha256 hex
    }

    #[test]
    fn hash_changes_with_content() {
        let a = Config::from_toml_str(VALID).unwrap();
        let other = VALID.replace("seed = 42", "seed = 43");
        let b = Config::from_toml_str(&other).unwrap();
        assert_ne!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn rejects_unknown_base_resolution() {
        let toml = VALID.replace(r#"base = "5m""#, r#"base = "7m""#);
        let err = Config::from_toml_str(&toml).unwrap_err();
        match err {
            ConfigError::Invalid { field, .. } => assert_eq!(field, "bars.base"),
            other => panic!("expected Invalid(bars.base), got {other:?}"),
        }
    }

    #[test]
    fn rejects_reconstructed_not_coarser_than_base() {
        let toml = VALID.replace(
            r#"reconstructed = ["30m", "4h"]"#,
            r#"reconstructed = ["5m"]"#,
        );
        let err = Config::from_toml_str(&toml).unwrap_err();
        match err {
            ConfigError::Invalid { field, message } => {
                assert_eq!(field, "bars.reconstructed[0]");
                assert!(message.contains("strictly coarser"), "msg: {message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_instruments() {
        let toml = VALID.replace(
            r#"instruments = ["BTCUSDT", "ETHUSDT"]"#,
            "instruments = []",
        );
        let err = Config::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "instruments"));
    }

    #[test]
    fn rejects_missing_start_when_not_max_available() {
        let toml = format!("{VALID}\n[history]\nmax_available = false\n");
        let err = Config::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "history.start"));
    }
}
