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

/// Quantile of the running-peak drawdown distribution used for the slow (depth) tier (QE-416).
pub const DEFAULT_SLOW_QUANTILE: f64 = 0.75;
/// Quantile of the running-peak drawdown distribution used for the med (deeper depth) tier (QE-416).
pub const DEFAULT_MED_QUANTILE: f64 = 0.95;
/// Quantile of the fast-window drop distribution used for the fast (speed) tier (QE-416).
pub const DEFAULT_FAST_QUANTILE: f64 = 0.95;

/// The default safety margin (QE-416): calibrated thresholds sit `1.5×` beyond the observed quantile, so
/// the breaker fires on genuinely abnormal losses rather than normal behaviour.
#[must_use]
pub fn default_calibration_margin() -> Decimal {
    Decimal::new(15, 1) // 1.5
}

/// Decimal places a calibrated threshold is quantized to (QE-416). Thresholds derived from an
/// `f64`-valued equity curve carry excess precision (a `Decimal` from division can hold ~28 significant
/// digits); rounding to a fixed, modest scale keeps the **hashed** vintage's `Decimal` strings
/// bounded-precision and **serialize-idempotent** (a value with excess precision serialises then reparses
/// to a different byte string, which would break the QE-402 content-hash verify). Twelve places is
/// sub-basis-point resolution — far finer than any breaker threshold needs.
pub const CALIBRATION_SCALE: u32 = 12;

/// Quantize a calibrated [`Fraction`] to [`CALIBRATION_SCALE`] decimal places so the sealed value
/// round-trips byte-identically through serde (see [`CALIBRATION_SCALE`]). The rounded value is
/// `normalize`d to its minimal (canonical) scale — `round_dp` alone can retain the operand's
/// trailing-zero scale, whose `Decimal` string then differs from the minimal string a parse reconstructs,
/// breaking serialize-idempotency and so the content-hash verify. Clamped construction always succeeds;
/// the value stays in `[0,1]`.
#[must_use]
pub fn quantize_calibration(f: Fraction) -> Fraction {
    fraction_clamped(f.get().round_dp(CALIBRATION_SCALE).normalize())
}

/// The running-peak **drawdown** magnitudes of an `equity` curve — one per tick, each a fraction in
/// `[0,1]` (`(peak − equity) / peak`, `0` while at/above the peak). This is the total-drawdown measure
/// the [`CircuitBreaker`](crate::CircuitBreaker) slow/med tiers observe, so calibrating against it places
/// the thresholds just beyond replayed behaviour.
#[must_use]
pub fn drawdown_distribution(equity: &[Decimal]) -> Vec<Decimal> {
    let mut peak = Decimal::ZERO;
    let mut out = Vec::with_capacity(equity.len());
    for &e in equity {
        peak = peak.max(e);
        let dd = if peak > Decimal::ZERO {
            (peak - e) / peak
        } else {
            Decimal::ZERO
        };
        out.push(dd.max(Decimal::ZERO));
    }
    out
}

/// The rolling **fast-window drop** magnitudes of an `equity` curve over `fast_window` — one per tick,
/// each `(window_max − equity) / window_max` over the last `fast_window` ticks. Mirrors the
/// [`CircuitBreaker`](crate::CircuitBreaker) fast-drop measure (which retains `fast_window + 1` ticks),
/// so calibrating against it places the fast threshold just beyond replayed speed-of-drop behaviour.
#[must_use]
pub fn fast_drop_distribution(equity: &[Decimal], fast_window: usize) -> Vec<Decimal> {
    let w = fast_window.max(1);
    let mut out = Vec::with_capacity(equity.len());
    for i in 0..equity.len() {
        let lo = i.saturating_sub(w);
        let window_max = equity[lo..=i]
            .iter()
            .copied()
            .max()
            .unwrap_or_else(|| equity[i]);
        let drop = if window_max > Decimal::ZERO {
            (window_max - equity[i]) / window_max
        } else {
            Decimal::ZERO
        };
        out.push(drop.max(Decimal::ZERO));
    }
    out
}

/// Calibrate a strategy's three-tier [`BreakerThresholds`] from its **observed** `equity` behaviour
/// (QE-116/D4, spec A2 baseline; wired at seal by QE-416): `slow_dd` / `med_dd` from the running-peak
/// [`drawdown_distribution`] at [`DEFAULT_SLOW_QUANTILE`] / [`DEFAULT_MED_QUANTILE`], `fast_drop` from the
/// [`fast_drop_distribution`] at [`DEFAULT_FAST_QUANTILE`], each scaled by `margin` via
/// [`calibrate_threshold`]. `med_dd` is forced `>= slow_dd` (the tier invariant `slow_dd < med_dd`).
///
/// An all-flat curve (no observed drawdown) yields zero thresholds — the QE-116 fail-safe: a strategy
/// that cannot be calibrated should not be deployed.
#[must_use]
pub fn calibrate_thresholds(
    equity: &[Decimal],
    fast_window: usize,
    margin: Decimal,
) -> BreakerThresholds {
    let dd = drawdown_distribution(equity);
    let fast = fast_drop_distribution(equity, fast_window);
    let slow_dd = quantize_calibration(calibrate_threshold(&dd, DEFAULT_SLOW_QUANTILE, margin));
    let med_raw = quantize_calibration(calibrate_threshold(&dd, DEFAULT_MED_QUANTILE, margin));
    // The med tier is the deeper-depth tier; a higher quantile normally yields med >= slow, but guard the
    // invariant explicitly (equal distributions, ties) so med_dd is never below slow_dd.
    let med_dd = if med_raw.get() >= slow_dd.get() {
        med_raw
    } else {
        slow_dd
    };
    let fast_drop = quantize_calibration(calibrate_threshold(&fast, DEFAULT_FAST_QUANTILE, margin));
    BreakerThresholds {
        slow_dd,
        med_dd,
        fast_drop,
    }
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
    fn calibrate_thresholds_from_equity_orders_tiers_and_uses_observed_behaviour() {
        // A drawdown to 0.7 then a partial recovery, then a deeper trough to 0.6 — real observed drawdown.
        let equity = [
            d("1.0"),
            d("1.1"),
            d("0.9"),
            d("0.7"),
            d("0.95"),
            d("1.2"),
            d("0.6"),
        ];
        let th = calibrate_thresholds(
            &equity,
            crate::DEFAULT_FAST_WINDOW,
            default_calibration_margin(),
        );
        // Non-degenerate: the curve draws down, so calibration produces positive thresholds.
        assert!(
            th.slow_dd.get() > d("0"),
            "slow tier calibrated from observed drawdown"
        );
        assert!(
            th.fast_drop.get() > d("0"),
            "fast tier calibrated from observed drops"
        );
        // Tier invariant: slow <= med.
        assert!(
            th.med_dd.get() >= th.slow_dd.get(),
            "med tier is at least the slow tier"
        );
    }

    #[test]
    fn quantized_thresholds_round_trip_through_serde() {
        // QE-416 hash-stability: thresholds calibrated from an f64-derived equity curve carry excess
        // Decimal precision; quantization must make them serialize→parse→serialize idempotent (else the
        // hashed vintage fails its content-hash verify on reload).
        let equity: Vec<Decimal> = [1.0f64, 1.1, 0.93, 0.71, 0.88, 1.05, 0.6]
            .iter()
            .map(|&v| Decimal::from_f64_retain(v).unwrap())
            .collect();
        let th = calibrate_thresholds(
            &equity,
            crate::DEFAULT_FAST_WINDOW,
            default_calibration_margin(),
        );
        for f in [th.slow_dd, th.med_dd, th.fast_drop] {
            let s1 = serde_json::to_string(&f).unwrap();
            let back: Fraction = serde_json::from_str(&s1).unwrap();
            let s2 = serde_json::to_string(&back).unwrap();
            assert_eq!(
                s1, s2,
                "quantized threshold must serialize idempotently: {s1}"
            );
            // Bounded to CALIBRATION_SCALE decimal places.
            assert!(
                f.get().scale() <= CALIBRATION_SCALE,
                "scale {} > {CALIBRATION_SCALE}",
                f.get().scale()
            );
        }
    }

    #[test]
    fn calibrate_thresholds_flat_equity_is_zero_fail_safe() {
        // A monotonically non-decreasing curve has no drawdown ⇒ zero thresholds (QE-116 fail-safe).
        let equity = [d("1.0"), d("1.1"), d("1.2"), d("1.3")];
        let th = calibrate_thresholds(
            &equity,
            crate::DEFAULT_FAST_WINDOW,
            default_calibration_margin(),
        );
        assert_eq!(th.slow_dd, frac("0"));
        assert_eq!(th.med_dd, frac("0"));
        assert_eq!(th.fast_drop, frac("0"));
    }

    #[test]
    fn drawdown_and_fast_drop_distributions_track_the_curve() {
        // Peak 1.1 then trough 0.55 ⇒ running-peak drawdown reaches 0.5.
        let equity = [d("1.0"), d("1.1"), d("0.55"), d("0.66")];
        let dd = drawdown_distribution(&equity);
        assert_eq!(dd[0], d("0")); // at peak
        assert_eq!(dd[1], d("0")); // new peak
        assert_eq!(dd[2], d("0.5")); // 1 − 0.55/1.1
                                     // Fast-window drop over a wide window sees the same 0.5 drop from the recent max.
        let fast = fast_drop_distribution(&equity, crate::DEFAULT_FAST_WINDOW);
        assert_eq!(fast[2], d("0.5"));
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
