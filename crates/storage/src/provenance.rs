//! Per-run data provenance recorded alongside the bars (QE-464).
//!
//! The market store must tell **real** bars from **synthetic** ones so nobody trains on generated
//! prices believing they are real. Provenance is stored **key-scannably** in a dedicated `provenance`
//! sub-database — a small index **separate** from the bars DB — so the bars coverage scan stays
//! byte-for-byte key-only (QE-412). Each ingest run records one [`ProvenanceSegment`] spanning the
//! `[first, last]` open-time of the bars it wrote; per-bar provenance is recoverable by locating the
//! segment whose range contains the bar.
//!
//! A single `(instrument, resolution)` may carry **multiple** contiguous segments (interleaved real +
//! synthetic) — always labelled, never a silent blend: the coverage query emits one row per segment.

use serde::{Deserialize, Serialize};

/// The origin of a run of bars: real market data, deterministic synthetic output, or legacy/untagged.
///
/// `Unknown` is the documented default for bars written before this ticket (no migration/guess — they
/// are recorded `unknown` until re-ingested).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Real market data (the QE-463 real ingest path).
    Real,
    /// Deterministic synthetic data (`qe ingest --synthetic`).
    Synthetic,
    /// Legacy / untagged bars present before provenance tagging existed.
    #[default]
    Unknown,
}

impl Provenance {
    /// Stable lowercase identifier for the wire / coverage row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Provenance::Real => "real",
            Provenance::Synthetic => "synthetic",
            Provenance::Unknown => "unknown",
        }
    }
}

/// Whether the tradability inputs (slippage / impact / ADV) were **measured** or left at defaults.
///
/// The QE-463 real klines-only slice is [`Calibration::Uncalibrated`]: no premium/impact/ADV inputs
/// are fabricated, so its default-calibrated tradability numbers must not read as measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Calibration {
    /// Slippage/impact/ADV calibrated from real measured inputs (not QE-463).
    Calibrated,
    /// Calibration left at its default — not measured (QE-463 klines-only; synthetic).
    #[default]
    Uncalibrated,
}

impl Calibration {
    /// Whether this segment's tradability inputs were measured.
    #[must_use]
    pub const fn is_calibrated(self) -> bool {
        matches!(self, Calibration::Calibrated)
    }
}

/// One recorded provenance run over a contiguous `[start, end]` open-time range of bars (inclusive on
/// both ends). Keyed in the `provenance` sub-DB by the range **start** (identical layout to a bar key),
/// so segments sort chronologically and are prefix-scannable per `(instrument, resolution)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceSegment {
    /// Latest bar open-time this run wrote, epoch-ms (inclusive). The start is carried in the key.
    pub end_ms: i64,
    /// Whether the run's bars are real, synthetic, or unknown.
    pub provenance: Provenance,
    /// Whether the run's tradability inputs were measured.
    pub calibration: Calibration,
}

/// The provenance make-up of a set of bars across the scanned segments (QE-464 / QE-467 headline).
///
/// Folds many [`ProvenanceSegment`]s into a single verdict the train path maps onto
/// `qe_vintage::DataProvenance`: an all-real store is `Real`, all-synthetic is `Synthetic`, any labelled
/// mix is `Mixed`, and a store with no tagged bars is `Empty`/`Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceSummary {
    /// No bars / no provenance segments were found.
    Empty,
    /// Every tagged segment is real.
    Real,
    /// Every tagged segment is synthetic.
    Synthetic,
    /// A labelled mix of real and synthetic segments.
    Mixed,
    /// Only legacy/untagged (`unknown`) segments were found.
    Unknown,
}

impl ProvenanceSummary {
    /// Fold a stream of segment provenances into a summary. Order-independent.
    #[must_use]
    pub fn from_provenances(items: impl IntoIterator<Item = Provenance>) -> Self {
        let mut real = false;
        let mut synthetic = false;
        let mut unknown = false;
        let mut any = false;
        for p in items {
            any = true;
            match p {
                Provenance::Real => real = true,
                Provenance::Synthetic => synthetic = true,
                Provenance::Unknown => unknown = true,
            }
        }
        match (any, real, synthetic, unknown) {
            (false, ..) => ProvenanceSummary::Empty,
            // Any labelled mix of real + synthetic is Mixed (unknown alongside does not un-mix it).
            (_, true, true, _) => ProvenanceSummary::Mixed,
            (_, true, false, false) => ProvenanceSummary::Real,
            (_, false, true, false) => ProvenanceSummary::Synthetic,
            // Real (or synthetic) interleaved with legacy unknown is still a labelled mix.
            (_, true, false, true) | (_, false, true, true) => ProvenanceSummary::Mixed,
            (_, false, false, _) => ProvenanceSummary::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_folds_real_synthetic_mixed_unknown_empty() {
        use Provenance::{Real, Synthetic, Unknown};
        assert_eq!(
            ProvenanceSummary::from_provenances([]),
            ProvenanceSummary::Empty
        );
        assert_eq!(
            ProvenanceSummary::from_provenances([Real, Real]),
            ProvenanceSummary::Real
        );
        assert_eq!(
            ProvenanceSummary::from_provenances([Synthetic]),
            ProvenanceSummary::Synthetic
        );
        assert_eq!(
            ProvenanceSummary::from_provenances([Real, Synthetic]),
            ProvenanceSummary::Mixed
        );
        assert_eq!(
            ProvenanceSummary::from_provenances([Unknown, Unknown]),
            ProvenanceSummary::Unknown
        );
        // Real interleaved with legacy unknown is a labelled mix, never silently Real.
        assert_eq!(
            ProvenanceSummary::from_provenances([Real, Unknown]),
            ProvenanceSummary::Mixed
        );
    }

    #[test]
    fn segment_round_trips() {
        let seg = ProvenanceSegment {
            end_ms: 42,
            provenance: Provenance::Synthetic,
            calibration: Calibration::Uncalibrated,
        };
        let json = serde_json::to_string(&seg).unwrap();
        let back: ProvenanceSegment = serde_json::from_str(&json).unwrap();
        assert_eq!(seg, back);
        assert_eq!(Provenance::Real.as_str(), "real");
        assert!(!Calibration::Uncalibrated.is_calibrated());
        assert!(Calibration::Calibrated.is_calibrated());
    }
}
