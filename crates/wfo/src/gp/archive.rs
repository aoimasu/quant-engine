//! QE-451 Phase 1a — the separate `Elite<ExprTree>` MAP-Elites archive (QE-450 §4.5).
//!
//! A **separate** archive from the strategy [`MapElitesArchive`](crate::mapelites::MapElitesArchive):
//! only the descriptor-band math and the Deep-Grid pattern are reused, not the storage. Each niche is a
//! Deep-Grid sub-population bounded to [`SUBPOP_SIZE`]. Parent sampling ([`ExprArchive::sample_parent`])
//! draws a **non-empty cell uniformly** then an elite within it — sparse niches reproduce as often as
//! crowded ones. **In-sample behavioural dedup** rejects an offspring whose quantised series
//! Pearson-correlates `> DEDUP_THRESHOLD` with an existing elite in its target cell (firewall-safe:
//! in-sample only, never an out-of-sample signal).

use std::collections::BTreeMap;

use qe_determinism::DetRng;
use qe_signal::indicator::expr::ExprTree;
use rand_core::RngCore;

use crate::archive::SUBPOP_SIZE;
use crate::gp::descriptor::{descriptor_for_tree, ExprCell};

/// In-sample behavioural-dedup correlation threshold (§4.5): reject an offspring whose quantised series
/// correlates above this with an existing elite in its target cell.
pub const DEDUP_THRESHOLD: f64 = 0.95;

/// A stored tree elite: the repaired tree, its scalar illumination fitness, its cached canonical hash,
/// and its quantised behavioural series (`None` during warmup) — the last used for behavioural dedup and
/// as the pinned canonical eval vector.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprElite {
    /// The repaired genotype.
    pub tree: ExprTree,
    /// Illumination fitness (a score, not money — orders elites; never feeds a hash).
    pub fitness: f64,
    /// Canonical content hash (constant-folded / order-normalised / rank-monotone-collapsed).
    pub hash: String,
    /// Quantised state series over the sample window (`None` while the tree is warming up).
    pub series: Vec<Option<i64>>,
}

/// The outcome of offering an offspring to the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprInsert {
    /// Filled a previously-empty niche.
    NewCell,
    /// Joined a non-full Deep-Grid cell.
    Added,
    /// Displaced the worst elite of a full cell (strictly better fitness).
    ImprovedElite,
    /// Full cell, not better than its worst elite.
    Rejected,
    /// Rejected by in-sample behavioural dedup (`> DEDUP_THRESHOLD` correlation with a cell elite).
    DedupRejected,
}

/// Pearson correlation over the paired warm (`Some`) entries of two quantised series. Returns `0.0` when
/// fewer than two overlapping points or either side has zero variance (so a constant signal never reads
/// as a duplicate). Deterministic; `f64` here only classifies dedup — it never feeds a hash.
#[must_use]
pub fn quantised_correlation(a: &[Option<i64>], b: &[Option<i64>]) -> f64 {
    let paired: Vec<(f64, f64)> = a
        .iter()
        .zip(b.iter())
        .filter_map(|(x, y)| match (x, y) {
            (Some(x), Some(y)) => Some((*x as f64, *y as f64)),
            _ => None,
        })
        .collect();
    let n = paired.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean_x = paired.iter().map(|(x, _)| x).sum::<f64>() / nf;
    let mean_y = paired.iter().map(|(_, y)| y).sum::<f64>() / nf;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (x, y) in &paired {
        let dx = x - mean_x;
        let dy = y - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    if var_x <= 0.0 || var_y <= 0.0 {
        return 0.0;
    }
    cov / (var_x.sqrt() * var_y.sqrt())
}

/// The separate `Elite<ExprTree>` MAP-Elites archive: a sparse map from [`ExprCell`] to its Deep-Grid
/// sub-population.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExprArchive {
    cells: BTreeMap<ExprCell, Vec<ExprElite>>,
}

impl ExprArchive {
    /// An empty archive.
    #[must_use]
    pub fn new() -> Self {
        ExprArchive::default()
    }

    /// Offer `elite` at its structural cell. Applies behavioural dedup first, then Deep-Grid replacement.
    pub fn insert(&mut self, elite: ExprElite) -> ExprInsert {
        let cell = descriptor_for_tree(&elite.tree);
        let sub = self.cells.entry(cell).or_default();

        // In-sample behavioural dedup against the target cell's existing elites.
        for existing in sub.iter() {
            if quantised_correlation(&existing.series, &elite.series) > DEDUP_THRESHOLD {
                return ExprInsert::DedupRejected;
            }
        }

        if sub.is_empty() {
            sub.push(elite);
            return ExprInsert::NewCell;
        }
        if sub.len() < SUBPOP_SIZE {
            sub.push(elite);
            return ExprInsert::Added;
        }
        // Full cell: displace the worst (min fitness, ties → lowest index) iff strictly better.
        let worst = sub
            .iter()
            .enumerate()
            .min_by(|(ia, a), (ib, b)| a.fitness.total_cmp(&b.fitness).then_with(|| ia.cmp(ib)))
            .map(|(i, _)| i)
            .expect("non-empty cell has a worst elite");
        if elite.fitness > sub[worst].fitness {
            sub[worst] = elite;
            ExprInsert::ImprovedElite
        } else {
            ExprInsert::Rejected
        }
    }

    /// Deep-Grid parent sampling: a non-empty cell uniformly, then an elite within it uniformly.
    #[must_use]
    pub fn sample_parent(&self, rng: &mut DetRng) -> Option<&ExprElite> {
        if self.cells.is_empty() {
            return None;
        }
        let cell_idx = (rng.next_u64() % self.cells.len() as u64) as usize;
        let sub = self.cells.values().nth(cell_idx)?;
        if sub.is_empty() {
            return None;
        }
        let elite_idx = (rng.next_u64() % sub.len() as u64) as usize;
        sub.get(elite_idx)
    }

    /// The occupied cells, in deterministic (sorted) order.
    pub fn occupied_cells(&self) -> impl Iterator<Item = &ExprCell> {
        self.cells.keys()
    }

    /// Number of occupied niches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether no niche is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Total elites across all cells.
    #[must_use]
    pub fn total_elites(&self) -> usize {
        self.cells.values().map(Vec::len).sum()
    }

    /// The sub-population at `cell`, if occupied.
    #[must_use]
    pub fn cell(&self, cell: &ExprCell) -> Option<&[ExprElite]> {
        self.cells.get(cell).map(Vec::as_slice)
    }

    /// The highest-fitness elite in `cell`, if any.
    #[must_use]
    pub fn best_in(&self, cell: &ExprCell) -> Option<&ExprElite> {
        self.cells
            .get(cell)?
            .iter()
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::task_rng;
    use qe_signal::indicator::expr::{Expr, Field, WinOp};

    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }

    fn elite(root: Expr, fitness: f64, series: Vec<Option<i64>>) -> ExprElite {
        let tree = ExprTree::repaired(root);
        let hash = tree.canonical_hash();
        ExprElite {
            tree,
            fitness,
            hash,
            series,
        }
    }

    fn win(op: WinOp, f: Field, n: usize) -> Expr {
        Expr::Window(op, boxed(Expr::Input(f)), n)
    }

    // A distinct behavioural series per test elite (uncorrelated) unless we want a dup.
    fn series(pattern: &[i64]) -> Vec<Option<i64>> {
        pattern.iter().map(|v| Some(*v)).collect()
    }

    #[test]
    fn fills_distinct_niches() {
        let mut arc = ExprArchive::new();
        // Volatility, Momentum, Trend, Volume families in distinct cells.
        arc.insert(elite(
            win(WinOp::Std, Field::Close, 20),
            1.0,
            series(&[0, 1, 2, 3]),
        ));
        arc.insert(elite(
            win(WinOp::Delta, Field::Close, 10),
            1.0,
            series(&[3, 2, 1, 0]),
        ));
        arc.insert(elite(
            win(WinOp::Mean, Field::Close, 20),
            1.0,
            series(&[0, 2, 0, 2]),
        ));
        arc.insert(elite(
            win(WinOp::Rank, Field::Volume, 20),
            1.0,
            series(&[2, 0, 2, 0]),
        ));
        assert_eq!(arc.len(), 4, "four distinct family niches");
        assert_eq!(arc.total_elites(), 4);
    }

    #[test]
    fn behavioural_dedup_rejects_a_correlated_offspring() {
        let mut arc = ExprArchive::new();
        let s = series(&[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            arc.insert(elite(win(WinOp::Std, Field::Close, 20), 1.0, s.clone())),
            ExprInsert::NewCell
        );
        // An identical-series offspring in the same cell is dedup-rejected.
        assert_eq!(
            arc.insert(elite(win(WinOp::Std, Field::Close, 20), 2.0, s.clone())),
            ExprInsert::DedupRejected
        );
        // An uncorrelated offspring in the same cell is accepted.
        let uncorr = series(&[3, 3, 0, 5, 1, 2]);
        assert_ne!(
            arc.insert(elite(win(WinOp::Std, Field::High, 20), 2.0, uncorr)),
            ExprInsert::DedupRejected
        );
    }

    #[test]
    fn deep_grid_is_bounded_and_keeps_the_fittest() {
        let mut arc = ExprArchive::new();
        // Same cell, distinct (uncorrelated) behaviour, increasing fitness.
        for i in 0..(SUBPOP_SIZE + 4) {
            // Vary the series so dedup never trips (shift a unique pattern per i).
            let pat: Vec<i64> = (0..8).map(|k| ((k * 7 + i * 3) % 5) as i64).collect();
            let out = arc.insert(elite(
                win(WinOp::Std, Field::Close, 20),
                i as f64,
                series(&pat),
            ));
            match i {
                0 => assert_eq!(out, ExprInsert::NewCell),
                x if x < SUBPOP_SIZE => {
                    assert!(matches!(out, ExprInsert::Added | ExprInsert::DedupRejected))
                }
                _ => assert!(matches!(
                    out,
                    ExprInsert::ImprovedElite | ExprInsert::Rejected | ExprInsert::DedupRejected
                )),
            }
        }
        assert_eq!(arc.len(), 1);
        let cell = *arc.occupied_cells().next().unwrap();
        assert!(arc.cell(&cell).unwrap().len() <= SUBPOP_SIZE);
    }

    #[test]
    fn parent_sampling_is_deterministic_and_reaches_sparse_cells() {
        let mut arc = ExprArchive::new();
        arc.insert(elite(
            win(WinOp::Std, Field::Close, 20),
            1.0,
            series(&[0, 1, 2]),
        ));
        arc.insert(elite(
            win(WinOp::Delta, Field::Close, 10),
            1.0,
            series(&[2, 1, 0]),
        ));

        let mut r1 = task_rng(42, 0);
        let mut r2 = task_rng(42, 0);
        assert_eq!(
            arc.sample_parent(&mut r1).map(|e| &e.hash),
            arc.sample_parent(&mut r2).map(|e| &e.hash)
        );

        let empty = ExprArchive::new();
        let mut r = task_rng(1, 0);
        assert!(empty.sample_parent(&mut r).is_none());
    }

    #[test]
    fn correlation_edge_cases() {
        assert_eq!(quantised_correlation(&[Some(1)], &[Some(1)]), 0.0); // < 2 points
        assert_eq!(
            quantised_correlation(&[Some(1), Some(1)], &[Some(3), Some(9)]),
            0.0
        ); // zero variance on x
        let c = quantised_correlation(&[Some(0), Some(1), Some(2)], &[Some(0), Some(1), Some(2)]);
        assert!((c - 1.0).abs() < 1e-9);
    }
}
