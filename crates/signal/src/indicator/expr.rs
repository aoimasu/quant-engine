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

use super::roll::Roll;
use super::{Indicator, Kernel, Quantiser, Sample};

/// Denominators with magnitude below this snap `protected_div` to zero (QE-450 §4.2 fixed zero
/// convention). Far below any real price/volume scale, so the reproduced catalogue subset never
/// trips it.
const DIV_EPSILON: Decimal = Decimal::from_parts(1, 0, 0, false, 9); // 1e-9

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
}
