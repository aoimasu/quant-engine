//! QE-451 Phase 1a — pure-structural descriptors for the `Elite<ExprTree>` archive (QE-450 §4.5).
//!
//! Three **window-invariant, genotype-derived** axes (so `cell_reassignment_rate = 0`, respecting the
//! QE-111 `STABILITY_THRESHOLD`):
//!
//! 1. [`family_of_tree`] — a **structural classifier** on the tree's dominant `Field`/op, mapped onto the
//!    existing five [`IndicatorFamily`] variants. It replaces `family_of`'s id-prefix match (which breaks
//!    on auto-named formulas). Flow is **never** produced in Phase 1a (flow terminals are gated off).
//! 2. [`TimescaleBand`] — reused verbatim from the strategy archive, banded off the tree's structural
//!    lookback.
//! 3. [`ComplexityBand`] — a node-count band `{≤2 / 3–4 / ≥5}`, the parsimony-illuminating axis.
//!
//! Their product is the `5 × 3 × 3 = 45`-cell grid ([`grid_cells`], [`EXPR_CELLS`]).

use qe_signal::indicator::expr::{Expr, ExprTree, Field, WinOp};

use crate::archive::{IndicatorFamily, TimescaleBand, FAMILIES, TIMESCALES};

/// Node-count complexity band (§4.5) — the parsimony-illuminating axis, so a 2-node formula is a
/// first-class elite a complex one can never out-compete in its own band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComplexityBand {
    /// `≤ 2` nodes (a bare normalised terminal).
    Trivial,
    /// `3–4` nodes.
    Simple,
    /// `≥ 5` nodes.
    Complex,
}

/// All complexity bands in fixed order.
pub const COMPLEXITIES: [ComplexityBand; 3] = [
    ComplexityBand::Trivial,
    ComplexityBand::Simple,
    ComplexityBand::Complex,
];

impl ComplexityBand {
    /// Band for a node count (§4.5 cutoffs).
    #[must_use]
    pub fn from_node_count(nodes: usize) -> Self {
        if nodes <= 2 {
            ComplexityBand::Trivial
        } else if nodes <= 4 {
            ComplexityBand::Simple
        } else {
            ComplexityBand::Complex
        }
    }

    /// This band's index in [`COMPLEXITIES`].
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            ComplexityBand::Trivial => 0,
            ComplexityBand::Simple => 1,
            ComplexityBand::Complex => 2,
        }
    }
}

/// Structural signal counts gathered in one traversal (excluding the normalising root's own op, which
/// carries no family information).
#[derive(Default)]
struct Stats {
    volume_terminals: usize,
    other_terminals: usize,
    has_dispersion: bool, // Std / MeanAbsDev present
    has_temporal: bool,   // Delta / Lag present
    has_smoothing: bool,  // Mean / Max / Min present
}

fn gather(expr: &Expr, stats: &mut Stats) {
    match expr {
        Expr::Input(Field::Volume) => stats.volume_terminals += 1,
        Expr::Input(_) => stats.other_terminals += 1,
        Expr::Const(_) => {}
        Expr::Unary(_, c) => gather(c, stats),
        Expr::Binary(_, a, b) => {
            gather(a, stats);
            gather(b, stats);
        }
        Expr::Window(op, c, _) => {
            match op {
                WinOp::Std | WinOp::MeanAbsDev => stats.has_dispersion = true,
                WinOp::Delta | WinOp::Lag => stats.has_temporal = true,
                WinOp::Mean | WinOp::Max | WinOp::Min => stats.has_smoothing = true,
                // Rank / Zscore are normalising (not a family signal).
                WinOp::Rank | WinOp::Zscore => {}
            }
            gather(c, stats);
        }
    }
}

/// Classify a tree into its dominant [`IndicatorFamily`] (§4.5). Deterministic and window-invariant.
///
/// Priority (first match wins): volume-dominant ⇒ `Volume`; a dispersion op (`Std`/`MeanAbsDev`) ⇒
/// `Volatility`; a temporal op (`Delta`/`Lag`) ⇒ `Momentum`; a smoothing op (`Mean`/`Max`/`Min`) ⇒
/// `Trend`; otherwise (a bare normalised terminal — a percentile/z oscillator) ⇒ `Momentum`. Flow is
/// **never** produced (flow terminals are gated off in Phase 1a).
#[must_use]
pub fn family_of_tree(expr: &Expr) -> IndicatorFamily {
    let mut stats = Stats::default();
    gather(expr, &mut stats);
    if stats.volume_terminals > stats.other_terminals {
        IndicatorFamily::Volume
    } else if stats.has_dispersion {
        IndicatorFamily::Volatility
    } else if stats.has_temporal {
        IndicatorFamily::Momentum
    } else if stats.has_smoothing {
        IndicatorFamily::Trend
    } else {
        IndicatorFamily::Momentum
    }
}

/// A structural niche coordinate in the `Elite<ExprTree>` archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprCell {
    /// Dominant structural family.
    pub family: IndicatorFamily,
    /// Reaction-speed band (from structural lookback).
    pub timescale: TimescaleBand,
    /// Node-count complexity band.
    pub complexity: ComplexityBand,
}

/// The descriptor [`ExprCell`] a repaired tree occupies — all inputs are structural (genotype), so the
/// niche is stable across evaluation windows.
#[must_use]
pub fn descriptor_for_tree(tree: &ExprTree) -> ExprCell {
    ExprCell {
        family: family_of_tree(tree.root()),
        timescale: TimescaleBand::from_lookback(tree.lookback()),
        complexity: ComplexityBand::from_node_count(tree.node_count()),
    }
}

/// Number of grid cells = `|families| × |timescales| × |complexities|`.
pub const EXPR_CELLS: usize = FAMILIES.len() * TIMESCALES.len() * COMPLEXITIES.len();

/// Enumerate every grid cell in a fixed deterministic order.
pub fn grid_cells() -> impl Iterator<Item = ExprCell> {
    FAMILIES.into_iter().flat_map(|family| {
        TIMESCALES.into_iter().flat_map(move |timescale| {
            COMPLEXITIES.into_iter().map(move |complexity| ExprCell {
                family,
                timescale,
                complexity,
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_signal::indicator::expr::UnOp;

    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }
    fn win(op: WinOp, f: Field, n: usize) -> Expr {
        Expr::Window(op, boxed(Expr::Input(f)), n)
    }

    #[test]
    fn grid_has_exactly_45_unique_cells() {
        let cells: Vec<ExprCell> = grid_cells().collect();
        assert_eq!(cells.len(), 45);
        assert_eq!(EXPR_CELLS, 45);
        let mut u = cells.clone();
        u.sort();
        u.dedup();
        assert_eq!(u.len(), cells.len());
    }

    #[test]
    fn family_classifier_is_structural_and_never_flow() {
        // Volume-dominant → Volume.
        assert_eq!(
            family_of_tree(&win(WinOp::Rank, Field::Volume, 20)),
            IndicatorFamily::Volume
        );
        // Std present → Volatility.
        assert_eq!(
            family_of_tree(&Expr::Window(
                WinOp::Rank,
                boxed(win(WinOp::Std, Field::Close, 20)),
                50
            )),
            IndicatorFamily::Volatility
        );
        // Delta present → Momentum.
        assert_eq!(
            family_of_tree(&Expr::Window(
                WinOp::Rank,
                boxed(win(WinOp::Delta, Field::Close, 10)),
                50
            )),
            IndicatorFamily::Momentum
        );
        // Mean smoothing (no dispersion/temporal) → Trend.
        assert_eq!(
            family_of_tree(&Expr::Window(
                WinOp::Rank,
                boxed(win(WinOp::Mean, Field::Close, 20)),
                50
            )),
            IndicatorFamily::Trend
        );
        // Bare normalised terminal → Momentum (oscillator).
        assert_eq!(
            family_of_tree(&win(WinOp::Zscore, Field::Close, 10)),
            IndicatorFamily::Momentum
        );
        // A pointwise wrapper does not change the (absent) family signal → Momentum default.
        assert_eq!(
            family_of_tree(&Expr::Window(
                WinOp::Rank,
                boxed(Expr::Unary(UnOp::Abs, boxed(Expr::Input(Field::Close)))),
                50
            )),
            IndicatorFamily::Momentum
        );
    }

    #[test]
    fn complexity_bands() {
        assert_eq!(ComplexityBand::from_node_count(1), ComplexityBand::Trivial);
        assert_eq!(ComplexityBand::from_node_count(2), ComplexityBand::Trivial);
        assert_eq!(ComplexityBand::from_node_count(3), ComplexityBand::Simple);
        assert_eq!(ComplexityBand::from_node_count(4), ComplexityBand::Simple);
        assert_eq!(ComplexityBand::from_node_count(5), ComplexityBand::Complex);
    }

    #[test]
    fn descriptor_is_structural() {
        let t = ExprTree::repaired(win(WinOp::Std, Field::Close, 20));
        let cell = descriptor_for_tree(&t);
        assert_eq!(cell.family, IndicatorFamily::Volatility);
        // Structural lookback bands the timescale; complexity from node count.
        assert_eq!(
            cell.complexity,
            ComplexityBand::from_node_count(t.node_count())
        );
    }
}
