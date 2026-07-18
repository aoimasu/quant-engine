//! QE-451 Phase 0 — the `Expr`/`Kernel` tree-interpreter seam proof (default-off).
//!
//! The load-bearing decision of the QE-450 GP design is that variable indicator *structure* lives
//! behind the existing [`Indicator`]/[`Kernel`] trait, never in the strategy genome. This module
//! proves the seam is real and free: an expression tree [`Expr`] compiles to a [`Kernel`], so it
//! rides the existing `impl<K: Kernel> Indicator` blanket impl and the one `update()` path that *is*
//! [`compute_batch`](super::compute_batch) — **batch == streaming parity for free**. A pure
//! [`max_lookback`] recursion yields the exact FIR span for purge/embargo.
//!
//! **Phase 0 only:** no search, no evolution, no archive, no deflation. The interpreter is
//! `rust_decimal` only (no `f64` on any evaluation path). FIR-only grammar (no EWMA/IIR, no
//! expanding/cumulative/forward ops). Price terminals only — flow terminals (funding/OI/premium) are
//! deferred to Phase 1 (their lookback is in present-scalars, not bars, until dense forward-fill).
//!
//! **Default-off:** nothing in the default pipeline references this module. The seeded
//! catalogue-equivalent indicators are produced only by [`seed_catalogue_subset`], which
//! [`catalogue`](super::catalogue) never calls — so the production catalogue is unchanged, no golden
//! moves, and [`CATALOGUE_VERSION`](super::CATALOGUE_VERSION) does not bump. See
//! `docs/architecture/qe-451-phase0-expr-seam-design.md`.

use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use super::roll::Roll;
use super::{Indicator, Kernel, Quantiser, Sample};

/// Denominators with magnitude below this snap `protected_div` to zero (QE-450 §4.2 fixed zero
/// convention). Far below any real price/volume scale, so the reproduced catalogue subset never
/// trips it.
const DIV_EPSILON: Decimal = Decimal::from_parts(1, 0, 0, false, 9); // 1e-9

/// Symmetric clip bound for the `Zscore` normalising root (QE-450 §4.2: clipped `[−4, 4]`).
const ZSCORE_CLIP: Decimal = Decimal::from_parts(4, 0, 0, false, 0);

// ---- QE-451 Phase 1a: fixed lattices, grid, and structural caps (§4.2 / §4.3) ----------------

/// The fixed window-period lattice every window op snaps to in [`ExprTree::repair`] (§4.2). Snapping
/// to this set also enforces "window ≥ 5" and makes `Delta(x, 1)` unreachable in the search grammar.
pub const PERIOD_LATTICE: [Period; 5] = [5, 10, 20, 50, 100];

/// The default period the normalising root wrapper uses when [`ExprTree::repair`] forces a root
/// (`Rank` is the default root, §4.2). Must be a member of [`PERIOD_LATTICE`].
pub const DEFAULT_RANK_PERIOD: Period = 50;

/// Hard structural caps (§4.3 / QE-450 spectrum position). Enforced by [`ExprTree::repair`].
pub const MAX_DEPTH: usize = 4;
/// Maximum node count (inclusive).
pub const MAX_NODES: usize = 16;
/// Maximum total FIR lookback in bars (inclusive).
pub const MAX_TOTAL_LOOKBACK: usize = 200;

/// The fixed rational constant grid (§4.2). A finite grid keeps the reachable canonical set countable
/// (`E[maxSR]` well-posed for Phase 1b's deflation). Ascending order (nearest-snap, ties → lower).
pub const CONST_GRID: [Decimal; 15] = [
    Decimal::from_parts(100, 0, 0, true, 0),  // -100
    Decimal::from_parts(10, 0, 0, true, 0),   // -10
    Decimal::from_parts(5, 0, 0, true, 0),    // -5
    Decimal::from_parts(2, 0, 0, true, 0),    // -2
    Decimal::from_parts(1, 0, 0, true, 0),    // -1
    Decimal::from_parts(5, 0, 0, true, 1),    // -0.5
    Decimal::from_parts(25, 0, 0, true, 2),   // -0.25
    Decimal::from_parts(0, 0, 0, false, 0),   // 0
    Decimal::from_parts(25, 0, 0, false, 2),  // 0.25
    Decimal::from_parts(5, 0, 0, false, 1),   // 0.5
    Decimal::from_parts(1, 0, 0, false, 0),   // 1
    Decimal::from_parts(2, 0, 0, false, 0),   // 2
    Decimal::from_parts(5, 0, 0, false, 0),   // 5
    Decimal::from_parts(10, 0, 0, false, 0),  // 10
    Decimal::from_parts(100, 0, 0, false, 0), // 100
];

/// Snap a period to the nearest member of [`PERIOD_LATTICE`] (ties → the lower period). Deterministic.
#[must_use]
pub fn snap_period(period: Period) -> Period {
    let mut best = PERIOD_LATTICE[0];
    let mut best_dist = period.abs_diff(best);
    for &p in &PERIOD_LATTICE[1..] {
        let d = period.abs_diff(p);
        if d < best_dist {
            best = p;
            best_dist = d;
        }
    }
    best
}

/// Snap a constant to the nearest member of [`CONST_GRID`] (ties → the lower, since the grid is scanned
/// ascending and a strict `<` keeps the first). Deterministic, exact-`Decimal`.
#[must_use]
pub fn snap_const(value: Decimal) -> Decimal {
    let mut best = CONST_GRID[0];
    let mut best_dist = (value - best).abs();
    for &g in &CONST_GRID[1..] {
        let d = (value - g).abs();
        if d < best_dist {
            best = g;
            best_dist = d;
        }
    }
    best
}

/// A price/volume terminal read from the current bar. Leaf lookback = 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Bar close.
    Close,
    /// Bar high.
    High,
    /// Bar low.
    Low,
    /// Bar volume.
    Volume,
    /// Typical price `(high + low + close) / 3`.
    Typical,
}

/// A point-wise unary operator (lookback = child).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Absolute value.
    Abs,
    /// Sign: `-1`, `0`, or `+1`.
    Sign,
    /// Negation.
    Neg,
}

/// A point-wise binary operator (lookback = max of children).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Protected division: `|denominator| < ε ⇒ 0`.
    Div,
}

/// A strictly-causal FIR window operator over the child's trailing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinOp {
    /// Arithmetic mean of the window.
    Mean,
    /// Maximum of the window.
    Max,
    /// Minimum of the window.
    Min,
    /// Population standard deviation of the window.
    Std,
    /// Mean absolute deviation from the window mean.
    MeanAbsDev,
    /// Difference between the newest and oldest window value (`current − k-ago`, capacity `k + 1`).
    Delta,
    /// The oldest window value (`k-ago`, capacity `k + 1`).
    Lag,
    /// **Normalising root (QE-451 Phase 1a, §4.2).** Trailing rank of the current value in its window,
    /// `count(v < current) / n ∈ [0, 1)`. Monotone-invariant, strictly-causal FIR.
    Rank,
    /// **Normalising root (QE-451 Phase 1a, §4.2).** Trailing population z-score of the current value,
    /// `(current − mean) / std_pop`, clipped to `[−4, 4]` (`std == 0 ⇒ 0`). Strictly-causal FIR.
    Zscore,
}

impl WinOp {
    /// Whether this op is one of the strongly-typed **normalising roots** (§4.2) — the only ops allowed
    /// at a tree's root so its output is bounded and feeds the point-wise [`Quantiser`] unchanged. `Rank`
    /// is the default root.
    #[must_use]
    pub fn is_normalising(self) -> bool {
        matches!(self, WinOp::Rank | WinOp::Zscore)
    }
}

/// A window's [`Roll`] capacity, in bars. For `Mean/Max/Min/Std/MeanAbsDev` this is the window length
/// `n`; for the temporal ops `Lag(x, k)` / `Delta(x, k)` it is `k + 1` (current bar + `k` of history),
/// so one uniform rule (`(capacity − 1) + child`) covers every window op.
pub type Period = usize;

/// A FIR indicator expression tree (QE-450 §4.1). Interpreted in `rust_decimal` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A price/volume terminal read from the current bar.
    Input(Field),
    /// A constant terminal (lookback = 0).
    Const(Decimal),
    /// A point-wise unary op applied to its child.
    Unary(UnOp, Box<Expr>),
    /// A point-wise binary op combining two children.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// A trailing FIR window (capacity `Period`) over its child's values.
    Window(WinOp, Box<Expr>, Period),
}

/// The **exact** FIR span of `expr` — the number of most-recent observed bars the latest value depends
/// on (QE-450 §4.1). Fed verbatim to [`IndicatorSpec::lookback`](super::IndicatorSpec) so purge/embargo
/// stays correct.
///
/// `leaf → 1`, `const → 0`, `unary → child`, `binary → max(children)`,
/// `window(op, child, cap) → (cap − 1) + child`.
#[must_use]
pub fn max_lookback(expr: &Expr) -> usize {
    match expr {
        Expr::Input(_) => 1,
        Expr::Const(_) => 0,
        Expr::Unary(_, child) => max_lookback(child),
        Expr::Binary(_, a, b) => max_lookback(a).max(max_lookback(b)),
        Expr::Window(_, child, cap) => cap.saturating_sub(1) + max_lookback(child),
    }
}

// ---- compiled (stateful) tree ----------------------------------------------------------------

/// A compiled node: the arithmetic mirror of [`Expr`], but every [`Node::Window`] owns a live
/// [`Roll`] so one post-order pass per bar folds the whole tree.
enum Node {
    Input(Field),
    Const(Decimal),
    Unary(UnOp, Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    Window {
        op: WinOp,
        child: Box<Node>,
        roll: Roll,
    },
}

fn compile_node(expr: &Expr) -> Node {
    match expr {
        Expr::Input(f) => Node::Input(*f),
        Expr::Const(c) => Node::Const(*c),
        Expr::Unary(op, child) => Node::Unary(*op, Box::new(compile_node(child))),
        Expr::Binary(op, a, b) => {
            Node::Binary(*op, Box::new(compile_node(a)), Box::new(compile_node(b)))
        }
        Expr::Window(op, child, cap) => Node::Window {
            op: *op,
            child: Box::new(compile_node(child)),
            roll: Roll::new((*cap).max(1)),
        },
    }
}

/// The current-bar value of `field`, always defined (the bar always carries these).
fn field_value(field: Field, sample: &Sample) -> Decimal {
    let b = &sample.bar;
    match field {
        Field::Close => b.close().get(),
        Field::High => b.high().get(),
        Field::Low => b.low().get(),
        Field::Volume => b.volume().get(),
        Field::Typical => (b.high().get() + b.low().get() + b.close().get()) / Decimal::from(3),
    }
}

fn apply_unary(op: UnOp, v: Decimal) -> Decimal {
    match op {
        UnOp::Abs => v.abs(),
        UnOp::Neg => -v,
        UnOp::Sign => {
            if v.is_zero() {
                Decimal::ZERO
            } else if v.is_sign_positive() {
                Decimal::ONE
            } else {
                -Decimal::ONE
            }
        }
    }
}

fn apply_binary(op: BinOp, x: Decimal, y: Decimal) -> Decimal {
    match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y.abs() < DIV_EPSILON {
                Decimal::ZERO
            } else {
                x / y
            }
        }
    }
}

/// Aggregate a full window. `Roll` is oldest→newest, so `Delta = newest − oldest` and `Lag = oldest`.
fn aggregate(op: WinOp, roll: &Roll) -> Option<Decimal> {
    match op {
        WinOp::Mean => roll.mean(),
        WinOp::Max => roll.max(),
        WinOp::Min => roll.min(),
        WinOp::Std => roll.std_pop(),
        WinOp::MeanAbsDev => roll.mean_abs_dev(),
        WinOp::Delta => Some(roll.last()? - roll.first()?),
        WinOp::Lag => roll.first(),
        WinOp::Rank => {
            // Trailing rank in [0, 1): fraction of window values strictly below the current (newest)
            // value. `aggregate` is only called on a full window, so capacity == buffered length.
            let current = roll.last()?;
            let below = roll.iter().filter(|&v| v < current).count();
            Some(Decimal::from(below) / Decimal::from(roll.cap()))
        }
        WinOp::Zscore => {
            // Trailing population z-score of the current value, clipped to [-4, 4]; std == 0 ⇒ 0.
            let current = roll.last()?;
            let mean = roll.mean()?;
            let std = roll.std_pop()?;
            if std.is_zero() {
                return Some(Decimal::ZERO);
            }
            let z = (current - mean) / std;
            Some(z.clamp(-ZSCORE_CLIP, ZSCORE_CLIP))
        }
    }
}

/// One post-order fold of the tree for this bar. Returns the node's current value, or `None` while a
/// window feeding it is not yet full. **No short-circuit** — both `Binary` children are always
/// evaluated so every nested window advances its roll every bar (keeping warmup aligned with
/// [`max_lookback`]).
fn eval(node: &mut Node, sample: &Sample) -> Option<Decimal> {
    match node {
        Node::Input(f) => Some(field_value(*f, sample)),
        Node::Const(c) => Some(*c),
        Node::Unary(op, child) => eval(child, sample).map(|v| apply_unary(*op, v)),
        Node::Binary(op, a, b) => {
            let va = eval(a, sample);
            let vb = eval(b, sample);
            match (va, vb) {
                (Some(x), Some(y)) => Some(apply_binary(*op, x, y)),
                _ => None,
            }
        }
        Node::Window { op, child, roll } => {
            if let Some(v) = eval(child, sample) {
                roll.push(v);
            }
            if roll.is_full() {
                aggregate(*op, roll)
            } else {
                None
            }
        }
    }
}

fn clear_node(node: &mut Node) {
    match node {
        Node::Input(_) | Node::Const(_) => {}
        Node::Unary(_, child) => clear_node(child),
        Node::Binary(_, a, b) => {
            clear_node(a);
            clear_node(b);
        }
        Node::Window { child, roll, .. } => {
            *roll = Roll::new(roll.cap());
            clear_node(child);
        }
    }
}

/// An [`Expr`]-backed indicator: the compiled tree plus the catalogue's own point-wise quantiser. It
/// gets [`Indicator`] + batch==streaming parity for free via the [`Kernel`] blanket impl.
struct ExprIndicator {
    id: String,
    q: Quantiser,
    root: Node,
    lookback: usize,
    cached: Option<Decimal>,
}

impl Kernel for ExprIndicator {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn lookback(&self) -> usize {
        self.lookback
    }
    fn quantiser(&self) -> &Quantiser {
        &self.q
    }
    fn observe(&mut self, sample: &Sample) {
        self.cached = eval(&mut self.root, sample);
    }
    fn warm(&self) -> bool {
        self.cached.is_some()
    }
    fn raw(&self) -> Option<Decimal> {
        self.cached
    }
    fn clear(&mut self) {
        clear_node(&mut self.root);
        self.cached = None;
    }
}

/// Compile `expr` into an [`Indicator`] identified by `id` and quantised by `quantiser`. Its declared
/// lookback is the exact [`max_lookback`] span, so it slots into the schema/purge machinery unchanged.
#[must_use]
pub fn compile(id: &str, expr: &Expr, quantiser: Quantiser) -> Box<dyn Indicator> {
    Box::new(ExprIndicator {
        id: id.to_owned(),
        q: quantiser,
        root: compile_node(expr),
        lookback: max_lookback(expr),
        cached: None,
    })
}

/// Run the raw (pre-quantisation) interpreter over `samples`, returning one `Option<Decimal>` per bar
/// (`None` until warm). This is the streaming fold; the QE-451 slow-reference oracle checks it against
/// an independent naive recompute.
#[must_use]
pub fn eval_stream(expr: &Expr, samples: &[Sample]) -> Vec<Option<Decimal>> {
    let mut root = compile_node(expr);
    samples.iter().map(|s| eval(&mut root, s)).collect()
}

// ---- QE-451 Phase 1a: ExprTree (structure + repair + canonicalisation) -----------------------

/// Total node count of `expr` (every terminal and internal node counts 1).
#[must_use]
pub fn count_nodes(expr: &Expr) -> usize {
    match expr {
        Expr::Input(_) | Expr::Const(_) => 1,
        Expr::Unary(_, c) => 1 + count_nodes(c),
        Expr::Binary(_, a, b) => 1 + count_nodes(a) + count_nodes(b),
        Expr::Window(_, c, _) => 1 + count_nodes(c),
    }
}

/// Depth of `expr` — a terminal has depth 1; a node's depth is `1 + max(child depths)`.
#[must_use]
pub fn tree_depth(expr: &Expr) -> usize {
    match expr {
        Expr::Input(_) | Expr::Const(_) => 1,
        Expr::Unary(_, c) => 1 + tree_depth(c),
        Expr::Binary(_, a, b) => 1 + tree_depth(a).max(tree_depth(b)),
        Expr::Window(_, c, _) => 1 + tree_depth(c),
    }
}

/// Immutable pre-order (node, then children left→right) reference to the `index`-th node, or `None` if
/// `index` is out of range. The traversal order is deterministic — the substrate for uniform node
/// selection in the tree operators (§4.3).
#[must_use]
pub fn nth_node(expr: &Expr, index: usize) -> Option<&Expr> {
    fn walk<'a>(e: &'a Expr, target: usize, cur: &mut usize) -> Option<&'a Expr> {
        if *cur == target {
            return Some(e);
        }
        *cur += 1;
        match e {
            Expr::Input(_) | Expr::Const(_) => None,
            Expr::Unary(_, c) | Expr::Window(_, c, _) => walk(c, target, cur),
            Expr::Binary(_, a, b) => walk(a, target, cur).or_else(|| walk(b, target, cur)),
        }
    }
    let mut cur = 0;
    walk(expr, index, &mut cur)
}

/// Mutable pre-order reference to the `index`-th node (see [`nth_node`]). Lets a tree operator replace a
/// uniformly-chosen subtree in place.
#[must_use]
pub fn nth_node_mut(expr: &mut Expr, index: usize) -> Option<&mut Expr> {
    fn walk<'a>(e: &'a mut Expr, target: usize, cur: &mut usize) -> Option<&'a mut Expr> {
        if *cur == target {
            return Some(e);
        }
        *cur += 1;
        match e {
            Expr::Input(_) | Expr::Const(_) => None,
            Expr::Unary(_, c) | Expr::Window(_, c, _) => walk(c, target, cur),
            Expr::Binary(_, a, b) => walk(a, target, cur).or_else(|| walk(b, target, cur)),
        }
    }
    let mut cur = 0;
    walk(expr, index, &mut cur)
}

/// A structure-owning FIR expression tree with a cached exact lookback — the genotype the offline GP
/// pool evolves (QE-451 Phase 1a). Construct via [`ExprTree::repaired`] (or [`ExprTree::new`] +
/// [`ExprTree::repair`]) so the invariants (normalising root, on-lattice periods, on-grid constants,
/// caps, exact cached lookback) always hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprTree {
    root: Expr,
    lookback: usize,
}

impl ExprTree {
    /// Wrap `root` and cache its lookback **without** repairing. Prefer [`ExprTree::repaired`] unless you
    /// deliberately want the raw tree (e.g. to test that `repair` fixes it).
    #[must_use]
    pub fn new(root: Expr) -> Self {
        let lookback = max_lookback(&root);
        ExprTree { root, lookback }
    }

    /// Wrap `root` and immediately [`repair`](ExprTree::repair) it onto the validity manifold.
    #[must_use]
    pub fn repaired(root: Expr) -> Self {
        let mut t = ExprTree::new(root);
        t.repair();
        t
    }

    /// The tree's root expression.
    #[must_use]
    pub fn root(&self) -> &Expr {
        &self.root
    }

    /// Consume the tree, returning its root expression.
    #[must_use]
    pub fn into_root(self) -> Expr {
        self.root
    }

    /// The cached exact FIR lookback (bars). Recomputed by [`repair`](ExprTree::repair).
    #[must_use]
    pub fn lookback(&self) -> usize {
        self.lookback
    }

    /// Total node count.
    #[must_use]
    pub fn node_count(&self) -> usize {
        count_nodes(&self.root)
    }

    /// Tree depth.
    #[must_use]
    pub fn depth(&self) -> usize {
        tree_depth(&self.root)
    }

    /// The window op at the root, if the root is a window node. After [`repair`](ExprTree::repair) this
    /// is always a normalising root (`Rank`/`Zscore`).
    #[must_use]
    pub fn root_op(&self) -> Option<WinOp> {
        match &self.root {
            Expr::Window(op, _, _) => Some(*op),
            _ => None,
        }
    }

    /// **`ExprTree::repair` (§4.3)** — deterministic + idempotent. In order: force a normalising root;
    /// snap every period to [`PERIOD_LATTICE`] and every constant to [`CONST_GRID`]; cap total lookback
    /// ≤ [`MAX_TOTAL_LOOKBACK`]; cap depth ≤ [`MAX_DEPTH`] and nodes ≤ [`MAX_NODES`] (pruning the
    /// deterministically-deepest descendant subtree, never the root wrapper); recompute + cache lookback.
    pub fn repair(&mut self) {
        // (1) Force a normalising root: wrap iff not already a normalising window node.
        let already_normalising =
            matches!(&self.root, Expr::Window(op, _, _) if op.is_normalising());
        if !already_normalising {
            let inner = std::mem::replace(&mut self.root, Expr::Const(Decimal::ZERO));
            self.root = Expr::Window(WinOp::Rank, Box::new(inner), DEFAULT_RANK_PERIOD);
        }
        // (2) Snap periods to the lattice and constants to the grid throughout.
        snap_in_place(&mut self.root);
        // (3) Cap total lookback; (4) cap depth / nodes — both by pruning the deepest descendant.
        while max_lookback(&self.root) > MAX_TOTAL_LOOKBACK
            || tree_depth(&self.root) > MAX_DEPTH
            || count_nodes(&self.root) > MAX_NODES
        {
            if !prune_deepest_descendant(&mut self.root) {
                break; // root is irreducible (a bare normalising window over a terminal)
            }
        }
        // (5) Recompute + cache lookback.
        self.lookback = max_lookback(&self.root);
    }

    /// The canonical form (constant-folded, commutative operands ordered, rank-monotone outer wrappers
    /// collapsed). Two trees that are equivalent under these rules share a canonical form (§8).
    #[must_use]
    pub fn canonical(&self) -> Expr {
        canonicalize(&self.root)
    }

    /// The exact **canonical S-expression text** the content hash is taken over (constants rendered via
    /// exact `Decimal` `Display`, so **`rust_decimal`-only**, no `f64`). Human-readable and exact — the
    /// frozen-pool artefact (QE-451 Phase 1b) carries this string alongside its [`canonical_hash`].
    #[must_use]
    pub fn canonical_sexpr(&self) -> String {
        let mut s = String::new();
        write_sexpr(&self.canonical(), &mut s);
        s
    }

    /// A content hash of the [`canonical`](ExprTree::canonical) form — a SHA-256 over the canonical
    /// S-expression text (constants rendered via exact `Decimal` `Display`, so **`rust_decimal`-only**,
    /// no `f64` ever feeds the hash). The distinct count of these over all evaluated trees is the
    /// distinct-canonical trial basis (§8).
    #[must_use]
    pub fn canonical_hash(&self) -> String {
        let digest = Sha256::digest(self.canonical_sexpr().as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Snap all periods to [`PERIOD_LATTICE`] and constants to [`CONST_GRID`], in place.
fn snap_in_place(expr: &mut Expr) {
    match expr {
        Expr::Input(_) => {}
        Expr::Const(c) => *c = snap_const(*c),
        Expr::Unary(_, child) => snap_in_place(child),
        Expr::Binary(_, a, b) => {
            snap_in_place(a);
            snap_in_place(b);
        }
        Expr::Window(_, child, period) => {
            *period = snap_period(*period);
            snap_in_place(child);
        }
    }
}

/// Whether `expr` is a terminal (leaf) node.
fn is_terminal(expr: &Expr) -> bool {
    matches!(expr, Expr::Input(_) | Expr::Const(_))
}

/// Collapse the leftmost bottom-most *internal, non-root* node (one whose children are all terminals)
/// to a terminal. Returns `false` only when the root is a window directly over a terminal (nothing
/// below to prune). Each successful call strictly reduces the node count, so the `repair` loop halts;
/// because node count bounds depth, the depth/lookback caps are also reached.
fn prune_deepest_descendant(root: &mut Expr) -> bool {
    fn prune(e: &mut Expr, is_root: bool) -> bool {
        // Recurse deepest-first so a bottom internal node is collapsed before its ancestors.
        match e {
            Expr::Input(_) | Expr::Const(_) => return false,
            Expr::Unary(_, c) | Expr::Window(_, c, _) => {
                if prune(c, false) {
                    return true;
                }
            }
            Expr::Binary(_, a, b) => {
                if prune(a, false) || prune(b, false) {
                    return true;
                }
            }
        }
        if is_root {
            return false; // never collapse the (normalising) root itself
        }
        let children_all_terminal = match e {
            Expr::Unary(_, c) | Expr::Window(_, c, _) => is_terminal(c),
            Expr::Binary(_, a, b) => is_terminal(a) && is_terminal(b),
            _ => false,
        };
        if children_all_terminal {
            *e = Expr::Input(Field::Close);
            return true;
        }
        false
    }
    prune(root, true)
}

/// Canonicalise (§8): constant-fold, order commutative operands, and collapse outer affine wrappers under
/// a normalising root. **`Rank`** is invariant to every strictly-increasing transform and **`Zscore`** is
/// invariant to positive-affine transforms (`zscore(a·x+b) = zscore(x)` for `a > 0`, since standardising
/// removes the mean and the positive scale) — both collapse the same positive-affine outer layers
/// ([`strip_monotone_incr`]), so `Rank(a·x+b)`→`Rank(x)` and `Zscore(a·x+b)`→`Zscore(x)`. Extending the
/// strip to `Zscore` (QE-451 Phase 1b) removes an over-count of Zscore-affine equivalents from the
/// distinct-canonical trial basis. **Purely additive** — it changes only the canonical form of trees whose
/// normalising root wraps a strippable affine layer; every other tree's canonical form (and `formula_hash`)
/// is unchanged.
#[must_use]
pub fn canonicalize(expr: &Expr) -> Expr {
    let folded = fold(expr);
    match &folded {
        Expr::Window(op @ (WinOp::Rank | WinOp::Zscore), child, n) => {
            Expr::Window(*op, Box::new(strip_monotone_incr(child)), *n)
        }
        _ => folded,
    }
}

/// Constant-fold + commutative-order-normalise, bottom-up.
fn fold(expr: &Expr) -> Expr {
    match expr {
        Expr::Input(_) | Expr::Const(_) => expr.clone(),
        Expr::Unary(op, child) => {
            let c = fold(child);
            if let Expr::Const(v) = c {
                Expr::Const(apply_unary(*op, v))
            } else {
                Expr::Unary(*op, Box::new(c))
            }
        }
        Expr::Binary(op, a, b) => {
            let mut fa = fold(a);
            let mut fb = fold(b);
            if let (Expr::Const(x), Expr::Const(y)) = (&fa, &fb) {
                return Expr::Const(apply_binary(*op, *x, *y));
            }
            // Order commutative operands by a canonical key so `add(a,b)` and `add(b,a)` coincide.
            if matches!(op, BinOp::Add | BinOp::Mul) && canon_key(&fb) < canon_key(&fa) {
                std::mem::swap(&mut fa, &mut fb);
            }
            Expr::Binary(*op, Box::new(fa), Box::new(fb))
        }
        Expr::Window(op, child, n) => Expr::Window(*op, Box::new(fold(child)), *n),
    }
}

/// Strip strictly-monotone-**increasing** affine outer wrappers (rank-preserving under a `Rank` root):
/// `add(_, c)`, `sub(_, c)`, `mul(_, c>0)`, `div(_, c>0)`. Stops at the first non-collapsible layer.
/// `Neg`/`Abs`/`Sign` are not monotone-increasing and are never stripped.
fn strip_monotone_incr(expr: &Expr) -> Expr {
    let mut cur = expr.clone();
    loop {
        cur = match cur {
            Expr::Binary(BinOp::Add, a, b) => match (&*a, &*b) {
                (Expr::Const(_), _) => (*b).clone(),
                (_, Expr::Const(_)) => (*a).clone(),
                _ => return Expr::Binary(BinOp::Add, a, b),
            },
            // `x - c` is increasing in x; `c - x` is decreasing → only strip a constant subtrahend.
            Expr::Binary(BinOp::Sub, a, b) if matches!(&*b, Expr::Const(_)) => (*a).clone(),
            Expr::Binary(BinOp::Mul, a, b) => match (&*a, &*b) {
                (Expr::Const(c), _) if c.is_sign_positive() && !c.is_zero() => (*b).clone(),
                (_, Expr::Const(c)) if c.is_sign_positive() && !c.is_zero() => (*a).clone(),
                _ => return Expr::Binary(BinOp::Mul, a, b),
            },
            // `x / c` is increasing for c > 0.
            Expr::Binary(BinOp::Div, a, b) if matches!(&*b, Expr::Const(c) if c.is_sign_positive() && !c.is_zero()) => {
                (*a).clone()
            }
            other => return other,
        };
    }
}

/// A total canonical ordering key over expressions (for commutative-operand ordering).
fn canon_key(expr: &Expr) -> String {
    let mut s = String::new();
    write_sexpr(expr, &mut s);
    s
}

/// Write a canonical S-expression for `expr`. Constants render via exact `Decimal` `Display` (no `f64`).
fn write_sexpr(expr: &Expr, out: &mut String) {
    use std::fmt::Write as _;
    match expr {
        Expr::Input(f) => out.push_str(field_tag(*f)),
        Expr::Const(c) => {
            out.push('#');
            let _ = write!(out, "{c}");
        }
        Expr::Unary(op, child) => {
            out.push('(');
            out.push_str(unop_tag(*op));
            out.push(' ');
            write_sexpr(child, out);
            out.push(')');
        }
        Expr::Binary(op, a, b) => {
            out.push('(');
            out.push_str(binop_tag(*op));
            out.push(' ');
            write_sexpr(a, out);
            out.push(' ');
            write_sexpr(b, out);
            out.push(')');
        }
        Expr::Window(op, child, n) => {
            out.push('(');
            out.push_str(winop_tag(*op));
            let _ = write!(out, " {n} ");
            write_sexpr(child, out);
            out.push(')');
        }
    }
}

fn field_tag(f: Field) -> &'static str {
    match f {
        Field::Close => "close",
        Field::High => "high",
        Field::Low => "low",
        Field::Volume => "volume",
        Field::Typical => "typical",
    }
}

fn unop_tag(op: UnOp) -> &'static str {
    match op {
        UnOp::Abs => "abs",
        UnOp::Sign => "sign",
        UnOp::Neg => "neg",
    }
}

fn binop_tag(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
    }
}

fn winop_tag(op: WinOp) -> &'static str {
    match op {
        WinOp::Mean => "mean",
        WinOp::Max => "max",
        WinOp::Min => "min",
        WinOp::Std => "std",
        WinOp::MeanAbsDev => "mad",
        WinOp::Delta => "delta",
        WinOp::Lag => "lag",
        WinOp::Rank => "rank",
        WinOp::Zscore => "zscore",
    }
}

// ---- seeded catalogue-equivalent formulas (default-off) --------------------------------------

fn boxed(expr: Expr) -> Box<Expr> {
    Box::new(expr)
}

/// `(a / b − 1) · 100` — the catalogue's `pct_change(from = b, to = a)`, same operation order.
fn pct_change(a: Expr, b: Expr) -> Expr {
    let ratio = Expr::Binary(BinOp::Div, boxed(a), boxed(b));
    let less_one = Expr::Binary(BinOp::Sub, boxed(ratio), boxed(Expr::Const(Decimal::ONE)));
    Expr::Binary(
        BinOp::Mul,
        boxed(less_one),
        boxed(Expr::Const(Decimal::from(100))),
    )
}

fn window(op: WinOp, field: Field, cap: Period) -> Expr {
    Expr::Window(op, boxed(Expr::Input(field)), cap)
}

fn lin(min: i64, max: i64, states: u16) -> Quantiser {
    Quantiser::Linear {
        min: Decimal::from(min),
        max: Decimal::from(max),
        states,
    }
}

/// The five catalogue indicators the seam reproduces byte-identically (QE-451 Phase 0). Each tuple is
/// `(catalogue id, formula, the catalogue's own quantiser)`. **Nothing in the default pipeline calls
/// this** — it exists only for the equivalence proof.
#[must_use]
pub fn seed_catalogue_subset(states: u16) -> Vec<(&'static str, Expr, Quantiser)> {
    vec![
        // sma_ratio_20 = (close / mean(close,20) − 1)·100
        (
            "sma_ratio_20",
            pct_change(
                Expr::Input(Field::Close),
                window(WinOp::Mean, Field::Close, 20),
            ),
            lin(-10, 10, states),
        ),
        // volume_ratio_20 = volume / mean(volume,20)
        (
            "volume_ratio_20",
            Expr::Binary(
                BinOp::Div,
                boxed(Expr::Input(Field::Volume)),
                boxed(window(WinOp::Mean, Field::Volume, 20)),
            ),
            lin(0, 4, states),
        ),
        // return_1 = (close / lag(close,1) − 1)·100 ; Lag capacity = k + 1 = 2
        (
            "return_1",
            pct_change(
                Expr::Input(Field::Close),
                Expr::Window(WinOp::Lag, boxed(Expr::Input(Field::Close)), 2),
            ),
            lin(-5, 5, states),
        ),
        // roc_10 = (close / lag(close,10) − 1)·100 ; Lag capacity = k + 1 = 11
        (
            "roc_10",
            pct_change(
                Expr::Input(Field::Close),
                Expr::Window(WinOp::Lag, boxed(Expr::Input(Field::Close)), 11),
            ),
            lin(-25, 25, states),
        ),
        // stoch_k_14 = (close − min(low,14)) / (max(high,14) − min(low,14)) · 100
        (
            "stoch_k_14",
            {
                let min_low = window(WinOp::Min, Field::Low, 14);
                let max_high = window(WinOp::Max, Field::High, 14);
                let numer = Expr::Binary(
                    BinOp::Sub,
                    boxed(Expr::Input(Field::Close)),
                    boxed(min_low.clone()),
                );
                let range = Expr::Binary(BinOp::Sub, boxed(max_high), boxed(min_low));
                Expr::Binary(
                    BinOp::Mul,
                    boxed(Expr::Binary(BinOp::Div, boxed(numer), boxed(range))),
                    boxed(Expr::Const(Decimal::from(100))),
                )
            },
            lin(0, 100, states),
        ),
    ]
}

/// Compile the seeded subset into indicators (opt-in; the default catalogue never includes these).
#[must_use]
pub fn seed_indicators(states: u16) -> Vec<Box<dyn Indicator>> {
    seed_catalogue_subset(states)
        .into_iter()
        .map(|(id, expr, q)| compile(id, &expr, q))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::CatalogueIdentity;
    use crate::indicator::{catalogue, compute_batch, CatalogueConfig, CATALOGUE_VERSION};
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }

    /// The exact series ac1/ac2 use in `indicator/mod.rs` — positive, gently oscillating prices, all
    /// scalar context present.
    fn series(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                let base = 100 + (i64i % 7) * 3 - (i64i % 3) * 2 + i64i / 5;
                let high = base + 5 + (i64i % 4);
                let low = base - 5 - (i64i % 5);
                let close = base + (i64i % 3) - 1;
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 5 * MIN),
                    Resolution::M5,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(high)).unwrap(),
                    Price::new(dec(low)).unwrap(),
                    Price::new(dec(close)).unwrap(),
                    Qty::new(dec(10 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample {
                    bar,
                    funding: Some(Decimal::new((i64i % 5) - 2, 4)),
                    open_interest: Some(dec(1000 + i64i * 7)),
                    premium: Some(Decimal::new((i64i % 3) - 1, 4)),
                }
            })
            .collect()
    }

    #[test]
    fn max_lookback_recursion() {
        // leaf → 1, const → 0
        assert_eq!(max_lookback(&Expr::Input(Field::Close)), 1);
        assert_eq!(max_lookback(&Expr::Const(dec(7))), 0);
        // unary → child
        assert_eq!(
            max_lookback(&Expr::Unary(UnOp::Neg, boxed(Expr::Input(Field::Close)))),
            1
        );
        // binary → max(children)
        let bin = Expr::Binary(
            BinOp::Sub,
            boxed(window(WinOp::Mean, Field::Close, 20)),
            boxed(Expr::Input(Field::High)),
        );
        assert_eq!(max_lookback(&bin), 20);
        // window(op, child, cap) → (cap − 1) + child ; leaf child = 1 ⇒ cap
        assert_eq!(max_lookback(&window(WinOp::Mean, Field::Close, 20)), 20);
        assert_eq!(max_lookback(&window(WinOp::Max, Field::High, 14)), 14);
        // Lag capacity 2 ⇒ lookback 2 ; capacity 11 ⇒ 11
        assert_eq!(
            max_lookback(&Expr::Window(
                WinOp::Lag,
                boxed(Expr::Input(Field::Close)),
                2
            )),
            2
        );
        // nested windows compose: (cap_outer − 1) + cap_inner
        let nested = Expr::Window(WinOp::Mean, boxed(window(WinOp::Max, Field::High, 4)), 3);
        assert_eq!(max_lookback(&nested), (3 - 1) + 4);
    }

    #[test]
    fn seed_lookbacks_match_the_catalogue() {
        // Declared lookback per seed == the hand-written catalogue's declared lookback.
        let want = [
            ("sma_ratio_20", 20usize),
            ("volume_ratio_20", 20),
            ("return_1", 2),
            ("roc_10", 11),
            ("stoch_k_14", 14),
        ];
        for (id, expr, _q) in seed_catalogue_subset(5) {
            let expect = want.iter().find(|(w, _)| *w == id).unwrap().1;
            assert_eq!(max_lookback(&expr), expect, "lookback for {id}");
        }
    }

    #[test]
    fn ac1_batch_equals_streaming_for_every_seed() {
        let samples = series(80);
        for mut ind in seed_indicators(5) {
            let batch = compute_batch(ind.as_mut(), &samples);
            ind.reset();
            let streamed: Vec<_> = samples.iter().map(|s| ind.update(s)).collect();
            assert_eq!(batch, streamed, "batch≠streaming for {}", ind.spec().id);
            // Reproducible across a second fresh run.
            ind.reset();
            let again: Vec<_> = samples.iter().map(|s| ind.update(s)).collect();
            assert_eq!(batch, again, "not reproducible for {}", ind.spec().id);
        }
    }

    #[test]
    fn ac2_warmup_emits_none_until_exactly_lookback_then_some() {
        let samples = series(120);
        for mut ind in seed_indicators(5) {
            let lookback = ind.spec().lookback;
            let id = ind.spec().id;
            for (i, s) in samples.iter().enumerate() {
                let out = ind.update(s);
                if i + 1 < lookback {
                    assert!(out.is_none(), "{id} emitted before lookback at i={i}");
                } else if i + 1 == lookback {
                    assert!(out.is_some(), "{id} did not emit at exactly lookback");
                    break;
                }
            }
        }
    }

    #[test]
    fn ac2_latest_output_independent_of_out_of_window_samples() {
        let base = series(120);
        for mut ind in seed_indicators(5) {
            let lookback = ind.spec().lookback;
            let id = ind.spec().id;

            let original = compute_batch(ind.as_mut(), &base);
            let last_state = *original.last().unwrap();

            let mut perturbed = base.clone();
            let idx = perturbed.len() - 1 - lookback;
            perturbed[idx] = perturb(&perturbed[idx]);

            ind.reset();
            let after = compute_batch(ind.as_mut(), &perturbed);
            assert_eq!(
                *after.last().unwrap(),
                last_state,
                "{id}: latest output changed by an out-of-window sample"
            );
        }
    }

    fn perturb(s: &Sample) -> Sample {
        let b = &s.bar;
        let bump = |p: Price| Price::new(p.get() + dec(50)).unwrap();
        let bar = Bar::new(
            b.open_time(),
            b.resolution(),
            bump(b.open()),
            bump(b.high()),
            bump(b.low()),
            bump(b.close()),
            Qty::new(b.volume().get() + dec(100)).unwrap(),
            b.trades() + 7,
        )
        .unwrap();
        Sample {
            bar,
            funding: Some(dec(1)),
            open_interest: Some(dec(999_999)),
            premium: Some(dec(1)),
        }
    }

    #[test]
    fn seed_reproduces_catalogue_byte_for_byte() {
        // The equivalence proof: each Expr-backed indicator == its hand-written catalogue twin,
        // bar-for-bar (warmup `None`s and every quantised state), over the shared ac1/ac2 series.
        let cfg = CatalogueConfig::default();
        let samples = series(120);
        let mut cat = catalogue(&cfg);

        for (id, expr, q) in seed_catalogue_subset(cfg.states) {
            let cat_ind = cat
                .iter_mut()
                .find(|i| i.spec().id == id)
                .unwrap_or_else(|| panic!("catalogue missing {id}"));
            let cat_out = compute_batch(cat_ind.as_mut(), &samples);

            let mut expr_ind = compile(id, &expr, q);
            let expr_out = compute_batch(expr_ind.as_mut(), &samples);

            assert_eq!(
                cat_out, expr_out,
                "Expr twin diverges from catalogue for {id}"
            );
            // Non-vacuous: the series actually warms this indicator up to real states.
            assert!(
                expr_out.iter().any(Option::is_some),
                "{id}: equivalence over an all-None stream is vacuous"
            );
        }
    }

    #[test]
    fn seam_is_default_off_no_golden_moved() {
        // The default catalogue is untouched: the seed ids are absent, size/version/identity unchanged.
        let cfg = CatalogueConfig::default();
        let cat_ids: Vec<String> = catalogue(&cfg).iter().map(|i| i.spec().id).collect();
        assert_eq!(cat_ids.len(), 22, "default catalogue size changed");
        assert_eq!(
            CATALOGUE_VERSION, 1,
            "CATALOGUE_VERSION must not bump in Phase 0"
        );

        // The Expr seeds REPRODUCE catalogue ids (same id strings), but the point is the default
        // catalogue is not enlarged — its identity hash is exactly what the current build produces.
        let identity_now = CatalogueIdentity::from_config(&cfg);
        assert_eq!(
            identity_now,
            CatalogueIdentity::current(),
            "catalogue identity moved — a golden would move"
        );

        // And the assembler-facing schema width is still 22 (seam adds nothing to the pipeline).
        assert_eq!(
            crate::feature::FeatureSchema::from_catalogue(&cfg).len(),
            22
        );
    }

    // ---- QE-451 Phase 1a: grammar (normalising roots) --------------------------------------------

    #[test]
    fn rank_root_is_bounded_zero_to_one_and_fir() {
        // Strictly increasing series → the newest value is the largest → rank = (n-1)/n each full bar.
        let samples: Vec<Sample> = (0..30)
            .map(|i| {
                let p = 100 + i as i64;
                Sample::from_bar(
                    Bar::new(
                        Timestamp::from_millis(i as i64 * 5 * MIN),
                        Resolution::M5,
                        Price::new(dec(p)).unwrap(),
                        Price::new(dec(p + 1)).unwrap(),
                        Price::new(dec(p - 1)).unwrap(),
                        Price::new(dec(p)).unwrap(),
                        Qty::new(dec(10)).unwrap(),
                        1,
                    )
                    .unwrap(),
                )
            })
            .collect();
        let tree = Expr::Window(WinOp::Rank, boxed(Expr::Input(Field::Close)), 10);
        assert_eq!(max_lookback(&tree), 10);
        let out = eval_stream(&tree, &samples);
        // Warm only from bar index 9 onward.
        assert!(out[..9].iter().all(Option::is_none));
        for v in out[9..].iter().flatten() {
            assert!(
                *v >= Decimal::ZERO && *v < Decimal::ONE,
                "rank out of [0,1): {v}"
            );
            assert_eq!(*v, Decimal::from(9) / Decimal::from(10)); // strictly-increasing ⇒ 9/10
        }
    }

    #[test]
    fn zscore_root_is_clipped_and_zero_on_flat_window() {
        // A flat window → std = 0 → z-score defined as 0.
        let flat: Vec<Sample> = (0..12)
            .map(|i| {
                Sample::from_bar(
                    Bar::new(
                        Timestamp::from_millis(i as i64 * 5 * MIN),
                        Resolution::M5,
                        Price::new(dec(100)).unwrap(),
                        Price::new(dec(100)).unwrap(),
                        Price::new(dec(100)).unwrap(),
                        Price::new(dec(100)).unwrap(),
                        Qty::new(dec(10)).unwrap(),
                        1,
                    )
                    .unwrap(),
                )
            })
            .collect();
        let tree = Expr::Window(WinOp::Zscore, boxed(Expr::Input(Field::Close)), 5);
        let out = eval_stream(&tree, &flat);
        assert_eq!(out.last().unwrap().unwrap(), Decimal::ZERO);
        assert!(WinOp::Zscore.is_normalising() && WinOp::Rank.is_normalising());
        assert!(!WinOp::Mean.is_normalising());
    }

    // ---- QE-451 Phase 1a: ExprTree::repair -------------------------------------------------------

    fn deep_binary_chain(depth: usize) -> Expr {
        // A left-leaning add chain of `depth` internal nodes over Close — depth/nodes over cap.
        let mut e = Expr::Input(Field::Close);
        for _ in 0..depth {
            e = Expr::Binary(BinOp::Add, boxed(e), boxed(Expr::Input(Field::High)));
        }
        e
    }

    #[test]
    fn repair_forces_a_normalising_root() {
        // A non-window root is wrapped in Rank.
        let t = ExprTree::repaired(Expr::Input(Field::Close));
        assert_eq!(t.root_op(), Some(WinOp::Rank));
        // A non-normalising window root (Mean) is also wrapped.
        let t2 = ExprTree::repaired(window(WinOp::Mean, Field::Close, 20));
        assert!(matches!(t2.root_op(), Some(op) if op.is_normalising()));
        // An already-normalising root is preserved (no double wrap).
        let z = Expr::Window(WinOp::Zscore, boxed(Expr::Input(Field::Close)), 10);
        let t3 = ExprTree::repaired(z);
        assert_eq!(t3.root_op(), Some(WinOp::Zscore));
        assert_eq!(t3.node_count(), 2);
    }

    #[test]
    fn repair_snaps_periods_and_constants() {
        // Period 7 → 5, constant 0.4 → 0.5; verified through the repaired canonical text.
        let raw = Expr::Window(
            WinOp::Zscore,
            boxed(Expr::Binary(
                BinOp::Add,
                boxed(Expr::Input(Field::Close)),
                boxed(Expr::Const(Decimal::new(4, 1))), // 0.4
            )),
            7,
        );
        let t = ExprTree::repaired(raw);
        // Root period snapped to a lattice member.
        assert!(PERIOD_LATTICE.contains(&match t.root() {
            Expr::Window(_, _, n) => *n,
            _ => unreachable!(),
        }));
        // Every constant in the tree is on the grid.
        fn all_consts_on_grid(e: &Expr) -> bool {
            match e {
                Expr::Const(c) => CONST_GRID.contains(c),
                Expr::Input(_) => true,
                Expr::Unary(_, c) | Expr::Window(_, c, _) => all_consts_on_grid(c),
                Expr::Binary(_, a, b) => all_consts_on_grid(a) && all_consts_on_grid(b),
            }
        }
        assert!(all_consts_on_grid(t.root()));
        assert_eq!(snap_period(7), 5);
        assert_eq!(snap_period(35), 20); // 35↔20 = 15, 35↔50 = 15 → tie broken to the lower (20)
        assert_eq!(snap_period(80), 100); // 80↔50 = 30, 80↔100 = 20 → 100
        assert_eq!(snap_const(Decimal::new(4, 1)), Decimal::new(5, 1));
    }

    #[test]
    fn repair_enforces_all_caps() {
        // A pathologically deep/wide tree is pruned within every cap.
        let raw = Expr::Window(WinOp::Mean, boxed(deep_binary_chain(30)), 200);
        let t = ExprTree::repaired(raw);
        assert!(t.depth() <= MAX_DEPTH, "depth {}", t.depth());
        assert!(t.node_count() <= MAX_NODES, "nodes {}", t.node_count());
        assert!(
            t.lookback() <= MAX_TOTAL_LOOKBACK,
            "lookback {}",
            t.lookback()
        );
        assert!(t.root_op().map(WinOp::is_normalising).unwrap_or(false));
        // Cached lookback equals the exact recomputation.
        assert_eq!(t.lookback(), max_lookback(t.root()));
    }

    #[test]
    fn repair_is_idempotent() {
        // Over a spread of raw trees, repair∘repair == repair.
        let raws = vec![
            Expr::Input(Field::Close),
            window(WinOp::Mean, Field::Close, 7),
            Expr::Window(WinOp::Mean, boxed(deep_binary_chain(30)), 123),
            Expr::Binary(
                BinOp::Div,
                boxed(Expr::Input(Field::Close)),
                boxed(window(WinOp::Std, Field::Close, 33)),
            ),
            Expr::Window(WinOp::Zscore, boxed(Expr::Const(Decimal::new(37, 2))), 88),
        ];
        for raw in raws {
            let once = ExprTree::repaired(raw);
            let mut twice = once.clone();
            twice.repair();
            assert_eq!(once, twice, "repair not idempotent");
            // Post-conditions hold on the once-repaired tree.
            assert!(once.depth() <= MAX_DEPTH);
            assert!(once.node_count() <= MAX_NODES);
            assert!(once.lookback() <= MAX_TOTAL_LOOKBACK);
            assert!(once.root_op().map(WinOp::is_normalising).unwrap_or(false));
        }
    }

    #[test]
    fn protected_div_zero() {
        // The grammar's only division is protected: |denom| < ε ⇒ 0 (interpreter invariant).
        assert_eq!(
            apply_binary(BinOp::Div, dec(5), Decimal::ZERO),
            Decimal::ZERO
        );
        assert_eq!(apply_binary(BinOp::Div, dec(6), dec(3)), dec(2));
    }

    // ---- QE-451 Phase 1a: canonicalisation + content hash ----------------------------------------

    #[test]
    fn canonical_collapses_equivalent_trees() {
        // (a) commutative order: add(close,high) == add(high,close).
        let a = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Binary(
                BinOp::Add,
                boxed(Expr::Input(Field::Close)),
                boxed(Expr::Input(Field::High)),
            )),
            50,
        ));
        let b = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Binary(
                BinOp::Add,
                boxed(Expr::Input(Field::High)),
                boxed(Expr::Input(Field::Close)),
            )),
            50,
        ));
        assert_eq!(a.canonical_hash(), b.canonical_hash());

        // (b) constant folding: mul(#2,#5) folds to #10.
        let folded = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Binary(
                BinOp::Mul,
                boxed(Expr::Const(dec(2))),
                boxed(Expr::Const(dec(5))),
            )),
            50,
        ));
        let direct = ExprTree::new(Expr::Window(WinOp::Rank, boxed(Expr::Const(dec(10))), 50));
        assert_eq!(folded.canonical_hash(), direct.canonical_hash());

        // (c) rank-monotone wrapper collapse: rank(mul(x, #2)) == rank(x) (positive scale, increasing).
        let wrapped = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Binary(
                BinOp::Mul,
                boxed(window(WinOp::Mean, Field::Close, 20)),
                boxed(Expr::Const(dec(2))),
            )),
            50,
        ));
        let bare = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(window(WinOp::Mean, Field::Close, 20)),
            50,
        ));
        assert_eq!(wrapped.canonical_hash(), bare.canonical_hash());

        // Neg is monotone-DEcreasing → NOT collapsed → distinct hash.
        let negd = ExprTree::new(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Unary(
                UnOp::Neg,
                boxed(window(WinOp::Mean, Field::Close, 20)),
            )),
            50,
        ));
        assert_ne!(negd.canonical_hash(), bare.canonical_hash());
    }

    #[test]
    fn zscore_root_collapses_positive_affine_wrappers() {
        // QE-451 Phase 1b: Zscore is affine-invariant — `zscore(a·x + b) = zscore(x)` for `a > 0`, since
        // standardising removes the mean and the positive scale. So positive-affine outer wrappers collapse
        // under a `Zscore` root exactly as under `Rank`, removing an over-count of Zscore-affine
        // equivalents from the distinct-canonical trial basis.
        let bare = ExprTree::new(Expr::Window(
            WinOp::Zscore,
            boxed(window(WinOp::Mean, Field::Close, 20)),
            50,
        ));
        // zscore(mean·#2 + #5) == zscore(mean): a nested positive-affine wrapper (×2 then +5).
        let affine = ExprTree::new(Expr::Window(
            WinOp::Zscore,
            boxed(Expr::Binary(
                BinOp::Add,
                boxed(Expr::Binary(
                    BinOp::Mul,
                    boxed(window(WinOp::Mean, Field::Close, 20)),
                    boxed(Expr::Const(dec(2))),
                )),
                boxed(Expr::Const(dec(5))),
            )),
            50,
        ));
        assert_eq!(affine.canonical_hash(), bare.canonical_hash());
        assert_eq!(affine.canonical_sexpr(), bare.canonical_sexpr());

        // `Neg` flips sign (`zscore(−x) = −zscore(x)`) → NOT collapsed → distinct hash.
        let negd = ExprTree::new(Expr::Window(
            WinOp::Zscore,
            boxed(Expr::Unary(
                UnOp::Neg,
                boxed(window(WinOp::Mean, Field::Close, 20)),
            )),
            50,
        ));
        assert_ne!(negd.canonical_hash(), bare.canonical_hash());

        // Additive, not over-collapsing: a genuinely different Zscore formula still hashes differently.
        let different = ExprTree::new(Expr::Window(
            WinOp::Zscore,
            boxed(window(WinOp::Mean, Field::High, 20)),
            50,
        ));
        assert_ne!(different.canonical_hash(), bare.canonical_hash());
    }

    #[test]
    fn distinct_canonical_count_over_a_set() {
        use std::collections::HashSet;
        // Three trees, two of which are canonically equivalent (commutative order) → 2 distinct.
        let trees = [
            ExprTree::new(Expr::Window(
                WinOp::Rank,
                boxed(Expr::Binary(
                    BinOp::Add,
                    boxed(Expr::Input(Field::Close)),
                    boxed(Expr::Input(Field::High)),
                )),
                50,
            )),
            ExprTree::new(Expr::Window(
                WinOp::Rank,
                boxed(Expr::Binary(
                    BinOp::Add,
                    boxed(Expr::Input(Field::High)),
                    boxed(Expr::Input(Field::Close)),
                )),
                50,
            )),
            ExprTree::new(Expr::Window(
                WinOp::Zscore,
                boxed(Expr::Input(Field::Low)),
                10,
            )),
        ];
        let distinct: HashSet<String> = trees.iter().map(ExprTree::canonical_hash).collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn nth_node_traverses_preorder() {
        // (add close high): pre-order = [add, close, high].
        let e = Expr::Binary(
            BinOp::Add,
            boxed(Expr::Input(Field::Close)),
            boxed(Expr::Input(Field::High)),
        );
        assert_eq!(count_nodes(&e), 3);
        assert!(matches!(nth_node(&e, 0), Some(Expr::Binary(..))));
        assert!(matches!(nth_node(&e, 1), Some(Expr::Input(Field::Close))));
        assert!(matches!(nth_node(&e, 2), Some(Expr::Input(Field::High))));
        assert!(nth_node(&e, 3).is_none());
        // Mutable replace of node 1.
        let mut m = e.clone();
        *nth_node_mut(&mut m, 1).unwrap() = Expr::Input(Field::Low);
        assert!(matches!(nth_node(&m, 1), Some(Expr::Input(Field::Low))));
    }
}
