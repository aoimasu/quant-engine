//! Cross-validated fold-isolation robustness *in the selection fitness path* (QE-415).
//!
//! The MAP-Elites / DE search historically scored a genome on `backtest(g, train_bars, cfg).elite_fitness()`
//! — the mean per-window [`log_growth`](crate::fitness::log_growth) over **contiguous** sub-windows of the
//! *entire* train series (one continuous backtest, returns sliced by `split_windows`). Those windows are
//! adjacent slices of the same series: a position opened in one window bleeds into the next, so selection
//! pressure rewarded a genome that only fit one contiguous stretch. The leakage-free
//! [`PurgedKFold`](crate::cv::PurgedKFold) object existed but was unused in selection.
//!
//! [`fold_isolation_fitness`] replaces that scalar with a **cross-validated fold-isolation** estimate: the
//! genome is scored on each of the `k` disjoint [`PurgedKFold`] **test** folds **in isolation** (flat start),
//! and the per-fold return series are reduced to `mean ± SE` exactly as [`NoiseRobustFitness::from_windows`]
//! does. Two properties make this more robust than the old whole-window number:
//!
//! 1. **Per-fold isolation** — each test block is backtested independently, flat-start, so no position or
//!    indicator state carries across a fold boundary (the exact carry the old contiguous windows leaked).
//! 2. **Leakage-free fold geometry** — the folds are built with the real feature lookback and a label horizon
//!    under the documented default embargo (= lookback), so every fold satisfies
//!    [`Fold::windows_disjoint`](crate::cv::Fold::windows_disjoint) (the QE-113 invariant).
//!
//! Aggregating per-fold `log_growth` with the standard-error penalty demotes a genome whose edge is
//! concentrated in a single fold below one that generalises across all `k` folds — the QE-415 acceptance
//! criterion. Fold construction is genome-independent, so [`fold_test_ranges`] is computed **once** and the
//! per-genome eval reuses it (no per-genome reallocation, and determinism is structural).
//!
//! ## This is NOT a held-out out-of-sample gate (do not over-trust it)
//!
//! Naming discipline (QE-415 review): despite the `PurgedKFold` machinery, **nothing is held out** here. The
//! `k` test folds *tile* the train window — their union is the whole train series — so every train bar is
//! scored in exactly one fold; no data is reserved. The purge/embargo only shapes each fold's (here **unused**)
//! *train* partition and defines the `windows_disjoint` invariant we verify; it does **not** gap or shrink the
//! scored **test** blocks, so the embargo is inert on the computed score. This function is therefore an
//! *in-window cross-validation robustness* signal (rewarding genomes consistent across disjoint, isolated
//! folds), **not** a true out-of-sample validation. The **G1 terminal holdout** (the final `holdout` bars,
//! purged from the train window) remains the **only true OOS gate** in the pipeline; this fitness does not
//! replace or weaken that boundary.

use std::ops::Range;

use crate::backtest::{backtest, BacktestConfig, Bar};
use crate::cv::PurgedKFold;
use crate::fitness::NoiseRobustFitness;
use crate::genome::Genome;

/// Default number of cross-validation folds the selection fitness scores over (`≥ 2` for a real
/// cross-validated standard error). Mirrors [`DEFAULT_WINDOWS`](crate::backtest::DEFAULT_WINDOWS).
pub const DEFAULT_CV_FOLDS: usize = 4;

/// Default label horizon (bars) for the selection folds. QE-120 realises a decision's P&L from the **next**
/// bar (fills at `i+1`, no same-bar fill), so the minimal information horizon of an evaluation is one bar.
pub const DEFAULT_LABEL_HORIZON: usize = 1;

/// The purged/embargoed k-fold scheme the selection fitness uses: `n_folds` (floored at 2 for a real SE)
/// balanced test blocks with the **documented default embargo = lookback** (QE-113/D5). The returned scheme
/// is what [`fold_test_ranges`] and the AC's `windows_disjoint` check operate on, so the geometry the fitness
/// consumes and the geometry the leakage-free invariant is proved on are one and the same. (The purge/embargo
/// shapes each fold's train partition and the disjointness invariant; the scored test blocks still tile the
/// window — see the module docs: nothing is held out.)
#[must_use]
pub fn selection_kfold(n_folds: usize, lookback: usize, label_horizon: usize) -> PurgedKFold {
    PurgedKFold::with_default_embargo(n_folds.max(2), lookback, label_horizon)
}

/// The `k` **test** ranges of `cv` over `0..n_bars`, in fold order. These tile the window (their union is
/// `0..n_bars`; nothing is held out). Genome-independent — compute once before the search and reuse for
/// every genome's [`fold_isolation_fitness`].
#[must_use]
pub fn fold_test_ranges(cv: &PurgedKFold, n_bars: usize) -> Vec<Range<usize>> {
    cv.folds(n_bars).into_iter().map(|f| f.test).collect()
}

/// Cross-validated fold-isolation fitness: backtest `genome` on each disjoint `test_ranges` fold **in
/// isolation** (flat start) and reduce the per-fold net-of-cost return series to `mean ± SE`
/// ([`NoiseRobustFitness::from_windows`]). The scalar the search selects on is the returned `.mean` — the
/// mean per-fold [`log_growth`](crate::fitness::log_growth), same units as the old `elite_fitness()`.
///
/// This is an *in-window cross-validation robustness* signal, **not** a held-out out-of-sample gate: the
/// folds tile the train window (nothing is reserved), so it rewards genomes that generalise across disjoint,
/// isolated folds rather than fitting one contiguous stretch. The G1 terminal holdout remains the only true
/// OOS gate (see module docs).
///
/// Determinism: pure in `(genome, bars, test_ranges, cfg)`; fixed fold order; no RNG. An empty `test_ranges`
/// (window shorter than the fold count) yields the neutral `{ mean: 0, n: 0 }`.
#[must_use]
pub fn fold_isolation_fitness(
    genome: &Genome,
    bars: &[Bar],
    test_ranges: &[Range<usize>],
    cfg: &BacktestConfig,
) -> NoiseRobustFitness {
    let windows: Vec<Vec<f64>> = test_ranges
        .iter()
        .map(|r| backtest(genome, &bars[r.clone()], cfg).returns)
        .collect();
    NoiseRobustFitness::from_windows(&windows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::backtest;
    use crate::genome::{
        Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
    };
    use qe_signal::{CatalogueConfig, FeatureSchema, FeatureVector, QState};
    use rust_decimal::Decimal;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    /// A long-only genome that enters when `feature`'s state is high `[3,4]` and exits after `hold` bars.
    fn long_on(feature: u16, hold: u16, size_bps: u16) -> Genome {
        let mut long = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        long[0] = Clause {
            enabled: true,
            feature,
            lo: 3,
            hi: 4,
        };
        let disabled = RuleSet {
            clauses: [Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            }; CLAUSES_PER_SET],
            min_satisfied: 1,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses: long,
                min_satisfied: 1,
            },
            short_entry: disabled,
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps },
        }
    }

    /// A bar whose feature 0 and feature 1 carry the given states (all other features absent).
    fn bar(schema: &FeatureSchema, i: i64, price: Decimal, state0: u16, state1: u16) -> Bar {
        let mut states = vec![None; schema.len()];
        states[0] = Some(QState::from_index(state0));
        states[1] = Some(QState::from_index(state1));
        Bar {
            features: FeatureVector {
                time_ms: i * 60_000,
                states,
            },
            price,
            funding_rate: None,
        }
    }

    fn cfg() -> BacktestConfig {
        // Selection cfg: low trade gate, and `.returns` (what the fold uses) is gate-independent.
        BacktestConfig {
            min_trades: 1,
            windows: 2,
            ..BacktestConfig::default()
        }
    }

    /// AC (a): an in-sample-overfit genome that is *great on the whole train window* but poor on the isolated
    /// CV folds must rank BELOW a genome that generalises across folds, under the new fitness.
    ///
    /// Construction: price rises +1/bar over the whole 120-bar window.
    /// - Feature 0 is high (=4) ONLY at bar 0 → the overfit genome `O` (entry on feature 0, huge hold) enters
    ///   exactly once at bar 0 and rides the entire uptrend fully invested — maximal whole-window growth, but
    ///   in the isolated folds 1..3 it never enters (feature 0 is never high there) ⇒ zero growth in 3 of 4
    ///   folds.
    /// - Feature 1 is high (=4) every 4th bar throughout → the robust genome `R` (entry on feature 1, short
    ///   hold) trades in every fold and captures the trend in each ⇒ consistent positive per-fold growth.
    #[test]
    fn overfit_genome_ranks_below_robust_under_fold_isolation() {
        let s = schema();
        let n = 120usize;
        let bars: Vec<Bar> = (0..n)
            .map(|i| {
                let state0 = if i == 0 { 4 } else { 0 }; // a single early "regime" only O exploits
                let state1 = if i % 4 == 0 { 4 } else { 0 }; // recurring signal R trades throughout
                bar(&s, i as i64, Decimal::from(100 + i as i64), state0, state1)
            })
            .collect();

        let overfit = long_on(0, 1_000, 10_000); // enters once, holds forever, full size
        let robust = long_on(1, 2, 5_000); // trades on the recurring signal in every fold

        // Whole-window in-sample fitness: the overfit "buy-and-hold" is at least as strong as the robust
        // genome (it is fully invested across the entire uptrend).
        let is_overfit = backtest(&overfit, &bars, &cfg()).elite_fitness();
        let is_robust = backtest(&robust, &bars, &cfg()).elite_fitness();
        assert!(
            is_overfit >= is_robust,
            "the overfit genome must look at least as good in-sample: overfit {is_overfit} < robust {is_robust}"
        );

        // Fold-isolation fitness over 4 folds: the overfit genome collapses (no trades in 3/4 folds) below the
        // robust genome, reversing the ranking the fix targets.
        let cv = selection_kfold(4, s.max_lookback(), DEFAULT_LABEL_HORIZON);
        let ranges = fold_test_ranges(&cv, n);
        let iso_overfit = fold_isolation_fitness(&overfit, &bars, &ranges, &cfg()).mean;
        let iso_robust = fold_isolation_fitness(&robust, &bars, &ranges, &cfg()).mean;
        assert!(
            iso_robust > iso_overfit,
            "fold isolation must demote the overfit genome: robust {iso_robust} !> overfit {iso_overfit}"
        );
    }

    /// AC (b): the CV folds the selection actually uses satisfy `windows_disjoint(lookback, label_horizon)`
    /// — every kept train bar's `[·−L, ·+H]` window is disjoint from every test bar's, the QE-113
    /// leakage-free invariant. Uses a non-degenerate configuration so the check is non-vacuous (folds have
    /// real, non-empty train sets).
    #[test]
    fn selection_folds_are_leakage_free() {
        let lookback = 5;
        let label_horizon = 2;
        let cv = selection_kfold(5, lookback, label_horizon);
        assert_eq!(
            cv.embargo, lookback,
            "documented default embargo = lookback"
        );

        let folds = cv.folds(200);
        assert_eq!(folds.len(), 5);
        for f in &folds {
            assert!(
                !f.train.is_empty(),
                "fold {:?} must have a real train set",
                f.test
            );
            assert!(
                f.windows_disjoint(lookback, label_horizon),
                "selection fold test {:?} leaks within the lookback+horizon span",
                f.test
            );
        }

        // The test ranges the fitness consumes are exactly these folds' test blocks, in order, and they
        // tile the window (no gaps/overlaps) — every bar is scored in exactly one fold (nothing held out).
        let ranges = fold_test_ranges(&cv, 200);
        assert_eq!(ranges.len(), folds.len());
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, 200);
        for pair in ranges.windows(2) {
            assert_eq!(
                pair[0].end, pair[1].start,
                "test folds must tile without gaps"
            );
        }
    }

    /// `n_folds` is floored at 2 so the fitness always has a real cross-validated SE, and an over-short
    /// window degrades gracefully to the neutral fitness rather than panicking.
    #[test]
    fn fold_count_floored_and_short_window_is_neutral() {
        let cv = selection_kfold(1, 3, 1);
        assert_eq!(cv.n_folds, 2, "fold count must be floored at 2");

        let empty = fold_test_ranges(&cv, 0);
        assert!(empty.is_empty());
        let s = schema();
        let neutral = fold_isolation_fitness(&long_on(0, 2, 5_000), &[], &empty, &cfg());
        assert_eq!((neutral.mean, neutral.n), (0.0, 0));
        let _ = s;
    }
}
