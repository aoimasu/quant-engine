//! Reconstructed state — the cold-start anchor the live planner resumes from (QE-210).
//!
//! The bootstrap replay (QE-209) warms an [`EvaluatorSession`](crate::evaluator::EvaluatorSession) and
//! produces a per-bar decision trace. QE-210 turns that into an explicit **reconstructed state**: for each
//! strategy (chromosome) its current [`PositionState`], a [`DormancyLatch`], and its **committed peak
//! equity** — the all-time maximum of the strategy's equity path.
//!
//! The committed peak is **load-bearing for drawdown breakers** (QE-116/QE-212): the breaker anchors total
//! drawdown on an all-time equity peak, so on live start that anchor must be the *true* peak over the whole
//! history — a *windowed* peak (e.g. a trailing max) would under-anchor the drawdown and mis-fire the
//! breaker. [`CommittedPeak`] therefore folds the entire equity path into a monotone running maximum, never
//! a window (the AC).
//!
//! **Equity-path boundary.** The per-strategy equity *series* is an input here, not computed: a faithful
//! live equity curve is net-of-cost (fees + funding, QE-109) marked against real fills, which — with the
//! live equity feed — belongs to QE-212/QE-217; recomputing a gross mark-to-market here would duplicate the
//! QE-120 backtester and risk train/live drift. QE-210 owns the state model + true-peak correctness;
//! positions and dormancy are wired to the real replay output.

use rust_decimal::Decimal;
use thiserror::Error;

use qe_signal::{Decision, PositionState};

use crate::evaluator::EvalOutput;

/// A reconstructed-state failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BootStateError {
    /// The number of equity paths did not match the number of strategies (positions).
    #[error("expected {strategies} equity paths (one per strategy), got {paths}")]
    MismatchedEquityPaths {
        /// Strategy (position) count.
        strategies: usize,
        /// Equity-path count supplied.
        paths: usize,
    },
}

/// The **true**, all-time running maximum of an equity path — never a trailing window.
///
/// This is the anti-mis-anchoring primitive: over a path longer than any window whose maximum is early and
/// then declines, [`peak`](Self::peak) still returns the early true peak (a trailing-window max would lose
/// it), so the breaker's drawdown anchor is seeded correctly on live start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommittedPeak {
    peak: Option<Decimal>,
}

impl CommittedPeak {
    /// An empty accumulator (no samples yet).
    #[must_use]
    pub fn new() -> Self {
        Self { peak: None }
    }

    /// Fold one equity sample into the running maximum.
    pub fn observe(&mut self, equity: Decimal) {
        self.peak = Some(match self.peak {
            Some(p) => p.max(equity),
            None => equity,
        });
    }

    /// The all-time peak seen so far, or `None` if no sample has been observed.
    #[must_use]
    pub fn peak(&self) -> Option<Decimal> {
        self.peak
    }

    /// The committed peak over an entire equity `series` (the whole path, not a window).
    #[must_use]
    pub fn from_series(series: &[Decimal]) -> Self {
        let mut c = Self::new();
        for &e in series {
            c.observe(e);
        }
        c
    }
}

/// A strategy's dormancy latch.
///
/// Cold-start semantic: a strategy is reconstructed **dormant** iff it never held a position across the
/// replay (emitted no [`Decision::Enter`]) — it has made no committed exposure. The live planner resumes a
/// dormant strategy at its seed anchor until it fires. (QE-212's breaker layer may *additionally* latch
/// dormancy on a gate/trip; that is not this ticket.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DormancyLatch {
    dormant: bool,
}

impl DormancyLatch {
    /// An active (non-dormant) strategy.
    #[must_use]
    pub fn active() -> Self {
        Self { dormant: false }
    }

    /// A dormant strategy.
    #[must_use]
    pub fn dormant() -> Self {
        Self { dormant: true }
    }

    /// Whether the strategy is dormant.
    #[must_use]
    pub fn is_dormant(&self) -> bool {
        self.dormant
    }

    /// Clear the latch — the strategy is active.
    pub fn activate(&mut self) {
        self.dormant = false;
    }
}

/// One strategy's reconstructed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrategyState {
    /// Index into the vintage's chromosomes / weights.
    pub index: usize,
    /// The strategy's current position at the end of the replay.
    pub position: PositionState,
    /// Whether the strategy is dormant (never committed during the replay).
    pub dormancy: DormancyLatch,
    /// The true, all-time committed peak equity over the replay (`None` if the equity path was empty).
    pub committed_peak_equity: Option<Decimal>,
}

/// The full reconstructed state handed to the cutover (QE-211).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructedState {
    /// Per-strategy state, aligned to the vintage's chromosomes.
    pub strategies: Vec<StrategyState>,
}

impl ReconstructedState {
    /// Assemble the reconstructed state from the replay outputs:
    /// - `positions` — the session's final per-chromosome positions
    ///   ([`EvaluatorSession::positions`](crate::evaluator::EvaluatorSession::positions));
    /// - `decisions` — the per-bar decision trace (used to derive dormancy: a strategy that never emitted
    ///   an [`Decision::Enter`] is dormant);
    /// - `equity_paths` — one per-strategy equity series (see the module-level equity-path boundary), folded
    ///   into each strategy's true committed peak.
    ///
    /// # Errors
    /// [`BootStateError::MismatchedEquityPaths`] if `equity_paths.len() != positions.len()`.
    pub fn from_replay(
        positions: &[PositionState],
        decisions: &[EvalOutput],
        equity_paths: &[Vec<Decimal>],
    ) -> Result<Self, BootStateError> {
        if equity_paths.len() != positions.len() {
            return Err(BootStateError::MismatchedEquityPaths {
                strategies: positions.len(),
                paths: equity_paths.len(),
            });
        }

        // A strategy is dormant unless it entered a position at least once during the replay.
        let mut traded = vec![false; positions.len()];
        for output in decisions {
            for cd in &output.decisions {
                if matches!(cd.decision, Decision::Enter(_)) {
                    if let Some(flag) = traded.get_mut(cd.index) {
                        *flag = true;
                    }
                }
            }
        }

        let strategies = positions
            .iter()
            .enumerate()
            .map(|(index, &position)| {
                let dormancy = if traded[index] {
                    DormancyLatch::active()
                } else {
                    DormancyLatch::dormant()
                };
                StrategyState {
                    index,
                    position,
                    dormancy,
                    committed_peak_equity: CommittedPeak::from_series(&equity_paths[index]).peak(),
                }
            })
            .collect();

        Ok(ReconstructedState { strategies })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{ChromosomeDecision, SessionMode};
    use qe_domain::Direction;

    fn dec(v: i64) -> Decimal {
        Decimal::from(v)
    }

    /// Convenience: an `EvalOutput` at `time_ms` from `(index, decision)` pairs.
    fn output(time_ms: i64, decisions: &[(usize, Decision)]) -> EvalOutput {
        EvalOutput {
            time_ms,
            mode: SessionMode::Replay,
            decisions: decisions
                .iter()
                .map(|&(index, decision)| ChromosomeDecision { index, decision })
                .collect(),
        }
    }

    /// A trailing-window maximum over the last `window` samples ending at each point — the *wrong* peak the
    /// AC guards against. Used only to demonstrate it diverges from the true committed peak.
    fn trailing_window_max(series: &[Decimal], window: usize) -> Decimal {
        let start = series.len().saturating_sub(window);
        series[start..]
            .iter()
            .copied()
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// **The AC.** The committed peak is the true all-time peak over a path longer than any window — even
    /// when the peak is early and equity declines after — and it differs from a trailing-window max.
    #[test]
    fn committed_peak_is_true_all_time_not_windowed() {
        // Equity rises to an early peak of 150 (bar 2), then declines for the rest of a long path.
        let series = vec![
            dec(100),
            dec(130),
            dec(150), // the true peak, early
            dec(140),
            dec(135),
            dec(120),
            dec(110),
            dec(105),
            dec(100),
            dec(98),
        ];
        let peak = CommittedPeak::from_series(&series).peak().unwrap();
        assert_eq!(
            peak,
            dec(150),
            "committed peak is the true all-time maximum"
        );

        // A trailing window shorter than the path would anchor on a *lower* recent max — the mis-anchoring
        // the AC forbids. Prove the two genuinely differ, so the test isn't vacuous.
        let windowed = trailing_window_max(&series, 4);
        assert!(
            windowed < peak,
            "a windowed peak ({windowed}) under-anchors vs the true peak ({peak})"
        );
    }

    /// Incremental `observe` equals `from_series`.
    #[test]
    fn committed_peak_observe_matches_from_series() {
        let series = vec![dec(10), dec(50), dec(30), dec(50), dec(20)];
        let mut c = CommittedPeak::new();
        assert_eq!(c.peak(), None);
        for &e in &series {
            c.observe(e);
        }
        assert_eq!(c.peak(), CommittedPeak::from_series(&series).peak());
        assert_eq!(c.peak(), Some(dec(50)));
    }

    /// Dormancy latch basics.
    #[test]
    fn dormancy_latch_basics() {
        assert!(DormancyLatch::dormant().is_dormant());
        assert!(!DormancyLatch::active().is_dormant());
        let mut d = DormancyLatch::dormant();
        d.activate();
        assert!(!d.is_dormant());
    }

    /// `from_replay` assembles positions, dormancy (never-traded ⇒ dormant), and true committed peaks.
    #[test]
    fn from_replay_assembles_positions_dormancy_and_peak() {
        // Two strategies. #0 enters long during replay (active); #1 never enters (dormant).
        let positions = vec![
            PositionState::held(Direction::Long, 3),
            PositionState::flat(),
        ];
        let decisions = vec![
            output(1_000, &[(0, Decision::Hold), (1, Decision::Hold)]),
            output(
                2_000,
                &[(0, Decision::Enter(Direction::Long)), (1, Decision::Hold)],
            ),
            output(3_000, &[(0, Decision::Hold), (1, Decision::Hold)]),
        ];
        // #0's equity peaks early then declines; #1 stays flat at seed.
        let equity_paths = vec![
            vec![dec(100), dec(120), dec(110), dec(105)],
            vec![dec(100), dec(100), dec(100), dec(100)],
        ];

        let state = ReconstructedState::from_replay(&positions, &decisions, &equity_paths).unwrap();
        assert_eq!(state.strategies.len(), 2);

        let s0 = &state.strategies[0];
        assert_eq!(s0.index, 0);
        assert_eq!(s0.position, PositionState::held(Direction::Long, 3));
        assert!(!s0.dormancy.is_dormant(), "strategy 0 traded → active");
        assert_eq!(s0.committed_peak_equity, Some(dec(120)));

        let s1 = &state.strategies[1];
        assert_eq!(s1.position, PositionState::flat());
        assert!(
            s1.dormancy.is_dormant(),
            "strategy 1 never traded → dormant"
        );
        assert_eq!(s1.committed_peak_equity, Some(dec(100)));
    }

    /// A wrong number of equity paths is a clear error, not a panic or silent truncation.
    #[test]
    fn from_replay_rejects_mismatched_equity_paths() {
        let positions = vec![PositionState::flat(), PositionState::flat()];
        let decisions: Vec<EvalOutput> = vec![];
        let equity_paths = vec![vec![dec(100)]]; // only 1 path for 2 strategies
        let err =
            ReconstructedState::from_replay(&positions, &decisions, &equity_paths).unwrap_err();
        assert_eq!(
            err,
            BootStateError::MismatchedEquityPaths {
                strategies: 2,
                paths: 1,
            }
        );
    }

    /// An empty equity path yields `None` committed peak (no samples), not a panic.
    #[test]
    fn empty_equity_path_yields_no_peak() {
        let positions = vec![PositionState::flat()];
        let decisions: Vec<EvalOutput> = vec![];
        let equity_paths = vec![vec![]];
        let state = ReconstructedState::from_replay(&positions, &decisions, &equity_paths).unwrap();
        assert_eq!(state.strategies[0].committed_peak_equity, None);
        assert!(state.strategies[0].dormancy.is_dormant());
    }
}
