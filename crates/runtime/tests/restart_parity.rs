//! QE-220 — Bootstrap/restart parity test.
//!
//! *(Reviewer-added rationale.)* If the state a runtime **reconstructs on restart** diverges from the state a
//! **continuously-running** engine accumulates, every drawdown breaker is mis-anchored — a capital-risk event.
//! This test asserts the two agree **bit-for-bit** on the breaker-relevant fields (committed peak, dormancy
//! latch, position) per strategy.
//!
//! The reconstructed side is the production restart path, `ReconstructedState::from_replay`. The continuous
//! side is an **independent** reference implementation (a plain running max + a plain entered-flag), so
//! agreement is a genuine parity check rather than a tautology — if `from_replay`'s committed peak ever
//! regressed to a trailing window (the exact bug this guards), the true-max reference would diverge and this
//! test would fail.

use qe_determinism::Lineage;
use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
use qe_risk::{CalibrationProfile, Fraction};
use qe_runtime::boot_state::{BootStateError, DormancyLatch, ReconstructedState, StrategyState};
use qe_runtime::evaluator::{EvalOutput, EvaluatorSession};
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
fn p(n: i64) -> Price {
    Price::new(Decimal::from(n)).unwrap()
}
fn q(n: i64) -> Qty {
    Qty::new(Decimal::from(n)).unwrap()
}
fn dec(n: i64) -> Decimal {
    Decimal::from(n)
}

fn off_clause() -> Clause {
    Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    }
}

/// A genome whose entry clause spans the whole feature range → it fires every bar and **trades**.
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

/// A genome with no active clauses → it never fires and stays **dormant**.
fn dormant_genome() -> Genome {
    Genome {
        version: REP_VERSION,
        long_entry: RuleSet {
            clauses: [off_clause(); CLAUSES_PER_SET],
            min_satisfied: 1,
        },
        short_entry: RuleSet {
            clauses: [off_clause(); CLAUSES_PER_SET],
            min_satisfied: 1,
        },
        exit: ExitParams {
            max_holding_bars: 3,
            exit_on_opposite: false,
        },
        risk: RiskParams { size_bps: 5_000 },
    }
}

/// A sealed 2-chromosome vintage: index 0 trades, index 1 stays dormant.
fn vintage() -> Vintage {
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: "qe-220-parity".to_owned(),
        chromosomes: vec![cycling_genome(3), dormant_genome()],
        weights: vec![0.5, 0.5],
        calibration: CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()),
        worst_case_loss: Some(0.2),
        lineage: Lineage::new("cfg", "snap", "commit", vec![1]),
    };
    Vintage::seal(content).unwrap()
}

/// A base 5m bar at index `i` (QE-209 fixture shape).
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

/// Run one session over `n` bars; return the per-bar decision trace and the final positions.
fn replay(n: i64) -> (Vec<EvalOutput>, Vec<PositionState>) {
    let mut session = EvaluatorSession::new(vintage(), &cfg());
    let outputs: Vec<EvalOutput> = (0..n).map(|i| session.on_bar(&bar(i))).collect();
    (outputs, session.positions().to_vec())
}

/// Rise-then-fall per-strategy equity paths: the true peak is an **interior** value the final sample is below,
/// so a trailing-window max would get it wrong.
fn equity_paths() -> Vec<Vec<Decimal>> {
    vec![
        vec![dec(100), dec(150), dec(120)], // strategy 0: peak 150, ends at 120
        vec![dec(100), dec(130), dec(90)],  // strategy 1: peak 130, ends at 90
    ]
}

/// The **independent** continuously-accumulated reference: a plain running max for the committed peak, a plain
/// entered-flag for dormancy, and the session's final positions — deliberately *not* calling `CommittedPeak`
/// or `from_replay`, so agreement is a real parity check.
fn continuous_state(
    outputs: &[EvalOutput],
    final_positions: &[PositionState],
    equity_paths: &[Vec<Decimal>],
) -> ReconstructedState {
    let n = final_positions.len();
    let mut entered = vec![false; n];
    for out in outputs {
        for cd in &out.decisions {
            if matches!(cd.decision, Decision::Enter(_)) {
                entered[cd.index] = true;
            }
        }
    }
    let strategies = (0..n)
        .map(|i| {
            let mut peak: Option<Decimal> = None;
            for &e in &equity_paths[i] {
                peak = Some(peak.map_or(e, |p| p.max(e)));
            }
            StrategyState {
                index: i,
                position: final_positions[i],
                dormancy: if entered[i] {
                    DormancyLatch::active()
                } else {
                    DormancyLatch::dormant()
                },
                committed_peak_equity: peak,
            }
        })
        .collect();
    ReconstructedState { strategies }
}

/// AC: reconstructed (cold-restart) state matches continuously-accumulated state bit-for-bit.
#[test]
fn reconstructed_state_matches_continuous_bit_for_bit() {
    let (outputs, positions) = replay(40);
    let paths = equity_paths();

    let continuous = continuous_state(&outputs, &positions, &paths);
    let reconstructed = ReconstructedState::from_replay(&positions, &outputs, &paths).unwrap();

    assert_eq!(
        reconstructed, continuous,
        "restart-reconstructed state must equal continuously-accumulated state on all breaker-relevant fields"
    );
}

/// The capital-risk guard: the committed peak is the true all-time max, not the declining tail — in **both**
/// derivations.
#[test]
fn committed_peak_is_true_all_time_max_not_trailing() {
    let (outputs, positions) = replay(40);
    let paths = equity_paths();
    let reconstructed = ReconstructedState::from_replay(&positions, &outputs, &paths).unwrap();

    // Strategy 0's path peaks at 150 mid-series and ends at 120; the peak must be 150, above the final sample.
    let s0 = &reconstructed.strategies[0];
    assert_eq!(s0.committed_peak_equity, Some(dec(150)));
    assert!(
        s0.committed_peak_equity.unwrap() > *paths[0].last().unwrap(),
        "an early peak must survive a later decline (a trailing window would lose it)"
    );
    assert_eq!(
        reconstructed.strategies[1].committed_peak_equity,
        Some(dec(130))
    );
}

/// Dormancy latches agree for a traded strategy (active, non-flat) and an untraded one (dormant).
#[test]
fn dormancy_latches_match_for_traded_and_untraded() {
    let (outputs, positions) = replay(40);
    let paths = equity_paths();
    let reconstructed = ReconstructedState::from_replay(&positions, &outputs, &paths).unwrap();

    // Strategy 0 traded → active and its final position is non-flat (it genuinely entered).
    assert!(!reconstructed.strategies[0].dormancy.is_dormant());
    assert_ne!(
        reconstructed.strategies[0].position,
        PositionState::flat(),
        "the trading strategy must actually hold a position (non-vacuous trace)"
    );
    // Strategy 1 never fired → dormant and flat.
    assert!(reconstructed.strategies[1].dormancy.is_dormant());
    assert_eq!(reconstructed.strategies[1].position, PositionState::flat());
}

/// The reconstruction rejects a mismatched equity-path count (one per strategy is a precondition).
#[test]
fn mismatched_equity_paths_are_rejected() {
    let (outputs, positions) = replay(4);
    let only_one = vec![vec![dec(100), dec(150)]]; // 1 path for 2 strategies

    let err = ReconstructedState::from_replay(&positions, &outputs, &only_one).unwrap_err();
    assert_eq!(
        err,
        BootStateError::MismatchedEquityPaths {
            strategies: 2,
            paths: 1,
        }
    );
}
