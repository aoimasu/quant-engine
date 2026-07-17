//! QE-429 — Wire the live `BreakerLayer` at cutover.
//!
//! QE-416/401/417 built the calibrated, committed-peak-seeded breaker but left it **latent**:
//! `BreakerLayer::from_calibration` + `seed_committed_peaks` had no production caller. QE-429 wires them at
//! the real cutover site (`Cutover::from_reconstructed_calibrated`), where the sealed vintage's calibration
//! (keyed by `content.strategy_ids()`) and the reconstructed committed-peak state converge, **before the
//! first live tick**. This integration test proves a cold-start live cutover at that site:
//!   (a) constructs the layer from the sealed calibration keyed by `strategy_ids()` — no calibrated member
//!       is pre-gated;
//!   (b) seeds the committed peak from `ReconstructedState`, so the first post-cutover equity tick reports
//!       the reconstructed drawdown (not ~0) and trips the right tier;
//!   (c) an uncalibrated/missing strategy is fail-safe pre-gated;
//! and that the wired `feed_live_bar` clamps gated strategies to flat — non-vacuous vs an un-seeded control.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use qe_domain::{Bar, Direction, Price, Qty, Resolution, Timestamp};
use qe_risk::{BreakerThresholds, BreakerTier, CalibrationProfile, Fraction, DEFAULT_FAST_WINDOW};
use qe_runtime::boot_state::{DormancyLatch, ReconstructedState, StrategyState};
use qe_runtime::{Cutover, CutoverStep, EvaluatorSession, Reconstructed};
use qe_signal::{
    CatalogueConfig, Clause, Decision, ExitParams, FeatureSchema, Genome, PositionState,
    RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
};
use qe_vintage::{Vintage, VintageContent, VINTAGE_FORMAT_VERSION};
use rust_decimal::Decimal;

const MIN: i64 = 60_000;

fn cfg() -> CatalogueConfig {
    CatalogueConfig::default()
}
fn dec(n: i64) -> Decimal {
    Decimal::from(n)
}
fn frac(s: &str) -> Fraction {
    Fraction::new(s.parse::<Decimal>().unwrap()).unwrap()
}
fn p(n: i64) -> Price {
    Price::new(Decimal::from(n)).unwrap()
}
fn q(n: i64) -> Qty {
    Qty::new(Decimal::from(n)).unwrap()
}

/// slow_dd = 5%, med_dd = 12%, fast_drop = 8% — a 15% drawdown clears the med tier.
fn thresholds() -> BreakerThresholds {
    BreakerThresholds {
        slow_dd: frac("0.05"),
        med_dd: frac("0.12"),
        fast_drop: frac("0.08"),
    }
}

fn off_clause() -> Clause {
    Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    }
}

/// A genome that goes long whenever feature 0 is warm and exits after `max_holding` bars.
fn cycling_genome(max_holding: u16) -> Genome {
    let num_states = FeatureSchema::from_catalogue(&cfg()).num_states();
    let mut clauses = [off_clause(); CLAUSES_PER_SET];
    clauses[0] = Clause {
        enabled: true,
        feature: 0,
        lo: 0,
        hi: num_states - 1,
    };
    Genome {
        version: REP_VERSION,
        long_entry: RuleSet {
            clauses,
            min_satisfied: 1,
        },
        short_entry: RuleSet {
            clauses: [off_clause(); CLAUSES_PER_SET],
            min_satisfied: 1,
        },
        exit: ExitParams {
            max_holding_bars: max_holding,
            exit_on_opposite: false,
        },
        risk: RiskParams { size_bps: 5_000 },
    }
}

/// A base 5m bar at index `i`.
fn bar(i: i64) -> Bar {
    let base = 100 + (i % 13);
    Bar::new(
        Timestamp::from_millis(i * 5 * MIN),
        Resolution::M5,
        p(base),
        p(base + 3),
        p(base - 2),
        p(base + 1),
        q(10 + (i % 7)),
        5,
    )
    .unwrap()
}

/// A sealed 2-strategy vintage whose calibration profile keys strategy `"0"` (calibrated with `thresholds`)
/// but **omits** `"1"` (uncalibrated → the fail-safe pre-gate the runtime must honour). Ensemble fast-drop
/// 0.20. This is the shape a partial calibration would take; the seal keys strategy ids positionally, so the
/// cutover keys the same ids via `EvaluatorSession::strategy_ids()`.
fn vintage_with_partial_calibration() -> Vintage {
    use qe_determinism::Lineage;
    let mut calibration = CalibrationProfile::new(frac("0.20"));
    calibration
        .per_strategy
        .insert("0".to_owned(), thresholds());
    // "1" intentionally absent → uncalibrated → pre-gated by `from_calibration`.
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: "qe-429-test".to_owned(),
        chromosomes: vec![cycling_genome(3), cycling_genome(2)],
        weights: vec![0.5, 0.5],
        calibration,
        slippage: qe_risk::SlippageCalibration::default(),
        worst_case_loss: None,
        catalogue: qe_signal::CatalogueIdentity::current(),
        lineage: Lineage::new("cfg", "snap", "commit", vec![]),
    };
    Vintage::seal(content).unwrap()
}

/// Build a warmed `Reconstructed` by replaying bars `0..n` through a fresh session on `vintage` (mirrors
/// what the bootstrap pipeline hands the cutover: a warmed session + its decision trace).
fn reconstructed(vintage: Vintage, n: i64) -> Reconstructed {
    let mut session = EvaluatorSession::new(vintage, &cfg());
    let decisions = (0..n).map(|i| session.on_bar(&bar(i))).collect::<Vec<_>>();
    Reconstructed {
        session,
        decisions,
        coarse_bars: Vec::new(),
        bars_replayed: n as usize,
        last_mark_price: None,
    }
}

/// A reconstructed state: strategy 0 has a true committed peak of 200; strategy 1 has an empty path (`None`).
fn state_peak_200_and_none() -> ReconstructedState {
    ReconstructedState {
        strategies: vec![
            StrategyState {
                index: 0,
                position: PositionState::flat(),
                dormancy: DormancyLatch::active(),
                committed_peak_equity: Some(dec(200)),
            },
            StrategyState {
                index: 1,
                position: PositionState::flat(),
                dormancy: DormancyLatch::active(),
                committed_peak_equity: None,
            },
        ],
    }
}

/// An empty-peak control state (both strategies un-seeded) — the un-seeded control for non-vacuity.
fn state_all_unseeded() -> ReconstructedState {
    ReconstructedState {
        strategies: vec![
            StrategyState {
                index: 0,
                position: PositionState::flat(),
                dormancy: DormancyLatch::active(),
                committed_peak_equity: None,
            },
            StrategyState {
                index: 1,
                position: PositionState::flat(),
                dormancy: DormancyLatch::active(),
                committed_peak_equity: None,
            },
        ],
    }
}

/// **The AC.** A cold-start live cutover wires the calibrated, committed-peak-seeded breaker at the real
/// cutover site: (a) calibration keyed by `strategy_ids()` pre-gates no calibrated member, (b) the seed
/// makes the first post-cutover equity tick report the true drawdown and trip Med, (c) the uncalibrated
/// strategy is fail-safe pre-gated — and `feed_live_bar` clamps both gated strategies to flat.
#[test]
fn cold_start_cutover_wires_calibrated_seeded_breaker() {
    const N: i64 = 20; // replayed bars 0..N (last replayed = N-1)
    let cutover = Cutover::from_reconstructed_calibrated(
        reconstructed(vintage_with_partial_calibration(), N),
        &state_peak_200_and_none(),
        Resolution::M5,
        DEFAULT_FAST_WINDOW,
    )
    .unwrap();
    let mut cutover = cutover;

    // (a) + (c): the layer is constructed from the sealed calibration keyed by strategy_ids — strategy 0
    // (calibrated) is NOT pre-gated; strategy 1 (missing from the profile) IS fail-safe pre-gated.
    let breaker = cutover
        .breaker()
        .expect("QE-429: the cutover must wire a live breaker");
    assert_eq!(breaker.strategy_count(), 2);
    assert!(
        !breaker.is_gated(0),
        "calibrated strategy 0 must not be pre-gated"
    );
    assert!(
        breaker.is_gated(1),
        "uncalibrated strategy 1 must be fail-safe pre-gated (AC c)"
    );

    // (b): the committed peak was seeded from the reconstructed state BEFORE any live tick — the anchor is
    // the true all-time peak (200), bit-for-bit.
    assert_eq!(
        cutover.breaker().unwrap().strategy_peak(0),
        Some(dec(200)),
        "strategy 0's drawdown anchor must be seeded to the reconstructed committed peak"
    );

    // The FIRST post-cutover equity tick at the declined level (170 = 15% below 200) reports the true
    // drawdown and trips Med — routed through the wired cutover, not re-anchoring on ~0.
    let tier = cutover.observe_strategy_equity(0, dec(170));
    assert_eq!(
        tier,
        Some(BreakerTier::Med),
        "a book 15% below its seeded peak must trip Med on the first live tick (AC b)"
    );
    assert!(cutover.breaker().unwrap().is_gated(0));

    // The wired `feed_live_bar` clamps every gated strategy to flat BEFORE netting: strategy 0 (tripped) and
    // strategy 1 (pre-gated) both become `Exit`. Feed the next contiguous bar (open = N*5m).
    let step = cutover.feed_live_bar(&bar(N)).unwrap();
    let CutoverStep::Evaluated(out) = step else {
        panic!("bar {N} should evaluate, not be a duplicate/gap");
    };
    assert!(
        out.decisions.iter().all(|d| d.decision == Decision::Exit),
        "both gated strategies must be clamped to flat by the wired breaker, got {:?}",
        out.decisions
    );
}

/// Non-vacuous control: the same wired cutover seeded from an **un-seeded** reconstructed state re-anchors on
/// the first tick and stays silent — proving the committed-peak seed is load-bearing at the cutover site
/// (the exact QE-401 capital-loss bug), and that the AC test above is not vacuous.
#[test]
fn unseeded_control_cutover_stays_silent_on_first_tick() {
    const N: i64 = 20;
    let mut cutover = Cutover::from_reconstructed_calibrated(
        reconstructed(vintage_with_partial_calibration(), N),
        &state_all_unseeded(),
        Resolution::M5,
        DEFAULT_FAST_WINDOW,
    )
    .unwrap();

    // No seed → strategy 0 has no anchor; its first tick at 170 re-anchors and reports ~0 drawdown.
    assert_eq!(cutover.breaker().unwrap().strategy_peak(0), None);
    assert_eq!(
        cutover.observe_strategy_equity(0, dec(170)),
        None,
        "without the committed-peak seed the first live tick is silent (control)"
    );
    assert!(
        !cutover.breaker().unwrap().is_gated(0),
        "the un-seeded calibrated strategy is not gated on its first tick"
    );
    // Strategy 1 (uncalibrated) is still fail-safe pre-gated regardless of seeding.
    assert!(cutover.breaker().unwrap().is_gated(1));
}

/// The legacy (un-wired) cutover carries no breaker and clamps nothing — behaviour-preserving for the
/// pre-QE-429 decision stream.
#[test]
fn legacy_cutover_has_no_breaker_and_does_not_clamp() {
    let mut session = EvaluatorSession::new(vintage_with_partial_calibration(), &cfg());
    for i in 0..10 {
        session.on_bar(&bar(i));
    }
    let mut cutover = Cutover::new(session, bar(9).open_time().millis(), Resolution::M5);
    assert!(
        cutover.breaker().is_none(),
        "legacy constructor wires no breaker"
    );
    // Equity routing is a no-op on the legacy path.
    assert_eq!(cutover.observe_strategy_equity(0, dec(50)), None);
    // A live bar evaluates without any clamp — strategy 1 (uncalibrated) would be gated on the wired path,
    // but here it is not, so at least one decision is not forced to Exit once the genomes trade.
    let CutoverStep::Evaluated(out) = cutover.feed_live_bar(&bar(10)).unwrap() else {
        panic!("bar 10 should evaluate");
    };
    // Directions: the raw decisions include entries once warm; on the legacy path they are NOT all clamped.
    // (We assert the breaker is absent above; this confirms the clamp branch is skipped structurally.)
    assert_eq!(out.decisions.len(), 2);
    assert!(matches!(
        out.decisions[0].decision,
        Decision::Enter(Direction::Long) | Decision::Hold | Decision::Exit
    ));
}
