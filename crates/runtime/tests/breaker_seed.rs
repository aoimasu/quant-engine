//! QE-401 — Seed the live drawdown breaker with the reconstructed committed-peak equity.
//!
//! The capital-safety AC: after a cold start, the drawdown breaker must anchor on the *true* all-time
//! committed peak reconstructed from history — not re-anchor on the first live equity tick. Without the seed,
//! a book already 15% below its historical peak reports ≈0 drawdown on its first tick and the slow/med DD
//! breaker stays silent (a capital-loss path). This test feeds a history that peaks then declines 15%,
//! reconstructs the cold-start state via the production `ReconstructedState::from_replay`, seeds the live
//! `BreakerLayer` from it, and asserts the first live tick at the declined level reports the *true* drawdown
//! and trips the med tier — and that the seeded peak equals `CommittedPeak::from_series` of the replayed path
//! bit-for-bit.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use qe_risk::{BreakerThresholds, BreakerTier, CalibrationProfile, Fraction, DEFAULT_FAST_WINDOW};
use qe_runtime::boot_state::{CommittedPeak, ReconstructedState};
use qe_runtime::BreakerLayer;
use qe_signal::PositionState;
use rust_decimal::Decimal;

fn dec(n: i64) -> Decimal {
    Decimal::from(n)
}
fn frac(s: &str) -> Fraction {
    Fraction::new(s.parse::<Decimal>().unwrap()).unwrap()
}

/// slow_dd = 5%, med_dd = 12%, fast_drop = 8% — a 15% drawdown clears the med tier.
fn thresholds() -> BreakerThresholds {
    BreakerThresholds {
        slow_dd: frac("0.05"),
        med_dd: frac("0.12"),
        fast_drop: frac("0.08"),
    }
}

/// A history that rises to an all-time peak of 200 then declines 15% to 170 (the declined "live" level).
fn history_peaks_then_declines_15pct() -> Vec<Decimal> {
    vec![
        dec(100),
        dec(120),
        dec(150),
        dec(180),
        dec(200), // the true all-time peak
        dec(170), // already 15% below the peak: (200 − 170) / 200 = 0.15
    ]
}

/// **The AC.** Cold-start seeding: the first live tick at the declined level reports the true ~15% drawdown
/// (not ~0) and trips the med tier at threshold; the seeded peak equals `CommittedPeak::from_series`
/// bit-for-bit.
#[test]
fn seeded_breaker_reports_true_drawdown_and_trips_med_on_first_live_tick() {
    let history = history_peaks_then_declines_15pct();

    // Production cold-start reconstruction: one strategy, its equity path folded to a true committed peak.
    let positions = vec![PositionState::flat()];
    let decisions = vec![]; // no decision trace needed to reconstruct the committed peak
    let equity_paths = vec![history.clone()];
    let reconstructed =
        ReconstructedState::from_replay(&positions, &decisions, &equity_paths).unwrap();

    // The reconstructed committed peak is the true all-time max of the replayed path, bit-for-bit.
    let expected_peak = CommittedPeak::from_series(&history).peak().unwrap();
    assert_eq!(expected_peak, dec(200));
    assert_eq!(
        reconstructed.strategies[0].committed_peak_equity,
        Some(expected_peak)
    );

    // Build the live layer the way the runtime will (calibration profile → from_calibration), then seed.
    let mut profile = CalibrationProfile::new(frac("0.20"));
    profile.per_strategy.insert("s0".to_owned(), thresholds());
    let ids = vec!["s0".to_owned()];
    let mut layer = BreakerLayer::from_calibration(&profile, &ids, DEFAULT_FAST_WINDOW);
    layer.seed_committed_peaks(&reconstructed);

    // Bit-for-bit: the breaker's seeded anchor equals CommittedPeak::from_series of the replayed path.
    assert_eq!(
        layer.strategy_peak(0),
        Some(expected_peak),
        "the seeded breaker peak must equal the reconstructed committed peak bit-for-bit"
    );

    // The FIRST live tick at the declined level (170) reports the true 15% drawdown → trips Med.
    let tier = layer.observe_strategy(0, dec(170));
    assert_eq!(
        tier,
        Some(BreakerTier::Med),
        "a book 15% below its historical peak must trip the med tier on the first live tick"
    );
    assert!(layer.is_gated(0), "the tripped strategy is latched gated");

    // Control: an UNSEEDED layer re-anchors on the first tick and stays silent — the exact bug QE-401 fixes.
    let mut unseeded = BreakerLayer::from_calibration(&profile, &ids, DEFAULT_FAST_WINDOW);
    assert_eq!(
        unseeded.observe_strategy(0, dec(170)),
        None,
        "without the seed the first live tick re-anchors and reports ~0 drawdown (silent)"
    );
    assert!(!unseeded.is_gated(0));
}

/// The seed survives a session rollover (`reset`): the anchor is not silently cleared, so a post-rollover
/// first tick still measures the true drawdown.
#[test]
fn seeded_anchor_survives_rollover_reset() {
    let history = history_peaks_then_declines_15pct();
    let reconstructed = ReconstructedState::from_replay(
        &[PositionState::flat()],
        &[],
        std::slice::from_ref(&history),
    )
    .unwrap();

    let mut profile = CalibrationProfile::new(frac("0.20"));
    profile.per_strategy.insert("s0".to_owned(), thresholds());
    let mut layer =
        BreakerLayer::from_calibration(&profile, &["s0".to_owned()], DEFAULT_FAST_WINDOW);
    layer.seed_committed_peaks(&reconstructed);

    layer.observe_strategy(0, dec(195)); // 2.5% drawdown, no fire
    layer.reset();

    assert_eq!(
        layer.strategy_peak(0),
        Some(dec(200)),
        "the committed-peak anchor survives a rollover reset"
    );
    assert_eq!(
        layer.observe_strategy(0, dec(170)),
        Some(BreakerTier::Med),
        "after the rollover the first tick still trips on the true 15% drawdown"
    );
}
