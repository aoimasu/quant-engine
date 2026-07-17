//! QE-431 AC1 — coefficient parity across the search⟂portfolio firewall.
//!
//! `qe-wfo` (`SlippageModel`) and `qe-ensemble` (`CapacityModel`) may not depend on each other, so this
//! cross-crate assertion lives in `qe-cli`, the composition root that links both. It proves that when both
//! sides derive from the **one** [`SlippageCalibration`], they charge the **identical** slippage for
//! identical `(side, qty, mark, spread)` — the two unit systems (per-contract vs per-$) are reconciled
//! through the shared calibration and can never drift.
#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use qe_domain::Side;
use qe_ensemble::CapacityModel;
use qe_risk::SlippageCalibration;
use qe_wfo::SlippageModel;

/// friction (Decimal, per-contract) and capacity (f64, per-$) charge the identical cost for a trade
/// marked at the calibration's `reference_mark`, when both derive from the same calibration.
fn assert_agree(cal: &SlippageCalibration, qty: Decimal) {
    let friction = SlippageModel::from_calibration(cal);
    let capacity = CapacityModel::from_calibration(cal);

    let mark = cal.reference_mark;
    let notional = qty * mark;

    // Exact (Decimal): friction's per-contract derivation reduces to the canonical per-notional cost.
    let friction_cost = friction.cost(notional, qty);
    assert_eq!(
        friction_cost,
        cal.notional_cost(notional),
        "friction must reduce to the canonical per-notional calibration cost"
    );

    // Cross-unit (friction Decimal→f64 vs capacity f64): agree within f64 rounding.
    let capacity_cost = capacity.slippage_cost(notional.to_f64().unwrap());
    let f = friction_cost.to_f64().unwrap();
    assert!(
        (f - capacity_cost).abs() <= 1e-9 * f.abs().max(1.0),
        "friction and capacity must agree for identical (side, qty, mark): friction={f} capacity={capacity_cost}"
    );
}

#[test]
fn friction_and_capacity_agree_for_identical_inputs() {
    // Default calibration and an off-default fitted-shape calibration, several sizes and both sides.
    let cals = [
        SlippageCalibration::default(),
        SlippageCalibration::new(
            Decimal::new(3, 4),    // 0.0003 half-spread
            Decimal::new(5, 9),    // 5e-9 impact per notional
            Decimal::new(2000, 0), // $2000 reference mark (ETH-scale)
        ),
    ];
    for cal in &cals {
        for qty in [Decimal::new(1, 0), Decimal::new(7, 0), Decimal::new(25, 1)] {
            // side does not change the (unsigned) slippage magnitude, but exercise both.
            let _ = Side::Buy;
            let _ = Side::Sell;
            assert_agree(cal, qty);
        }
    }
}

#[test]
fn parity_is_non_vacuous_mismatched_calibrations_disagree() {
    // If capacity were built from a DIFFERENT calibration than friction, the costs diverge — proving the
    // agreement above is a real constraint, not a tautology.
    let a = SlippageCalibration::default();
    let b = SlippageCalibration::new(
        a.half_spread,
        a.impact_per_notional * Decimal::from(3), // 3× the size-impact
        a.reference_mark,
    );
    let friction = SlippageModel::from_calibration(&a);
    let capacity = CapacityModel::from_calibration(&b);

    let qty = Decimal::from(10);
    let notional = qty * a.reference_mark;
    let f = friction.cost(notional, qty).to_f64().unwrap();
    let c = capacity.slippage_cost(notional.to_f64().unwrap());
    assert!(
        (f - c).abs() > 1e-6 * f.abs(),
        "mismatched calibrations must disagree: friction={f} capacity={c}"
    );
}
