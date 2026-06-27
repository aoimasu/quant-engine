//! Purged + embargoed cross-validation (QE-113/D5).
//!
//! Plain k-fold leaks on autocorrelated series: a train bar's **feature lookback** or **label horizon**
//! window can reach into the test block, and serial correlation bleeds across the test→train boundary.
//! We therefore **purge** every train bar whose information window could overlap a test block — with
//! `purge = max_indicator_lookback + label_horizon` — and **embargo** a span immediately after each test
//! block (default = max lookback). The result is provably leakage-free: every kept train bar `tr` and
//! every test bar `te` satisfy `|tr − te| > lookback + label_horizon`, so their windows are disjoint
//! ([`Fold::windows_disjoint`]).

use std::ops::Range;

/// A purged + embargoed k-fold scheme over a contiguous bar index range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgedKFold {
    /// Number of contiguous test folds.
    pub n_folds: usize,
    /// Max indicator lookback (bars) — the feature-dependency span (QE-107 `max_lookback`).
    pub lookback: usize,
    /// Label horizon (bars) — how far ahead an evaluation's outcome reaches.
    pub label_horizon: usize,
    /// Embargo (bars) excluded from train immediately after each test block (QE-113/D5; default = `lookback`).
    pub embargo: usize,
}

/// One CV split: a contiguous `test` block and the purged/embargoed `train` indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    /// The contiguous test bar range `[start, end)`.
    pub test: Range<usize>,
    /// Train bar indices, with the purge + embargo zone around `test` removed.
    pub train: Vec<usize>,
}

impl PurgedKFold {
    /// Build a scheme with an explicit embargo.
    #[must_use]
    pub fn new(n_folds: usize, lookback: usize, label_horizon: usize, embargo: usize) -> Self {
        PurgedKFold {
            n_folds,
            lookback,
            label_horizon,
            embargo,
        }
    }

    /// Build a scheme with the **documented default embargo = max lookback** (QE-113/D5).
    #[must_use]
    pub fn with_default_embargo(n_folds: usize, lookback: usize, label_horizon: usize) -> Self {
        PurgedKFold::new(n_folds, lookback, label_horizon, lookback)
    }

    /// The purge span = `lookback + label_horizon` (QE-113/D5).
    #[must_use]
    pub fn purge(&self) -> usize {
        self.lookback + self.label_horizon
    }

    /// Generate the folds over `0..n_bars`. Test blocks partition the range into `n_folds` balanced
    /// contiguous pieces; each fold's train is every bar **outside** `[test_start − purge,
    /// test_end + purge + embargo)`, so train/test information windows are provably disjoint.
    #[must_use]
    pub fn folds(&self, n_bars: usize) -> Vec<Fold> {
        let folds = self.n_folds.max(1);
        let purge = self.purge();
        let mut out = Vec::with_capacity(folds);
        for k in 0..folds {
            // Balanced contiguous partition: block k = [k·n/folds, (k+1)·n/folds).
            let t_start = k * n_bars / folds;
            let t_end = (k + 1) * n_bars / folds;
            if t_start >= t_end {
                continue; // empty block (n_bars < n_folds) — skip
            }
            let excl_lo = t_start.saturating_sub(purge);
            let excl_hi = (t_end + purge + self.embargo).min(n_bars);
            let train: Vec<usize> = (0..n_bars)
                .filter(|&i| i < excl_lo || i >= excl_hi)
                .collect();
            out.push(Fold {
                test: t_start..t_end,
                train,
            });
        }
        out
    }
}

impl Fold {
    /// Whether every (train, test) pair has `|tr − te| > lookback + label_horizon` — i.e. their
    /// information windows `[·−lookback, ·+label_horizon]` are disjoint, including the lookback. This is
    /// the leakage-free invariant purging guarantees (and that naive k-fold violates).
    #[must_use]
    pub fn windows_disjoint(&self, lookback: usize, label_horizon: usize) -> bool {
        let span = lookback + label_horizon;
        self.train
            .iter()
            .all(|&tr| self.test.clone().all(|te| tr.abs_diff(te) > span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_partition_test_blocks_and_exclude_purge_embargo() {
        let cv = PurgedKFold::new(4, 3, 1, 2); // purge = 4, embargo = 2
        let folds = cv.folds(40);
        assert_eq!(folds.len(), 4);
        // Test blocks tile [0,40) with no gaps/overlaps.
        assert_eq!(folds[0].test, 0..10);
        assert_eq!(folds[3].test, 30..40);
        for f in &folds {
            // train ∩ test = ∅.
            for te in f.test.clone() {
                assert!(!f.train.contains(&te));
            }
            // Embargo region [test_end, test_end + embargo) is absent from train.
            for i in f.test.end..(f.test.end + cv.embargo).min(40) {
                assert!(!f.train.contains(&i), "embargo bar {i} leaked into train");
            }
        }
    }

    #[test]
    fn train_test_windows_are_provably_disjoint_including_lookback() {
        // The AC: across every fold, no train bar's [i−L, i+H] window overlaps any test bar's window.
        let lookback = 5;
        let label_horizon = 2;
        let cv = PurgedKFold::with_default_embargo(5, lookback, label_horizon);
        assert_eq!(cv.embargo, lookback); // documented default
        assert_eq!(cv.purge(), lookback + label_horizon);

        let folds = cv.folds(120);
        assert_eq!(folds.len(), 5);
        for f in &folds {
            assert!(!f.train.is_empty());
            assert!(
                f.windows_disjoint(lookback, label_horizon),
                "fold test {:?} has a train bar within the lookback+horizon span",
                f.test
            );
        }
    }

    #[test]
    fn naive_kfold_without_purge_leaks() {
        // Same geometry with no purge / embargo = standard k-fold: adjacent train/test bars whose
        // lookback windows overlap → windows_disjoint must be FALSE (this is why we reject k-fold).
        let lookback = 5;
        let label_horizon = 2;
        let naive = PurgedKFold::new(5, 0, 0, 0); // no purge, no embargo
        let folds = naive.folds(120);
        let any_leak = folds
            .iter()
            .any(|f| !f.windows_disjoint(lookback, label_horizon));
        assert!(
            any_leak,
            "naive k-fold should leak under a non-zero lookback"
        );
        // Concretely: fold 1 starts at 24, so train bar 23 sits adjacent to test bar 24.
        assert!(folds[1].train.contains(&23));
        assert!(folds[1].test.contains(&24));
    }

    #[test]
    fn fewer_bars_than_folds_skips_empty_blocks() {
        let cv = PurgedKFold::new(10, 1, 0, 1);
        let folds = cv.folds(3); // only 3 non-empty blocks possible
        assert!(folds.iter().all(|f| !f.test.is_empty()));
        assert!(folds.len() <= 3);
    }
}
