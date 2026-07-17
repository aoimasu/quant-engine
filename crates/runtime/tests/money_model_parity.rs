//! QE-435 — train-backtest ↔ live execution/money-model parity.
//!
//! Archive selection rides on the **net-of-friction `log_growth`** the `qe-wfo` backtest produces, so the
//! *money* model — how a `(side, qty, mark, spread)` fill becomes cash — not just the *decision*, must match
//! the live path. Today only `Genome::decide` / `PositionState::advance` are shared; **fills, sizing, and
//! cost are implemented on each side independently** (`qe-wfo` `friction`/`backtest` vs `qe-edge`
//! `plan_delta`/`VenueSimulator`). This is the QE-432 slow-reference-oracle move applied to the execution
//! money model: an **independent oracle test** that pins the invariant, rather than a refactor that would
//! move goldens.
//!
//! **The finding this test encodes** (see `docs/architecture/qe-435-money-model-parity-design.md`): the two
//! sides agree **exactly** on the *fill* — side, qty, traded notional, resulting signed position — because
//! `notional/price` (wfo) and `target/mark − current` (live) are the same arithmetic. They do **not** "agree"
//! on *cost*, because the live `VenueSimulator` is a paper/plumbing model that charges **zero** cost: it
//! fills at the raw mark. Cost on the selection path lives in exactly one place — `qe-wfo` friction — and
//! that model is (QE-431) derived from the single content-addressed `SlippageCalibration`, the same source
//! `capacity` reads. So the parity we assert is: (1) fill-geometry is identical; (2) the wfo friction cost
//! reduces to the shared calibration (single source, no divergent second number); (3) the one real gap — the
//! sim's zero-cost fill — is quantified and asserted, not papered over.
//!
//! This crate is `qe-runtime` (the live composition facade); it links `qe-edge`/`qe-hedger`/`qe-risk` in
//! production and `qe-wfo` as a **dev-dependency** — firewall-clean (dev-deps are excluded from the
//! `qe-architecture` firewall graph, and `runtime → wfo` is the allowed downstream direction).

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::str::FromStr;

use rust_decimal::Decimal;

use qe_domain::{InstrumentId, Notional, Price, Qty, Side};
use qe_edge::{plan_delta, VenueSimulator};
use qe_risk::SlippageCalibration;
use qe_venue::userdata::UserDataEvent;
use qe_wfo::{FeeSchedule, Liquidity, Position, SlippageModel};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn instrument() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

/// The signed position quantity the wfo backtester reaches by moving from `current` to `target_qty`, and the
/// live `plan_delta`+`VenueSimulator` reaches for the same move — driven through **both** real code paths.
struct FillMove {
    live_final_signed: Decimal,
    wfo_final_signed: Decimal,
    intent_side: Side,
    intent_qty: Decimal,
    /// The price the sim actually filled the delta at (to prove zero embedded slippage).
    sim_fill_price: Decimal,
}

/// Drive a move to `target` notional from a `current` signed position at `mark` through both money models.
fn move_to_target(current: Decimal, target: Decimal, mark: Decimal) -> Option<FillMove> {
    let mark_price = Price::new(mark).unwrap();

    // --- live: plan_delta emits the venue-native order, VenueSimulator fills it ---
    let intent = plan_delta(Notional::new(target), current, mark_price)?;

    let mut sim = VenueSimulator::new(instrument());
    // Seed the sim to `current` (a real fill), so both sides start from the identical position.
    if !current.is_zero() {
        let seed_side = if current.is_sign_negative() {
            Side::Sell
        } else {
            Side::Buy
        };
        let seed = qe_edge::OrderIntent {
            side: seed_side,
            qty: Qty::abs_of(current),
        };
        sim.submit(seed, mark_price, 1);
    }
    let fill = sim.submit(intent, mark_price, 2);
    let sim_fill_price = match &fill.event {
        UserDataEvent::Fill(f) => f.price.get(),
        _ => panic!("VenueSimulator must emit a Fill event"),
    };

    // --- wfo: friction Position, seeded at the identical `current`, applies the same delta fill ---
    let mut pos = Position {
        qty: current,
        avg_price: mark,
    };
    pos.apply(intent.side, intent.qty.get(), mark);

    Some(FillMove {
        live_final_signed: sim.signed_qty(),
        wfo_final_signed: pos.qty,
        intent_side: intent.side,
        intent_qty: intent.qty.get(),
        sim_fill_price,
    })
}

/// AC (Scope, fill parity): for identical `(current, target, mark)` the live `plan_delta`/`VenueSimulator`
/// and the wfo `friction` sizing reach the **same signed position** with the **same order side/qty** — the
/// fills are the same arithmetic, not two divergent implementations.
#[test]
fn fill_geometry_is_identical_across_train_and_live() {
    // (current signed qty, target notional, mark): flat→long, flat→short, add, reduce, and a flip through 0.
    let cases = [
        (d("0"), d("10000"), d("50000")),   // flat → +0.2
        (d("0"), d("-15000"), d("50000")),  // flat → −0.3
        (d("0.2"), d("5000"), d("50000")),  // reduce +0.2 → +0.1
        (d("0.2"), d("20000"), d("50000")), // add +0.2 → +0.4
        (d("0.1"), d("-6000"), d("50000")), // flip +0.1 → −0.12 (crosses zero)
        (d("-0.3"), d("8000"), d("50000")), // flip −0.3 → +0.16
        (d("3"), d("7000"), d("2000")),     // ETH-scale mark
    ];

    for (current, target, mark) in cases {
        let mv = move_to_target(current, target, mark)
            .expect("a non-degenerate move must produce an order");

        // Independent wfo-side mirror of the sizing, to pin what "identical inputs" means.
        let target_qty = target / mark;
        let delta = target_qty - current;
        let expect_side = if delta.is_sign_negative() {
            Side::Sell
        } else {
            Side::Buy
        };

        assert_eq!(
            mv.intent_side, expect_side,
            "order side must match the signed delta for ({current}, {target}, {mark})"
        );
        assert_eq!(
            mv.intent_qty,
            delta.abs(),
            "order qty must be |Δposition| = |target/mark − current|"
        );

        // Both money models reach the SAME absolute target position (= target/mark) from the same start.
        assert_eq!(
            mv.live_final_signed, mv.wfo_final_signed,
            "live and wfo must reach the identical signed position for ({current}, {target}, {mark})"
        );
        assert_eq!(
            mv.wfo_final_signed, target_qty,
            "the reached position must equal the absolute target/mark"
        );
    }
}

/// AC (Scope, single-source cost): the wfo friction slippage cost for a fill reduces **exactly** to the
/// shared QE-431/QE-440 `SlippageCalibration` participation cost (the common ground `slippage_parity`
/// uses). There is no *second, divergent* live cost number — the one calibration is the single source both
/// priced sides read.
#[test]
fn wfo_friction_cost_reduces_to_the_shared_calibration() {
    let cals = [
        SlippageCalibration::default(),
        SlippageCalibration::new(
            d("0.0003"), // 3bp half-spread
            d("0.02"),   // participation coefficient
            d("0.3"),    // β
        ),
    ];

    let mark = d("2000"); // ETH-scale mark
    let adv_qty = d("1000"); // rolling ADV in contracts
    for cal in &cals {
        let friction = SlippageModel::from_calibration(cal);
        for qty in [d("1"), d("7"), d("2.5")] {
            let notional = qty * mark;
            let adv_notional = adv_qty * mark;
            // Exact Decimal identity: friction (keyed on qty/adv_qty) IS the canonical per-notional cost
            // (keyed on notional/adv_notional) — the same participation.
            assert_eq!(
                friction.cost(notional, qty, adv_qty),
                cal.notional_cost(notional, adv_notional),
                "friction must reduce to the shared calibration participation cost (qty={qty})"
            );
        }
    }
}

/// AC (the finding, asserted & non-vacuous): the live `VenueSimulator` models **zero** cost — it fills at the
/// raw mark, moving cash only by the signed notional — while the wfo backtest charges `taker_fee +
/// calibration_slippage` on the identical fill. This pins the exact, quantified gap the panel named (so a
/// regression that silently started charging, or a wfo change that stopped matching the calibration, fails).
#[test]
fn venue_simulator_models_zero_cost_and_the_gap_equals_the_shared_calibration() {
    let cal = SlippageCalibration::default();
    let friction = SlippageModel::from_calibration(&cal);
    let fees = FeeSchedule::default();
    let mark = d("50000");
    let adv_qty = d("1000");
    let adv_notional = adv_qty * mark;

    for qty in [d("1"), d("4"), d("0.5")] {
        let notional = qty * mark;

        // Live: the sim fills at exactly the mark — no slippage embedded in the fill price, no cost deducted.
        let mv = move_to_target(Decimal::ZERO, notional, mark).unwrap();
        assert_eq!(
            mv.sim_fill_price, mark,
            "VenueSimulator must fill at the raw mark (zero modeled slippage)"
        );
        assert_eq!(mv.intent_qty, qty, "sanity: the sim traded exactly qty");

        // wfo: the same fill costs taker fee + shared-calibration slippage — strictly positive.
        let wfo_cost = fees.fee(notional, Liquidity::Taker) + friction.cost(notional, qty, adv_qty);
        assert!(
            wfo_cost > Decimal::ZERO,
            "the wfo money model charges a real cost"
        );
        assert_eq!(
            wfo_cost,
            fees.fee(notional, Liquidity::Taker) + cal.notional_cost(notional, adv_notional),
            "the cost the sim omits equals taker fee + the shared calibration slippage"
        );
    }
}

/// Non-vacuity guard (mirrors QE-432's mutation guard / QE-431 `parity_is_non_vacuous`): a *mismatched*
/// calibration makes the cost identity FAIL, proving the reduction above is a real constraint, not a
/// tautology.
#[test]
fn cost_identity_is_non_vacuous_mismatched_calibration_disagrees() {
    let a = SlippageCalibration::default();
    let b = SlippageCalibration::new(
        a.half_spread,
        a.impact_coeff * Decimal::from(3), // 3× participation coefficient
        a.impact_exponent,
    );
    let friction_a = SlippageModel::from_calibration(&a);

    let qty = d("10");
    let mark = d("50000");
    let adv_qty = d("1000");
    let notional = qty * mark;
    let adv_notional = adv_qty * mark;
    assert_ne!(
        friction_a.cost(notional, qty, adv_qty),
        b.notional_cost(notional, adv_notional),
        "a mismatched calibration must disagree — the parity is a genuine constraint"
    );
}
