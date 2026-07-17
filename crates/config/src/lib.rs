//! qe-config — typed, layered, reproducible configuration.
//!
//! Loading merges, in increasing precedence: a base TOML file, an optional profile overlay file
//! (`<stem>.<profile>.<ext>` next to the base), then `QE_`-prefixed environment overrides (nested
//! via `__`). The requested profile is authoritative over whatever the files contain. The resolved
//! [`Config`] is validated at load (fail-fast with field-level errors) and exposes a stable
//! [`Config::content_hash`] for vintage lineage.

mod error;
mod schema;
pub mod universe;

pub use error::ConfigError;
pub use schema::{
    BarsConfig, Config, DeterminismConfig, HistoryConfig, Profile, SelectionConfig, StorageConfig,
    UniverseMemberConfig,
};
pub use universe::{InstrumentListing, Universe};

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use qe_domain::InstrumentId;
use schema::resolution_minutes;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use universe::parse_iso_date;

impl Config {
    /// Load config for a given profile: base TOML file, then an optional `<stem>.<profile>.<ext>`
    /// overlay next to it, then `QE_`-prefixed environment overrides; finally validate.
    ///
    /// The requested `profile` is forced onto the resolved config (authoritative over file
    /// contents), so `train`/`runtime-sim`/`runtime-live` are genuinely separate configurations.
    /// A missing overlay file is simply skipped.
    ///
    /// # Errors
    /// Returns [`ConfigError::Load`] if sources cannot be read/parsed, or [`ConfigError::Invalid`]
    /// if a field fails validation.
    pub fn load(profile: Profile, base_path: &Path) -> Result<Self, ConfigError> {
        let mut fig = Figment::new().merge(Toml::file(base_path));
        if let Some(overlay) = profile_overlay_path(base_path, profile) {
            fig = fig.merge(Toml::file(overlay));
        }
        let mut cfg: Self = fig
            .merge(Env::prefixed("QE_").split("__"))
            .extract()
            .map_err(|e| ConfigError::Load(e.to_string()))?;
        cfg.profile = profile; // requested profile wins over file contents
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
        let mut seen_instruments = BTreeSet::new();
        for (i, sym) in self.instruments.iter().enumerate() {
            let field = format!("instruments[{i}]");
            if sym.trim().is_empty() {
                return Err(invalid(&field, "must not be blank"));
            }
            if !seen_instruments.insert(sym) {
                return Err(invalid(&field, &format!("duplicate instrument '{sym}'")));
            }
        }

        let base = resolution_minutes(&self.bars.base).ok_or_else(|| {
            invalid(
                "bars.base",
                &format!("unknown resolution '{}'", self.bars.base),
            )
        })?;
        let mut seen_res = BTreeSet::new();
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
            if !seen_res.insert(r) {
                return Err(invalid(&field, &format!("duplicate resolution '{r}'")));
            }
        }

        match &self.history.start {
            None if !self.history.max_available => {
                return Err(invalid(
                    "history.start",
                    "required when `max_available = false`",
                ));
            }
            // Validate through the same strict calendar parser as the universe (leap/per-month aware),
            // so `history.start` and `[[universe]]` dates share one definition of "valid ISO date".
            Some(start) => {
                parse_iso_date(start).map_err(|e| {
                    invalid(
                        "history.start",
                        &format!("'{start}' is not a valid ISO `YYYY-MM-DD` date: {e}"),
                    )
                })?;
            }
            None => {}
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

        // Building the universe validates the `[[universe]]` section (ids, dates, ordering, dups)
        // with dotted field paths — fail-fast at load like every other field.
        self.universe()?;

        // QE-403: the funding-coverage floor is a fraction.
        let f = self.selection.funding_coverage_min;
        if !f.is_finite() || !(0.0..=1.0).contains(&f) {
            return Err(invalid(
                "selection.funding_coverage_min",
                "must be a fraction in [0.0, 1.0]",
            ));
        }

        // QE-415: the selection needs ≥ 2 CV folds for a real cross-validated standard error.
        if self.selection.cv_folds < 2 {
            return Err(invalid(
                "selection.cv_folds",
                "must be at least 2 (fold-isolation cross-validation needs ≥ 2 folds)",
            ));
        }

        // QE-443: the inverse-vol seed's EWMA decay constant λ must be a strict fraction in (0, 1).
        let d = self.selection.ewma_decay;
        if !(d.is_finite() && 0.0 < d && d < 1.0) {
            return Err(invalid(
                "selection.ewma_decay",
                "must be a decay constant in the open interval (0.0, 1.0)",
            ));
        }

        Ok(())
    }

    /// Resolve the point-in-time [`Universe`] from this config.
    ///
    /// Prefers the `[[universe]]` section when present; otherwise falls back to the flat
    /// `instruments` list as an **open-ended** universe (every instrument always a member), so
    /// existing date-less configs keep working. Count-agnostic: one instrument or many.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] (with a dotted `universe[i].field` path) if an instrument id is
    /// invalid, a date is malformed, `delisted < listed`, or an instrument appears twice.
    pub fn universe(&self) -> Result<Universe, ConfigError> {
        if self.universe.is_empty() {
            // Fallback: the flat list (already non-empty + dup-checked above) as open-ended listings.
            let listings = self
                .instruments
                .iter()
                .enumerate()
                .map(|(i, sym)| {
                    let id = InstrumentId::new(sym)
                        .map_err(|e| invalid(&format!("instruments[{i}]"), &e.to_string()))?;
                    Ok(InstrumentListing::open_ended(id))
                })
                .collect::<Result<Vec<_>, ConfigError>>()?;
            return Ok(Universe::new(listings));
        }

        let mut listings = Vec::with_capacity(self.universe.len());
        let mut seen = BTreeSet::new();
        for (i, m) in self.universe.iter().enumerate() {
            let id = InstrumentId::new(&m.instrument)
                .map_err(|e| invalid(&format!("universe[{i}].instrument"), &e.to_string()))?;
            if !seen.insert(id.as_str().to_owned()) {
                return Err(invalid(
                    &format!("universe[{i}].instrument"),
                    &format!("duplicate instrument '{}'", id.as_str()),
                ));
            }
            // Omitted `listed` → open-ended (listed since forever).
            let listed = match &m.listed {
                Some(s) => {
                    parse_iso_date(s).map_err(|e| invalid(&format!("universe[{i}].listed"), e))?
                }
                None => universe::OPEN_LISTING,
            };
            let delisted = match &m.delisted {
                Some(s) => Some(
                    parse_iso_date(s)
                        .map_err(|e| invalid(&format!("universe[{i}].delisted"), e))?,
                ),
                None => None,
            };
            let listing = InstrumentListing::new(id, listed, delisted)
                .map_err(|e| invalid(&format!("universe[{i}].delisted"), e))?;
            listings.push(listing);
        }
        Ok(Universe::new(listings))
    }
}

/// Path to the optional profile overlay file: `<dir>/<stem>.<profile>.<ext>` next to `base_path`.
fn profile_overlay_path(base_path: &Path, profile: Profile) -> Option<PathBuf> {
    let stem = base_path.file_stem()?.to_str()?;
    let ext = base_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("toml");
    let parent = base_path.parent().unwrap_or_else(|| Path::new(""));
    Some(parent.join(format!("{stem}.{}.{ext}", profile.as_str())))
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
    fn funding_coverage_min_defaults_and_validates() {
        // Default is the sensible 0.90 floor.
        let cfg = Config::from_toml_str(VALID).unwrap();
        assert!((cfg.selection.funding_coverage_min - 0.90).abs() < 1e-12);

        // An explicit in-range override is accepted.
        let ok = format!("{VALID}\n[selection]\nfunding_coverage_min = 0.5\n");
        assert!(
            (Config::from_toml_str(&ok)
                .unwrap()
                .selection
                .funding_coverage_min
                - 0.5)
                .abs()
                < 1e-12
        );

        // Out of range is rejected with a dotted field path.
        let bad = format!("{VALID}\n[selection]\nfunding_coverage_min = 1.5\n");
        let err = Config::from_toml_str(&bad).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { field, .. } if field == "selection.funding_coverage_min")
        );
    }

    #[test]
    fn inverse_vol_seed_defaults_off_and_decay_validates() {
        // QE-443: the inverse-vol seed is OPT-IN — default OFF, so vintages/goldens are unchanged.
        let cfg = Config::from_toml_str(VALID).unwrap();
        assert!(!cfg.selection.inverse_vol_seed, "seed must default OFF");
        assert!((cfg.selection.ewma_decay - 0.94).abs() < 1e-12);

        // Explicit opt-in + in-range decay is accepted.
        let ok = format!("{VALID}\n[selection]\ninverse_vol_seed = true\newma_decay = 0.97\n");
        let c = Config::from_toml_str(&ok).unwrap();
        assert!(c.selection.inverse_vol_seed);
        assert!((c.selection.ewma_decay - 0.97).abs() < 1e-12);

        // A decay outside (0, 1) is rejected with a dotted field path.
        for bad_decay in ["0.0", "1.0", "1.5"] {
            let bad = format!("{VALID}\n[selection]\newma_decay = {bad_decay}\n");
            let err = Config::from_toml_str(&bad).unwrap_err();
            assert!(
                matches!(err, ConfigError::Invalid { field, .. } if field == "selection.ewma_decay"),
                "decay {bad_decay} must be rejected"
            );
        }
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

    #[test]
    fn rejects_malformed_start_date() {
        let toml = format!("{VALID}\n[history]\nmax_available = false\nstart = \"banana\"\n");
        let err = Config::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "history.start"));
    }

    #[test]
    fn accepts_valid_start_date() {
        let toml = format!("{VALID}\n[history]\nmax_available = false\nstart = \"2019-09-08\"\n");
        let cfg = Config::from_toml_str(&toml).expect("valid ISO date accepted");
        assert_eq!(cfg.history.start.as_deref(), Some("2019-09-08"));
    }

    #[test]
    fn rejects_blank_instrument() {
        let toml = VALID.replace(
            r#"instruments = ["BTCUSDT", "ETHUSDT"]"#,
            r#"instruments = ["BTCUSDT", ""]"#,
        );
        let err = Config::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "instruments[1]"));
    }

    #[test]
    fn rejects_duplicate_instruments() {
        let toml = VALID.replace(
            r#"instruments = ["BTCUSDT", "ETHUSDT"]"#,
            r#"instruments = ["BTCUSDT", "BTCUSDT"]"#,
        );
        let err = Config::from_toml_str(&toml).unwrap_err();
        match err {
            ConfigError::Invalid { field, message } => {
                assert_eq!(field, "instruments[1]");
                assert!(message.contains("duplicate"), "msg: {message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_reconstructed() {
        let toml = VALID.replace(
            r#"reconstructed = ["30m", "4h"]"#,
            r#"reconstructed = ["30m", "30m"]"#,
        );
        let err = Config::from_toml_str(&toml).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { field, .. } if field == "bars.reconstructed[1]")
        );
    }

    #[test]
    fn history_start_uses_strict_calendar_validation() {
        // After consolidating onto `parse_iso_date`, `history.start` rejects calendar-invalid dates
        // that the old lightweight check would have passed (e.g. Feb 29 on a non-leap year).
        let bad = format!("{VALID}\n[history]\nmax_available = false\nstart = \"2021-02-29\"\n");
        let err = Config::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "history.start"));

        // A real leap day is accepted.
        let ok = format!("{VALID}\n[history]\nmax_available = false\nstart = \"2020-02-29\"\n");
        assert!(Config::from_toml_str(&ok).is_ok());
    }
}
