//! Per-vintage calibration profile (QE-116/D3–D4) — the breaker-threshold sidecar handed to runtime.
//!
//! **Baseline (spec A2):** thresholds are calibrated from **observed behaviour** — the in-sample
//! drawdown distribution of the strategy/cohort/ensemble over the vintage's training history, scaled by
//! a margin and clamped to `[0,1]` ([`calibrate_threshold`]). They sit just beyond what the strategy
//! normally does, so the breaker fires on genuinely abnormal losses, "calibrated prior to deployment".
//!
//! **Alternative (reviewer, documented, not baseline):** calibrate on an **OOS / stressed** drawdown
//! distribution with a larger safety margin. [`calibrate_threshold`] is distribution-agnostic, so the
//! alternative just passes a stressed distribution and a bigger margin — recorded to revisit if
//! in-sample calibration proves too loose (QE-130/QE-134 evidence).

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::breaker::BreakerThresholds;
use crate::limit::Fraction;

/// Per-cohort slow + fast drawdown thresholds (QE-116/D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortThresholds {
    /// Slow-drawdown threshold for the cohort.
    pub slow_dd: Fraction,
    /// Fast-drawdown threshold for the cohort.
    pub fast_dd: Fraction,
}

/// The per-vintage calibration profile sidecar (QE-116/D3) — rides the vintage artefact (QE-129).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationProfile {
    /// Per-strategy breaker thresholds, keyed by strategy id.
    pub per_strategy: BTreeMap<String, BreakerThresholds>,
    /// Per-cohort slow + fast DD thresholds, keyed by cohort id.
    pub per_cohort: BTreeMap<String, CohortThresholds>,
    /// Ensemble-level fast-drop threshold.
    pub ensemble_fast_drop: Fraction,
}

impl CalibrationProfile {
    /// An empty profile with a given ensemble fast-drop threshold.
    #[must_use]
    pub fn new(ensemble_fast_drop: Fraction) -> Self {
        CalibrationProfile {
            per_strategy: BTreeMap::new(),
            per_cohort: BTreeMap::new(),
            ensemble_fast_drop,
        }
    }
}

/// Construct a [`Fraction`] from a value clamped into `[0,1]` (the construction is then always valid).
fn fraction_clamped(value: Decimal) -> Fraction {
    Fraction::new(value.clamp(Decimal::ZERO, Decimal::ONE))
        .expect("a value clamped into [0,1] is a valid Fraction")
}

/// Calibrate a breaker threshold from an **observed** drawdown distribution (QE-116/D4, spec A2): the
/// `quantile` of the observed |drawdown| magnitudes, scaled by `margin (≥ 0)`, clamped to `[0,1]`.
///
/// Distribution-agnostic: passing an OOS/stressed distribution and a larger margin yields the documented
/// stricter alternative. An empty distribution ⇒ a `0` threshold (fires immediately — fail-safe; the
/// caller should not deploy an uncalibrated breaker).
#[must_use]
pub fn calibrate_threshold(
    observed_drawdowns: &[Decimal],
    quantile: f64,
    margin: Decimal,
) -> Fraction {
    if observed_drawdowns.is_empty() {
        return fraction_clamped(Decimal::ZERO);
    }
    let mut magnitudes: Vec<Decimal> = observed_drawdowns.iter().map(|d| d.abs()).collect();
    magnitudes.sort();
    let q = quantile.clamp(0.0, 1.0);
    let idx = ((q * (magnitudes.len() - 1) as f64).round() as usize).min(magnitudes.len() - 1);
    let base = magnitudes[idx];
    fraction_clamped(base * margin.max(Decimal::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn frac(s: &str) -> Fraction {
        Fraction::new(d(s)).unwrap()
    }

    #[test]
    fn calibrate_uses_quantile_and_margin_of_observed() {
        // Observed |drawdowns| (some stored negative): sorted = 0.02, 0.04, 0.06, 0.08, 0.10.
        let observed = [d("-0.02"), d("0.10"), d("-0.06"), d("0.04"), d("-0.08")];
        // Median (q=0.5) → index round(0.5·4)=2 → 0.06, ×1 margin = 0.06.
        assert_eq!(
            calibrate_threshold(&observed, 0.5, Decimal::ONE),
            frac("0.06")
        );
        // Top (q=1.0) → 0.10; a 1.5× margin (the stressed/OOS alternative) → 0.15.
        assert_eq!(calibrate_threshold(&observed, 1.0, d("1.5")), frac("0.15"));
        // A larger margin (stressed alternative) raises the bar vs the baseline — distribution-agnostic.
        assert!(
            calibrate_threshold(&observed, 1.0, d("1.5")).get()
                > calibrate_threshold(&observed, 1.0, Decimal::ONE).get()
        );
    }

    #[test]
    fn calibrate_clamps_to_unit_and_handles_empty() {
        // A huge margin clamps to 1.0 (a Fraction cannot exceed 1).
        assert_eq!(calibrate_threshold(&[d("0.5")], 1.0, d("100")), frac("1"));
        // Empty distribution → 0 (fail-safe).
        assert_eq!(calibrate_threshold(&[], 0.5, Decimal::ONE), frac("0"));
    }

    #[test]
    fn profile_serde_round_trips() {
        let mut profile = CalibrationProfile::new(frac("0.07"));
        profile.per_strategy.insert(
            "rsi_meanrev".to_owned(),
            BreakerThresholds {
                slow_dd: frac("0.05"),
                med_dd: frac("0.12"),
                fast_drop: frac("0.08"),
            },
        );
        profile.per_cohort.insert(
            "momentum".to_owned(),
            CohortThresholds {
                slow_dd: frac("0.06"),
                fast_dd: frac("0.15"),
            },
        );
        let json = serde_json::to_string(&profile).unwrap();
        let back: CalibrationProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, profile);
    }
}
