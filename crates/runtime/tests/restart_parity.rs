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

/// The number of samples in each equity path. Deliberately **longer than any plausible breaker drawdown
/// window** so the guard below actually bites: with the true peak at index 0 (below), a trailing window of
/// *any* size short of the full path excludes the peak and would compute a smaller max than the true all-time
/// maximum — diverging from the independent plain-max reference. A short interior-peak path (e.g. length 3,
/// peak at index 1) would only catch the degenerate `window == 1` case, not a realistically sized window.
const EQUITY_LEN: usize = 20;

/// A peak-first, monotonically-declining equity path: `peak` at index 0, then `peak - drop*k`. The true
/// all-time max is the index-0 value, which every trailing window short of the full length misses.
fn declining_from(peak: i64, drop: i64) -> Vec<Decimal> {
    (0..EQUITY_LEN)
        .map(|k| dec(peak - drop * k as i64))
        .collect()
}

/// Per-strategy equity paths whose true peak is the **first** sample, followed by a long decline — so a
/// trailing-window max of any window shorter than the path diverges from the true all-time max.
fn equity_paths() -> Vec<Vec<Decimal>> {
    vec![
        declining_from(150, 2), // strategy 0: peak 150 at index 0, declines to 150 - 2*19 = 112
        declining_from(130, 2), // strategy 1: peak 130 at index 0, declines to 130 - 2*19 = 92
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

/// The capital-risk guard: the committed peak is the true all-time max (the index-0 value), not any trailing
/// window of the declining tail — and the path is built so *any* window short of the full length would
/// diverge, so the guard actually bites for a realistically sized breaker window (not just `window == 1`).
#[test]
fn committed_peak_is_true_all_time_max_not_trailing() {
    let (outputs, positions) = replay(40);
    let paths = equity_paths();
    let reconstructed = ReconstructedState::from_replay(&positions, &outputs, &paths).unwrap();

    // Strategy 0's path peaks at 150 at index 0, then declines monotonically; the peak must be that 150.
    let s0 = &reconstructed.strategies[0];
    assert_eq!(s0.committed_peak_equity, Some(dec(150)));
    assert_eq!(paths[0][0], dec(150), "the true peak is the first sample");
    assert!(
        s0.committed_peak_equity.unwrap() > *paths[0].last().unwrap(),
        "the index-0 peak must survive the whole decline (a trailing window would lose it)"
    );
    assert_eq!(
        reconstructed.strategies[1].committed_peak_equity,
        Some(dec(130))
    );

    // Prove the path genuinely defeats a windowed regression: a trailing window of *any* size short of the
    // full length excludes the index-0 peak, so its max is strictly below the true all-time max. This is what
    // makes the parity check bite for a realistically sized breaker window, not only the degenerate window==1.
    for window in 1..EQUITY_LEN {
        let trailing_max = paths[0][EQUITY_LEN - window..]
            .iter()
            .copied()
            .max()
            .unwrap();
        assert!(
            trailing_max < dec(150),
            "a trailing window of {window} would report {trailing_max}, below the true peak 150 — so a \
             windowed regression of from_replay would diverge from the plain-max reference and fail parity"
        );
    }
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
