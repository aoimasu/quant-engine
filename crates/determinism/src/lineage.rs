//! Vintage lineage records.
//!
//! Every produced artefact must carry a resolvable lineage record so a vintage can be audited and
//! reproduced months later. A [`Lineage`] binds the four inputs that fully determine a stage's
//! output: the config content hash (QE-002), the input-data snapshot id, the code commit, and the
//! RNG seeds. Its [`Lineage::id`] is a stable hash usable as a primary key — i.e. *resolvable*.
//!
//! # Universe provenance is captured (QE-448)
//!
//! A vintage must be traceable to a **survivorship-safe, point-in-time universe** (QE-012). It already
//! is: [`Lineage::from_config`] folds in [`qe_config::Config::content_hash`], which SHA-256s the
//! **entire** `Config` — including both the flat `instruments` roster **and** the point-in-time
//! `universe` (each member's instrument id plus its `[listed, delisted)` window). So the exact roster
//! *and* every listing/delisting date live inside `config_hash`, hence inside [`Lineage::id`]. Change
//! the roster, a listing date, or a delisting date and the vintage's resolvable id changes — a vintage
//! is bound to the specific survivorship-safe universe it was trained on. No separate universe field is
//! needed on the lineage record (see `universe_membership_changes_the_lineage_id`).

use qe_config::{Config, ConfigError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The provenance of an artefact: everything needed to reproduce it bit-for-bit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    /// SHA-256 content hash of the resolved config (`qe_config::Config::content_hash`).
    pub config_hash: String,
    /// Identifier of the immutable input-data snapshot the stage consumed.
    pub input_snapshot_id: String,
    /// Code provenance — typically the git commit SHA the binary was built from.
    pub code_commit: String,
    /// Master RNG seed(s) plumbed into the stage's stochastic steps.
    pub seeds: Vec<u64>,
}

impl Lineage {
    /// Construct a lineage from already-resolved parts.
    pub fn new(
        config_hash: impl Into<String>,
        input_snapshot_id: impl Into<String>,
        code_commit: impl Into<String>,
        seeds: Vec<u64>,
    ) -> Self {
        Self {
            config_hash: config_hash.into(),
            input_snapshot_id: input_snapshot_id.into(),
            code_commit: code_commit.into(),
            seeds,
        }
    }

    /// Build a lineage from a resolved config, folding in QE-002's content hash.
    ///
    /// # Errors
    /// Propagates [`ConfigError`] if the config cannot be serialised for hashing.
    pub fn from_config(
        config: &Config,
        input_snapshot_id: impl Into<String>,
        code_commit: impl Into<String>,
        seeds: Vec<u64>,
    ) -> Result<Self, ConfigError> {
        Ok(Self::new(
            config.content_hash()?,
            input_snapshot_id,
            code_commit,
            seeds,
        ))
    }

    /// Stable lineage id: lowercase-hex SHA-256 over the record's canonical JSON.
    ///
    /// Deterministic across runs and machines — the struct's field order is fixed and `seeds` is an
    /// ordered `Vec`, so the JSON encoding is byte-stable. Suitable as an artefact primary key.
    ///
    /// # Errors
    /// Returns [`LineageError::Serialize`] if the record cannot be serialised.
    pub fn id(&self) -> Result<String, LineageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| LineageError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }
}

/// A value tagged with the lineage that produced it.
///
/// Models AC #2 — "every produced artefact carries a resolvable lineage record": from any
/// `Artifact` you can reach [`HasLineage::lineage`] and resolve its [`Lineage::id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact<T> {
    /// The produced value.
    pub value: T,
    /// The lineage that produced `value`.
    pub lineage: Lineage,
}

impl<T> Artifact<T> {
    /// Tag `value` with its `lineage`.
    pub fn new(value: T, lineage: Lineage) -> Self {
        Self { value, lineage }
    }
}

/// Anything carrying a resolvable [`Lineage`].
pub trait HasLineage {
    /// The lineage that produced this artefact.
    fn lineage(&self) -> &Lineage;
}

impl<T> HasLineage for Artifact<T> {
    fn lineage(&self) -> &Lineage {
        &self.lineage
    }
}

/// Errors raised while hashing a [`Lineage`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LineageError {
    /// The lineage record could not be serialised for hashing.
    #[error("failed to serialise lineage for hashing: {0}")]
    Serialize(String),
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_64_hex_chars() {
        let a = Lineage::new("cfg", "snap-1", "commit", vec![42]);
        let b = Lineage::new("cfg", "snap-1", "commit", vec![42]);
        assert_eq!(a.id().unwrap(), b.id().unwrap());
        assert_eq!(a.id().unwrap().len(), 64);
        assert!(a.id().unwrap().bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn id_is_sensitive_to_every_field() {
        let base = Lineage::new("cfg", "snap-1", "commit", vec![42]);
        let id = base.id().unwrap();
        assert_ne!(
            id,
            Lineage::new("CFG", "snap-1", "commit", vec![42])
                .id()
                .unwrap()
        );
        assert_ne!(
            id,
            Lineage::new("cfg", "snap-2", "commit", vec![42])
                .id()
                .unwrap()
        );
        assert_ne!(
            id,
            Lineage::new("cfg", "snap-1", "other", vec![42])
                .id()
                .unwrap()
        );
        assert_ne!(
            id,
            Lineage::new("cfg", "snap-1", "commit", vec![43])
                .id()
                .unwrap()
        );
    }

    #[test]
    fn from_config_uses_content_hash() {
        let cfg = Config::from_toml_str("").expect("defaults are valid");
        let lin = Lineage::from_config(&cfg, "snap", "commit", vec![cfg.determinism.seed]).unwrap();
        assert_eq!(lin.config_hash, cfg.content_hash().unwrap());
    }

    #[test]
    fn universe_membership_changes_the_lineage_id() {
        // QE-448: the point-in-time / survivorship-safe universe (QE-012) is captured in the vintage
        // lineage SHA via `Config::content_hash`. Two configs differing ONLY in a `[[universe]]`
        // delisting date must produce different lineage ids — so a vintage is bound to the exact
        // survivorship-safe roster it was trained on.
        let base = "\
[[universe]]
instrument = \"BTCUSDT\"
listed = \"2019-09-08\"

[[universe]]
instrument = \"LUNAUSDT\"
listed = \"2020-01-01\"
delisted = \"2022-05-13\"
";
        // Same roster, but LUNA's delisting date moved — a different survivorship window.
        let moved = base.replace("2022-05-13", "2022-06-13");

        let cfg_a = Config::from_toml_str(base).expect("valid universe config");
        let cfg_b = Config::from_toml_str(&moved).expect("valid universe config");

        let lin_a = Lineage::from_config(&cfg_a, "snap", "commit", vec![7]).unwrap();
        let lin_b = Lineage::from_config(&cfg_b, "snap", "commit", vec![7]).unwrap();

        assert_ne!(
            lin_a.config_hash, lin_b.config_hash,
            "a moved delisting date must change config_hash (universe rides the lineage)"
        );
        assert_ne!(
            lin_a.id().unwrap(),
            lin_b.id().unwrap(),
            "a moved delisting date must change the resolvable lineage id"
        );
    }

    #[test]
    fn artifact_exposes_resolvable_lineage() {
        let lin = Lineage::new("cfg", "snap", "commit", vec![1, 2]);
        let id = lin.id().unwrap();
        let art = Artifact::new(vec![0u8; 3], lin);
        assert_eq!(art.lineage().id().unwrap(), id);
        assert_eq!(art.value, vec![0u8; 3]);
    }
}
