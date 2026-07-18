//! QE-451 Phase 1a — tree-aware variation operators (QE-450 §4.3), all on [`DetRng`].
//!
//! Three operators mirror the `variation.rs` vocabulary, "mutate freely then [`repair`]":
//! - [`local_refine`] (exploit) — constant-tweak ±one grid step, window ±one lattice step, or a
//!   same-family (price↔price) input-swap on a uniformly-chosen node.
//! - [`explore`] (cell-changing) — subtree crossover between two parents / subtree-replace / grow / prune.
//! - [`fresh_random`] (maximal exploration) — a ramped-half-and-half random tree to `D = 4`.
//!
//! **All** randomness flows through a [`DetRng`] (seed from `task_rng(master, index)` upstream), so the
//! stream is thread-count-independent (QE-006). Node selection is a **uniform index over the
//! deterministic pre-order traversal** ([`nth_node`]/[`nth_node_mut`]). Every operator ends in
//! [`ExprTree::repair`], so offspring are always valid (normalising root, on-lattice/on-grid, within caps).

use qe_signal::indicator::expr::{
    count_nodes, nth_node, nth_node_mut, BinOp, Expr, ExprTree, Field, UnOp, WinOp, CONST_GRID,
    PERIOD_LATTICE,
};
use rand_core::RngCore;

/// Price/volume terminals available to random generation (flow terminals gated off in Phase 1a).
const FIELDS: [Field; 5] = [
    Field::Close,
    Field::High,
    Field::Low,
    Field::Volume,
    Field::Typical,
];

/// Interior (non-normalising) window ops random generation may use; the root is forced to a normalising
/// op by [`ExprTree::repair`].
const INTERIOR_WINDOWS: [WinOp; 7] = [
    WinOp::Mean,
    WinOp::Max,
    WinOp::Min,
    WinOp::Std,
    WinOp::MeanAbsDev,
    WinOp::Delta,
    WinOp::Lag,
];

const UNOPS: [UnOp; 3] = [UnOp::Abs, UnOp::Sign, UnOp::Neg];
const BINOPS: [BinOp; 4] = [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div];

/// Maximum depth for random generation (`D_max`, §4.3).
const D_MAX: usize = 4;

/// Uniform integer in `0..n` from one draw (`0` if `n == 0`).
fn below(rng: &mut impl RngCore, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        (rng.next_u64() % n as u64) as usize
    }
}

fn flip(rng: &mut impl RngCore) -> bool {
    rng.next_u64() & 1 == 0
}

fn boxed(e: Expr) -> Box<Expr> {
    Box::new(e)
}

/// A random terminal: a price/volume field or an on-grid constant.
fn random_terminal(rng: &mut impl RngCore) -> Expr {
    if flip(rng) {
        Expr::Input(FIELDS[below(rng, FIELDS.len())])
    } else {
        Expr::Const(CONST_GRID[below(rng, CONST_GRID.len())])
    }
}

/// Grow a random subtree of at most `depth` (§4.3). `full = true` grows every branch to `depth`; `false`
/// (grow) lets branches terminate early. Windows draw a period from [`PERIOD_LATTICE`].
fn grow_expr(rng: &mut impl RngCore, depth: usize, full: bool) -> Expr {
    if depth <= 1 {
        return random_terminal(rng);
    }
    // In "grow" mode, sometimes stop early at a terminal.
    if !full && below(rng, 3) == 0 {
        return random_terminal(rng);
    }
    match below(rng, 3) {
        0 => Expr::Unary(
            UNOPS[below(rng, UNOPS.len())],
            boxed(grow_expr(rng, depth - 1, full)),
        ),
        1 => Expr::Binary(
            BINOPS[below(rng, BINOPS.len())],
            boxed(grow_expr(rng, depth - 1, full)),
            boxed(grow_expr(rng, depth - 1, full)),
        ),
        _ => Expr::Window(
            INTERIOR_WINDOWS[below(rng, INTERIOR_WINDOWS.len())],
            boxed(grow_expr(rng, depth - 1, full)),
            PERIOD_LATTICE[below(rng, PERIOD_LATTICE.len())],
        ),
    }
}

/// **Maximal exploration:** a ramped-half-and-half random tree (`D = 2..=D_MAX`, `full`/`grow` by a coin)
/// then [`repaired`](ExprTree::repaired) onto the validity manifold.
#[must_use]
pub fn fresh_random(rng: &mut impl RngCore) -> ExprTree {
    let depth = 2 + below(rng, D_MAX - 1); // 2..=4
    let full = flip(rng);
    ExprTree::repaired(grow_expr(rng, depth, full))
}

/// Pre-order indices of "tweakable" nodes (a `Const`, `Window`, or `Input`) — where a `local_refine`
/// nudge applies. Always non-empty (a repaired tree has at least the root window).
fn tweakable_indices(expr: &Expr) -> Vec<usize> {
    let mut out = Vec::new();
    let mut idx = 0;
    fn walk(e: &Expr, idx: &mut usize, out: &mut Vec<usize>) {
        match e {
            Expr::Input(_) | Expr::Const(_) => out.push(*idx),
            Expr::Window(_, _, _) => out.push(*idx),
            _ => {}
        }
        let here = *idx;
        *idx += 1;
        match e {
            Expr::Input(_) | Expr::Const(_) => {}
            Expr::Unary(_, c) | Expr::Window(_, c, _) => walk(c, idx, out),
            Expr::Binary(_, a, b) => {
                walk(a, idx, out);
                walk(b, idx, out);
            }
        }
        let _ = here;
    }
    walk(expr, &mut idx, &mut out);
    out
}

fn grid_index(value: &rust_decimal::Decimal) -> Option<usize> {
    CONST_GRID.iter().position(|g| g == value)
}
fn lattice_index(period: usize) -> Option<usize> {
    PERIOD_LATTICE.iter().position(|p| *p == period)
}

fn step_index(idx: usize, len: usize, up: bool) -> usize {
    if up {
        (idx + 1).min(len - 1)
    } else {
        idx.saturating_sub(1)
    }
}

/// **Exploitation:** nudge a uniformly-chosen tweakable node — a constant ±one grid step, a window ±one
/// lattice step, or a same-family (price↔price) input-swap — then repair.
#[must_use]
pub fn local_refine(parent: &ExprTree, rng: &mut impl RngCore) -> ExprTree {
    let mut root = parent.root().clone();
    let choices = tweakable_indices(&root);
    let target = choices[below(rng, choices.len())];
    if let Some(node) = nth_node_mut(&mut root, target) {
        match node {
            Expr::Const(c) => {
                let up = flip(rng);
                let i = grid_index(c).unwrap_or(0);
                *c = CONST_GRID[step_index(i, CONST_GRID.len(), up)];
            }
            Expr::Window(_, _, period) => {
                let up = flip(rng);
                let i = lattice_index(*period).unwrap_or(0);
                *period = PERIOD_LATTICE[step_index(i, PERIOD_LATTICE.len(), up)];
            }
            Expr::Input(f) => {
                // Same-family swap: choose a different price field.
                let cur = FIELDS.iter().position(|x| x == f).unwrap_or(0);
                let mut j = below(rng, FIELDS.len());
                if j == cur {
                    j = (cur + 1) % FIELDS.len();
                }
                *f = FIELDS[j];
            }
            _ => {}
        }
    }
    ExprTree::repaired(root)
}

/// Pre-order indices of terminal (leaf) nodes.
fn terminal_indices(expr: &Expr) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..count_nodes(expr) {
        if matches!(nth_node(expr, i), Some(Expr::Input(_) | Expr::Const(_))) {
            out.push(i);
        }
    }
    out
}

/// Pre-order indices of internal (non-leaf) nodes.
fn internal_indices(expr: &Expr) -> Vec<usize> {
    (0..count_nodes(expr))
        .filter(|i| !matches!(nth_node(expr, *i), Some(Expr::Input(_) | Expr::Const(_))))
        .collect()
}

/// **Exploration (cell-changing):** one of subtree-crossover (needs `other`), subtree-replace, grow, or
/// prune on a uniformly-chosen node, then repair.
#[must_use]
pub fn explore(parent: &ExprTree, other: Option<&ExprTree>, rng: &mut impl RngCore) -> ExprTree {
    let mut root = parent.root().clone();
    let n = count_nodes(&root);
    // Variant selection: 0 crossover (if other), 1 subtree-replace, 2 grow, 3 prune.
    let variant =
        below(rng, if other.is_some() { 4 } else { 3 }) + if other.is_some() { 0 } else { 1 };

    match variant {
        0 => {
            // Crossover: replace a uniform node with a copied subtree from `other`.
            let other = other.expect("variant 0 requires another parent");
            let target = below(rng, n);
            let donor_idx = below(rng, count_nodes(other.root()));
            if let (Some(slot), Some(donor)) = (
                nth_node_mut(&mut root, target),
                nth_node(other.root(), donor_idx),
            ) {
                *slot = donor.clone();
            }
        }
        1 => {
            // Subtree-replace with a fresh small random subtree.
            let target = below(rng, n);
            let depth = 2 + below(rng, 2);
            let full = flip(rng);
            if let Some(slot) = nth_node_mut(&mut root, target) {
                *slot = grow_expr(rng, depth, full);
            }
        }
        2 => {
            // Grow: replace a terminal with a small internal node.
            let terms = terminal_indices(&root);
            let target = terms[below(rng, terms.len())];
            if let Some(slot) = nth_node_mut(&mut root, target) {
                *slot = grow_expr(rng, 2, true);
            }
        }
        _ => {
            // Prune: replace an internal node with a terminal (fallback to subtree-replace if none).
            let internals = internal_indices(&root);
            if internals.is_empty() {
                let target = below(rng, n);
                if let Some(slot) = nth_node_mut(&mut root, target) {
                    *slot = random_terminal(rng);
                }
            } else {
                let target = internals[below(rng, internals.len())];
                if let Some(slot) = nth_node_mut(&mut root, target) {
                    *slot = random_terminal(rng);
                }
            }
        }
    }
    ExprTree::repaired(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::task_rng;
    use qe_signal::indicator::expr::{MAX_DEPTH, MAX_NODES, MAX_TOTAL_LOOKBACK};

    fn assert_valid(t: &ExprTree) {
        assert!(t.depth() <= MAX_DEPTH);
        assert!(t.node_count() <= MAX_NODES);
        assert!(t.lookback() <= MAX_TOTAL_LOOKBACK);
        assert!(t.root_op().map(WinOp::is_normalising).unwrap_or(false));
    }

    #[test]
    fn operators_always_repair_to_validity() {
        let mut rng = task_rng(1, 0);
        let mut parent = fresh_random(&mut rng);
        let mut other = fresh_random(&mut rng);
        for _ in 0..500 {
            let f = fresh_random(&mut rng);
            assert_valid(&f);
            let r = local_refine(&parent, &mut rng);
            assert_valid(&r);
            let e = explore(&parent, Some(&other), &mut rng);
            assert_valid(&e);
            let e2 = explore(&parent, None, &mut rng);
            assert_valid(&e2);
            parent = r;
            other = e;
        }
    }

    #[test]
    fn operators_are_detrng_deterministic() {
        let run = || {
            let mut rng = task_rng(123, 7);
            let p = fresh_random(&mut rng);
            let o = fresh_random(&mut rng);
            let a = local_refine(&p, &mut rng);
            let b = explore(&p, Some(&o), &mut rng);
            (
                p.canonical_hash(),
                o.canonical_hash(),
                a.canonical_hash(),
                b.canonical_hash(),
            )
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn local_refine_nudges_a_gene() {
        // A window-period tweak stays on the lattice; the tree remains valid.
        let mut rng = task_rng(5, 0);
        let parent = ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                20,
            )),
            50,
        ));
        for _ in 0..50 {
            let child = local_refine(&parent, &mut rng);
            assert_valid(&child);
        }
    }
}
