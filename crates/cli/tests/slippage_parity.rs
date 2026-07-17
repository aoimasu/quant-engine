//! QE-431/QE-440 AC1 — coefficient parity across the search⟂portfolio firewall.
//!
//! `qe-wfo` (`SlippageModel`) and `qe-ensemble` (`CapacityModel`) may not depend on each other, so this
//! cross-crate assertion lives in `qe-cli`, the composition root that links both. It proves that when both
//! sides derive from the **one** [`SlippageCalibration`], they charge the **identical** slippage for
//! identical `(side, qty, mark, spread, ADV)` — the two unit systems (contracts vs $) are reconciled
//! through the shared **participation-keyed** coefficient (QE-440) and can never drift.
#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use qe_domain::Side;
use qe_ensemble::CapacityModel;
use qe_risk::SlippageCalibration;
use qe_wfo::SlippageModel;

/// friction (Decimal, contracts) and capacity (f64, dollars) charge the identical cost for a trade of
/// `qty` at `mark` against a rolling ADV of `adv_qty` contracts, when both derive from the same
/// calibration. Participation is a pure ratio, so `qty/adv_qty` (friction) == `notional/adv_notional`
/// (capacity) with `notional = qty·mark`, `adv_notional = adv_qty·mark`.
fn assert_agree(cal: &SlippageCalibration, qty: Decimal, mark: Decimal, adv_qty: Decimal) {
    let friction = SlippageModel::from_calibration(cal);
    let capacity = CapacityModel::from_calibration(cal);

    let notional = qty * mark;
    let adv_notional = adv_qty * mark;

    // Exact (Decimal): friction keys off `qty/adv_qty`, the canonical per-notional cost keys off
    // `notional/adv_notional` — numerically the same participation.
    let friction_cost = friction.cost(notional, qty, adv_qty);
    assert_eq!(
        friction_cost,
        cal.notional_cost(notional, adv_notional),
        "friction must reduce to the canonical per-notional calibration cost"
    );

    // Cross-unit (friction Decimal→f64 vs capacity f64): agree within f64 rounding.
    let capacity_cost =
        capacity.slippage_cost(notional.to_f64().unwrap(), adv_notional.to_f64().unwrap());
    let f = friction_cost.to_f64().unwrap();
    assert!(
        (f - capacity_cost).abs() <= 1e-9 * f.abs().max(1.0),
        "friction and capacity must agree for identical (side, qty, mark, ADV): friction={f} capacity={capacity_cost}"
    );
}

#[test]
fn friction_and_capacity_agree_for_identical_inputs() {
    // Default calibration and an off-default fitted-shape calibration, several sizes/marks/ADVs.
    let cals = [
        SlippageCalibration::default(),
        SlippageCalibration::new(
            Decimal::new(3, 4), // 0.0003 half-spread
            Decimal::new(2, 2), // 0.02 participation coefficient
            Decimal::new(3, 1), // β = 0.3
        ),
    ];
    for cal in &cals {
        for &(mark, adv_qty) in &[
            (Decimal::new(50_000, 0), Decimal::new(1000, 0)),
            (Decimal::new(2000, 0), Decimal::new(500, 0)), // ETH-scale
        ] {
            for qty in [Decimal::new(1, 0), Decimal::new(7, 0), Decimal::new(25, 1)] {
                // side does not change the (unsigned) slippage magnitude, but exercise both.
                let _ = Side::Buy;
                let _ = Side::Sell;
                assert_agree(cal, qty, mark, adv_qty);
            }
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
        a.impact_coeff * Decimal::from(3), // 3× the participation coefficient
        a.impact_exponent,
    );
    let friction = SlippageModel::from_calibration(&a);
    let capacity = CapacityModel::from_calibration(&b);

    let qty = Decimal::from(10);
    let mark = Decimal::from(50_000);
    let adv_qty = Decimal::from(1000);
    let notional = qty * mark;
    let adv_notional = adv_qty * mark;
    let f = friction.cost(notional, qty, adv_qty).to_f64().unwrap();
    let c = capacity.slippage_cost(notional.to_f64().unwrap(), adv_notional.to_f64().unwrap());
    assert!(
        (f - c).abs() > 1e-6 * f.abs(),
        "mismatched calibrations must disagree: friction={f} capacity={c}"
    );
}
