//! qe-vintage (QE-129) — the vintage artefact format.
//!
//! A **vintage** is the unit handed to runtime: the chromosomes (strategy genomes — `qe_wfo::Genome`,
//! QE-110/123), the ensemble (materialised as per-chromosome weights — the capacity-capped output of
//! QE-126/127/128), and the per-vintage calibration profile (`qe_risk::CalibrationProfile`, QE-116),
//! tagged with a resolvable [`Lineage`] (QE-006) and pinned by a **content hash**. The format is the
//! output of Area ⑦; it is read-only-loadable by runtime (QE-219), which is out of scope here.
//!
//! Being *downstream* of the search⟂portfolio firewall (QE-001/QE-132 govern information flow during
//! search/portfolio construction, not a final artefact recording their outputs), the vintage may bundle
//! both sides' data. It stores the ensemble as plain `weights`, not `qe_ensemble`'s search types, so the
//! artefact is pure data — runtime loads it without pulling in any search/portfolio logic.

use std::io::{Read, Write};
use std::path::PathBuf;

use qe_determinism::Lineage;
use qe_risk::CalibrationProfile;
use qe_wfo::Genome;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The vintage artefact format version. Part of the hashed content, so a format change changes the hash.
pub const VINTAGE_FORMAT_VERSION: u16 = 1;

/// The hashed content of a vintage — everything the content hash covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VintageContent {
    /// Artefact format version ([`VINTAGE_FORMAT_VERSION`]).
    pub format_version: u16,
    /// Human / rollover identifier for this vintage (e.g. a date-stamped label).
    pub vintage_id: String,
    /// The strategy genomes (chromosomes) the ensemble selected (QE-110/123).
    pub chromosomes: Vec<Genome>,
    /// Per-chromosome ensemble weight, aligned to `chromosomes` (capacity-capped, QE-126/127/128).
    pub weights: Vec<f64>,
    /// The per-vintage calibration sidecar (QE-116).
    pub calibration: CalibrationProfile,
    /// The lineage that produced this vintage (QE-006).
    pub lineage: Lineage,
}

impl VintageContent {
    /// Validate the artefact's structural invariants — `weights` aligned one-to-one with `chromosomes`
    /// and every weight finite. Called by [`Vintage::seal`], so a silent upstream bug (a non-finite
    /// weight that would serialise to JSON `null` and fail re-load, or a weight/chromosome length
    /// mismatch) surfaces as a clear error at seal time rather than a corrupt artefact.
    ///
    /// # Errors
    /// [`VintageError::WeightChromosomeMismatch`] or [`VintageError::NonFiniteWeight`].
    pub fn validate(&self) -> Result<(), VintageError> {
        if self.weights.len() != self.chromosomes.len() {
            return Err(VintageError::WeightChromosomeMismatch {
                weights: self.weights.len(),
                chromosomes: self.chromosomes.len(),
            });
        }
        for (index, &value) in self.weights.iter().enumerate() {
            if !value.is_finite() {
                return Err(VintageError::NonFiniteWeight { index, value });
            }
        }
        Ok(())
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (same pattern as
    /// [`Lineage::id`]). Stable because every embedded type serialises deterministically (fixed field
    /// order; `BTreeMap`-ordered calibration maps; no `HashMap`/`HashSet` anywhere in the embedded types).
    ///
    /// **Hashing contract:** the hash is the digest of `serde_json`'s output. Its stability therefore
    /// depends on (a) no map type with nondeterministic iteration order ever entering the hashed content,
    /// and (b) `serde_json`'s number/whitespace formatting. Any future field addition must preserve (a);
    /// a `serde_json` major bump that changed (b) would change every vintage hash (and so must bump
    /// [`VINTAGE_FORMAT_VERSION`]).
    ///
    /// # Errors
    /// [`VintageError::Serialize`] if the content cannot be serialised.
    pub fn content_hash(&self) -> Result<String, VintageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| VintageError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }
}

/// A sealed vintage artefact: its [`VintageContent`] plus the content hash that pins it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vintage {
    /// The hashed content.
    pub content: VintageContent,
    /// The content hash computed at [`seal`](Vintage::seal) time.
    pub content_hash: String,
}

impl Vintage {
    /// Seal `content` by [validating](VintageContent::validate) its invariants, then computing and
    /// pinning its content hash.
    ///
    /// # Errors
    /// [`VintageContent::validate`] errors (non-finite or misaligned weights), or a serialisation
    /// failure from [`VintageContent::content_hash`].
    pub fn seal(content: VintageContent) -> Result<Self, VintageError> {
        content.validate()?;
        let content_hash = content.content_hash()?;
        Ok(Vintage {
            content,
            content_hash,
        })
    }

    /// Verify the stored hash matches a freshly recomputed one — detects any post-seal tampering.
    ///
    /// # Errors
    /// [`VintageError::HashMismatch`] if the stored hash does not match, or a serialisation failure.
    pub fn verify(&self) -> Result<(), VintageError> {
        let recomputed = self.content.content_hash()?;
        if recomputed != self.content_hash {
            return Err(VintageError::HashMismatch {
                stored: self.content_hash.clone(),
                recomputed,
            });
        }
        Ok(())
    }

    /// Serialise the sealed artefact as JSON to `w`.
    ///
    /// # Errors
    /// [`VintageError::Serialize`] / [`VintageError::Io`] on failure.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), VintageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| VintageError::Serialize(e.to_string()))?;
        w.write_all(&bytes)?;
        Ok(())
    }

    /// Load a sealed artefact from a JSON reader, **verifying the content hash** before returning — a
    /// load never yields an unverified vintage.
    ///
    /// # Errors
    /// [`VintageError::Deserialize`] / [`VintageError::Io`] on read failure, [`VintageError::HashMismatch`]
    /// if the content hash does not verify.
    pub fn load<R: Read>(r: R) -> Result<Self, VintageError> {
        let vintage: Vintage =
            serde_json::from_reader(r).map_err(|e| VintageError::Deserialize(e.to_string()))?;
        vintage.verify()?;
        Ok(vintage)
    }
}

/// A directory-backed store of vintages (the ensemble/vintage repository, QE-129/D3): one
/// `<root>/<vintage_id>.json` per vintage. Runtime (QE-219) opens it read-only.
#[derive(Debug, Clone)]
pub struct VintageRepository {
    root: PathBuf,
}

impl VintageRepository {
    /// A repository rooted at `root` (created on first [`write`](VintageRepository::write)).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        VintageRepository { root: root.into() }
    }

    /// The on-disk path for `vintage_id`.
    #[must_use]
    pub fn path_for(&self, vintage_id: &str) -> PathBuf {
        self.root.join(format!("{vintage_id}.json"))
    }

    /// Write `vintage` to `<root>/<vintage_id>.json`, creating `root` if needed. Returns the path.
    ///
    /// # Errors
    /// [`VintageError::Io`] / [`VintageError::Serialize`] on failure.
    pub fn write(&self, vintage: &Vintage) -> Result<PathBuf, VintageError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_for(&vintage.content.vintage_id);
        let mut file = std::fs::File::create(&path)?;
        vintage.write(&mut file)?;
        Ok(path)
    }

    /// Load and verify the vintage `vintage_id` from disk.
    ///
    /// # Errors
    /// [`VintageError::Io`] if the file is missing/unreadable, plus the [`Vintage::load`] errors.
    pub fn load(&self, vintage_id: &str) -> Result<Vintage, VintageError> {
        let file = std::fs::File::open(self.path_for(vintage_id))?;
        Vintage::load(file)
    }
}

/// Errors raised while sealing / writing / loading a vintage.
#[derive(Debug, Error)]
pub enum VintageError {
    /// The artefact could not be serialised.
    #[error("failed to serialise vintage: {0}")]
    Serialize(String),
    /// The artefact could not be deserialised.
    #[error("failed to deserialise vintage: {0}")]
    Deserialize(String),
    /// The content hash did not verify (tampered or corrupted artefact).
    #[error("vintage content hash mismatch: stored {stored}, recomputed {recomputed}")]
    HashMismatch {
        /// The hash stored in the artefact.
        stored: String,
        /// The hash recomputed from the content.
        recomputed: String,
    },
    /// `weights` is not aligned one-to-one with `chromosomes`.
    #[error("vintage has {weights} weights for {chromosomes} chromosomes (must be aligned)")]
    WeightChromosomeMismatch {
        /// Number of weights supplied.
        weights: usize,
        /// Number of chromosomes supplied.
        chromosomes: usize,
    },
    /// A weight is not finite (would serialise to JSON `null` and fail re-load).
    #[error("vintage weight {index} is not finite: {value}")]
    NonFiniteWeight {
        /// Index of the offending weight.
        index: usize,
        /// The non-finite value.
        value: f64,
    },
    /// Underlying I/O error.
    #[error("vintage I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Lowercase-hex encoding of a byte slice.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_wfo::{Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION};
    use rust_decimal::Decimal;

    fn genome(hold: u16) -> Genome {
        let off = Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        };
        let mut clauses = [off; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 1,
            hi: 2,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn calibration() -> CalibrationProfile {
        CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()) // 0.2 ensemble fast-drop
    }

    fn lineage() -> Lineage {
        Lineage::new(
            "cfg-hash-abc",
            "snapshot-2024-06",
            "commit-deadbeef",
            vec![7, 42],
        )
    }

    fn content() -> VintageContent {
        VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "2024-06-vintage".to_string(),
            chromosomes: vec![genome(10), genome(25)],
            weights: vec![0.6, 0.4],
            calibration: calibration(),
            lineage: lineage(),
        }
    }

    #[test]
    fn round_trips_with_stable_verifiable_hash() {
        let sealed = Vintage::seal(content()).unwrap();

        // Write → load reproduces the vintage exactly, and the load verifies the hash.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded, sealed);
        assert_eq!(loaded.content_hash, sealed.content_hash);

        // The hash is stable: sealing the same content again yields the same hash.
        let resealed = Vintage::seal(content()).unwrap();
        assert_eq!(resealed.content_hash, sealed.content_hash);
        // … and it is non-empty hex (a real SHA-256).
        assert_eq!(sealed.content_hash.len(), 64);
        assert!(sealed.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tampering_with_content_fails_verification() {
        let mut sealed = Vintage::seal(content()).unwrap();
        // Mutate the content without re-sealing — the stored hash no longer matches.
        sealed.content.weights[0] = 0.99;
        let err = sealed.verify().unwrap_err();
        assert!(matches!(err, VintageError::HashMismatch { .. }));

        // And a load of the tampered bytes is rejected.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        assert!(matches!(
            Vintage::load(buf.as_slice()),
            Err(VintageError::HashMismatch { .. })
        ));
    }

    #[test]
    fn seal_rejects_non_finite_and_misaligned_weights() {
        // A non-finite weight would serialise to JSON `null` and fail re-load — caught at seal time.
        let mut bad = content();
        bad.weights[1] = f64::NAN;
        assert!(matches!(
            Vintage::seal(bad),
            Err(VintageError::NonFiniteWeight { index: 1, .. })
        ));

        // Weights must be aligned one-to-one with chromosomes.
        let mut misaligned = content();
        misaligned.weights.pop(); // 1 weight for 2 chromosomes
        assert!(matches!(
            Vintage::seal(misaligned),
            Err(VintageError::WeightChromosomeMismatch {
                weights: 1,
                chromosomes: 2,
            })
        ));
    }

    #[test]
    fn format_version_is_part_of_the_hash() {
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.format_version = VINTAGE_FORMAT_VERSION + 1;
        let bumped = Vintage::seal(other).unwrap();
        assert_ne!(bumped.content_hash, base.content_hash);
    }

    #[test]
    fn repository_round_trips_from_disk() {
        let dir = std::env::temp_dir().join(format!("qe-vintage-test-{}", std::process::id()));
        let repo = VintageRepository::new(&dir);
        let sealed = Vintage::seal(content()).unwrap();

        let path = repo.write(&sealed).unwrap();
        assert!(path.exists());
        let loaded = repo.load(&sealed.content.vintage_id).unwrap();
        assert_eq!(loaded, sealed);

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
