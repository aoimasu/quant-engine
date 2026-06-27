//! qe-gate (QE-134) — GATE G1: holdout embargo & over-fit acceptance.
//!
//! Phase 1 is "validated" only when a vintage clears an **untouched** time-blocked holdout that no
//! training/selection step was allowed to read. This crate provides the two halves of G1:
//!
//! - [`split_with_embargo`] carves the dataset into disjoint `train | embargo | holdout` time blocks, so
//!   the holdout is the final OOS slice and an embargo gap purges look-ahead leakage at the boundary.
//! - [`evaluate_g1`] applies the four pre-registered acceptance criteria on the holdout and records a
//!   [`G1Decision`] with per-criterion evidence: a vintage failing **any** criterion is not promoted.
//!
//! G1 judges evidence; the *untouched* guarantee comes from the split discipline plus the information
//! firewall (QE-132). Live trust gates are out of scope (QE-222, QE-308).

use std::ops::Range;

use qe_validation::{sharpe_ratio, RobustnessReport};
use serde::{Deserialize, Serialize};

/// Default pre-registered net-of-cost edge floor on the holdout (Sharpe ≥ 0 ⇒ the edge persists).
pub const DEFAULT_MIN_HOLDOUT_SHARPE: f64 = 0.0;
/// Default pre-registered deflated-Sharpe threshold (DSR must exceed this).
pub const DEFAULT_DSR_THRESHOLD: f64 = 0.95;
/// Default pre-registered SPA significance level (the data-snooping p-value must be below this).
pub const DEFAULT_SPA_ALPHA: f64 = 0.05;
/// Default pre-registered OOS tolerance: the holdout Sharpe may fall at most this fraction below in-sample.
pub const DEFAULT_OOS_TOLERANCE: f64 = 0.5;
/// Default pre-registered minimum holdout length — below this the holdout is too small for the Sharpe /
/// over-fit checks to mean anything, so the gate refuses to promote (rather than passing vacuously).
pub const DEFAULT_MIN_HOLDOUT_SAMPLES: usize = 30;

/// A time-blocked train / embargo / holdout split (QE-134/D1). The three ranges are contiguous, disjoint,
/// and cover `0..n`; the `holdout` is the final slice and `embargo` is a purged gap between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holdout {
    /// The training range (everything before the embargo).
    pub train: Range<usize>,
    /// The purged embargo gap (belongs to neither train nor holdout).
    pub embargo: Range<usize>,
    /// The untouched out-of-sample holdout — the final time-blocked slice.
    pub holdout: Range<usize>,
}

/// Carve `0..n` into `train | embargo | holdout`, where `holdout` is the final `holdout_len` samples and
/// `embargo` is the `embargo` samples immediately before it (QE-134/D1). If `holdout_len + embargo ≥ n`
/// the earlier blocks clamp (an empty train, then an empty embargo), so the result is always well-defined
/// and the three ranges always partition `0..n`.
#[must_use]
pub fn split_with_embargo(n: usize, holdout_len: usize, embargo: usize) -> Holdout {
    let holdout_start = n.saturating_sub(holdout_len);
    let embargo_start = holdout_start.saturating_sub(embargo);
    Holdout {
        train: 0..embargo_start,
        embargo: embargo_start..holdout_start,
        holdout: holdout_start..n,
    }
}

/// The pre-registered G1 acceptance criteria (QE-134/D2). Frozen before evaluation — changing them
/// post-hoc defeats the gate — and recorded in the decision so the evidence carries the exact thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct G1Criteria {
    /// Minimum holdout Sharpe for the net-of-cost edge to count as persisting.
    pub min_holdout_sharpe: f64,
    /// The deflated-Sharpe-ratio threshold the DSR must exceed.
    pub dsr_threshold: f64,
    /// The SPA significance level the data-snooping p-value must be below.
    pub spa_alpha: f64,
    /// The fraction by which the holdout Sharpe may fall below in-sample before it is over-fit.
    pub oos_tolerance: f64,
    /// The minimum number of holdout samples for the holdout checks to be meaningful (a smaller holdout is
    /// refused rather than passed vacuously).
    pub min_holdout_samples: usize,
}

impl G1Criteria {
    /// The pre-registered defaults ([`DEFAULT_MIN_HOLDOUT_SHARPE`] etc.).
    #[must_use]
    pub fn with_defaults() -> Self {
        G1Criteria {
            min_holdout_sharpe: DEFAULT_MIN_HOLDOUT_SHARPE,
            dsr_threshold: DEFAULT_DSR_THRESHOLD,
            spa_alpha: DEFAULT_SPA_ALPHA,
            oos_tolerance: DEFAULT_OOS_TOLERANCE,
            min_holdout_samples: DEFAULT_MIN_HOLDOUT_SAMPLES,
        }
    }
}

/// The evidence for one G1 criterion: whether it passed, the observed value, and the threshold it was
/// compared against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionResult {
    /// A short identifier for the criterion.
    pub name: String,
    /// Whether the criterion passed.
    pub passed: bool,
    /// The observed value.
    pub value: f64,
    /// The threshold the value was compared against.
    pub threshold: f64,
}

/// The recorded G1 decision (QE-134/D3): the promotion verdict and the per-criterion evidence. `serde`, so
/// it persists alongside the vintage/report as the auditable acceptance record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct G1Decision {
    /// Whether the vintage is promoted (true iff **every** criterion passed).
    pub promoted: bool,
    /// The per-criterion evidence, in evaluation order.
    pub criteria: Vec<CriterionResult>,
}

impl G1Decision {
    /// The names of the criteria that failed (empty iff promoted).
    #[must_use]
    pub fn failed_criteria(&self) -> Vec<&str> {
        self.criteria
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.name.as_str())
            .collect()
    }
}

/// Evaluate the pre-registered G1 criteria (QE-134/D2) for a vintage on its **holdout** and record the
/// decision. `in_sample_sharpe` is the train-window net-of-cost Sharpe; `holdout_returns` are the
/// net-of-cost returns on the untouched holdout; `robustness` carries the DSR + SPA p-value (QE-131).
///
/// The vintage is promoted iff **all** of: the holdout has enough samples to be meaningful, the holdout
/// edge persists, the DSR exceeds the threshold, the SPA p-value beats the null at the stated level, and
/// the holdout Sharpe is within tolerance of in-sample. A failure of any one criterion blocks promotion,
/// and every criterion's value-vs-threshold evidence is recorded.
#[must_use]
pub fn evaluate_g1(
    in_sample_sharpe: f64,
    holdout_returns: &[f64],
    robustness: &RobustnessReport,
    criteria: &G1Criteria,
) -> G1Decision {
    let holdout_sharpe = sharpe_ratio(holdout_returns);
    let oos_floor = in_sample_sharpe * (1.0 - criteria.oos_tolerance);

    let results = vec![
        CriterionResult {
            name: "holdout_has_sufficient_samples".to_string(),
            passed: holdout_returns.len() >= criteria.min_holdout_samples,
            value: holdout_returns.len() as f64,
            threshold: criteria.min_holdout_samples as f64,
        },
        CriterionResult {
            name: "net_of_cost_edge_persists".to_string(),
            passed: holdout_sharpe >= criteria.min_holdout_sharpe,
            value: holdout_sharpe,
            threshold: criteria.min_holdout_sharpe,
        },
        CriterionResult {
            name: "dsr_exceeds_threshold".to_string(),
            passed: robustness.dsr > criteria.dsr_threshold,
            value: robustness.dsr,
            threshold: criteria.dsr_threshold,
        },
        CriterionResult {
            name: "spa_beats_null".to_string(),
            passed: robustness.spa_pvalue < criteria.spa_alpha,
            value: robustness.spa_pvalue,
            threshold: criteria.spa_alpha,
        },
        CriterionResult {
            name: "oos_within_tolerance_of_in_sample".to_string(),
            passed: holdout_sharpe >= oos_floor,
            value: holdout_sharpe,
            threshold: oos_floor,
        },
    ];

    G1Decision {
        promoted: results.iter().all(|c| c.passed),
        criteria: results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A robustness report that passes the DSR and SPA criteria by default.
    fn good_robustness() -> RobustnessReport {
        RobustnessReport {
            observed_sharpe: 1.8,
            dsr: 0.98, // > 0.95
            pbo: 0.10,
            spa_pvalue: 0.02, // < 0.05
            n_trials: 5000,
        }
    }

    /// Holdout returns with a clear positive edge (Sharpe well above 0).
    fn strong_holdout() -> Vec<f64> {
        (0..200)
            .map(|i| 0.01 + 0.002 * ((i % 5) as f64 - 2.0))
            .collect()
    }

    #[test]
    fn a_clean_vintage_is_promoted() {
        let holdout = strong_holdout();
        let is_sharpe = sharpe_ratio(&holdout); // OOS == IS here ⇒ within tolerance
        let decision = evaluate_g1(
            is_sharpe,
            &holdout,
            &good_robustness(),
            &G1Criteria::with_defaults(),
        );
        assert!(decision.promoted, "{:?}", decision.failed_criteria());
        assert!(decision.criteria.iter().all(|c| c.passed));
        assert!(decision.failed_criteria().is_empty());
    }

    #[test]
    fn each_criterion_blocks_promotion_alone() {
        let crit = G1Criteria::with_defaults();
        let holdout = strong_holdout();
        let is_sharpe = sharpe_ratio(&holdout);

        // 1. Holdout edge gone: a flat/negative holdout fails the edge floor (and the OOS-vs-IS floor).
        let flat: Vec<f64> = (0..200)
            .map(|i| -0.001 + 0.002 * ((i % 5) as f64 - 2.0))
            .collect();
        let d = evaluate_g1(is_sharpe, &flat, &good_robustness(), &crit);
        assert!(!d.promoted);
        assert!(d.failed_criteria().contains(&"net_of_cost_edge_persists"));

        // 2. DSR too low.
        let mut r = good_robustness();
        r.dsr = 0.80; // < 0.95
        let d = evaluate_g1(is_sharpe, &holdout, &r, &crit);
        assert!(!d.promoted);
        assert_eq!(d.failed_criteria(), vec!["dsr_exceeds_threshold"]);

        // 3. SPA p-value too high.
        let mut r = good_robustness();
        r.spa_pvalue = 0.20; // > 0.05
        let d = evaluate_g1(is_sharpe, &holdout, &r, &crit);
        assert!(!d.promoted);
        assert_eq!(d.failed_criteria(), vec!["spa_beats_null"]);

        // 4. OOS collapsed vs in-sample: a much larger IS Sharpe makes the holdout fall below the floor.
        let inflated_is = is_sharpe * 4.0; // holdout must be ≥ inflated_is·0.5 = 2·is_sharpe ⇒ fails
        let d = evaluate_g1(inflated_is, &holdout, &good_robustness(), &crit);
        assert!(!d.promoted);
        assert_eq!(
            d.failed_criteria(),
            vec!["oos_within_tolerance_of_in_sample"]
        );
    }

    #[test]
    fn decision_is_recorded_with_evidence() {
        let holdout = strong_holdout();
        let decision = evaluate_g1(
            sharpe_ratio(&holdout),
            &holdout,
            &good_robustness(),
            &G1Criteria::with_defaults(),
        );
        // Every criterion carries its value + threshold, and the record round-trips through serde.
        assert_eq!(decision.criteria.len(), 5);
        let json = serde_json::to_string(&decision).unwrap();
        let back: G1Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, decision);
    }

    #[test]
    fn an_undersized_holdout_is_refused_not_passed_vacuously() {
        // A too-short (here empty) holdout must not promote even if every other input looks fine — the
        // holdout checks would otherwise be meaningless (sharpe_ratio of an empty series is 0).
        let decision = evaluate_g1(0.0, &[], &good_robustness(), &G1Criteria::with_defaults());
        assert!(!decision.promoted);
        assert!(decision
            .failed_criteria()
            .contains(&"holdout_has_sufficient_samples"));
    }

    #[test]
    fn split_with_embargo_partitions_the_timeline() {
        let h = split_with_embargo(100, 20, 5);
        assert_eq!(h.train, 0..75);
        assert_eq!(h.embargo, 75..80);
        assert_eq!(h.holdout, 80..100);
        // Contiguous, disjoint, covering 0..100.
        assert_eq!(h.train.end, h.embargo.start);
        assert_eq!(h.embargo.end, h.holdout.start);
        assert_eq!(h.holdout.end, 100);

        // Clamps when the holdout + embargo exceed the data: empty train, then empty embargo.
        let h = split_with_embargo(10, 8, 5);
        assert_eq!(h.train, 0..0);
        assert_eq!(h.embargo, 0..2);
        assert_eq!(h.holdout, 2..10);
    }
}
