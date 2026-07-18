//! QE-451 Phase 0 — independent slow-reference oracle for the `Expr` tree interpreter.
//!
//! Mirrors the QE-432 pattern (`crates/wfo/tests/qe432_slow_reference_oracle.rs`): a deliberately
//! naive, from-scratch re-derivation of the FIR tree's value at each bar — a fresh O(n·window) scan
//! that **shares no code with the incremental `Roll`-folding interpreter** — is property-tested
//! `streaming == reference` over seeded random FIR trees, plus a mutation guard proving the oracle is
//! non-vacuous. Independence is structural: the reference re-implements the aggregate contract by hand
//! and never calls `eval_stream`'s folding path. `rust_decimal` only, so equality is exact (no `f64`,
//! no tolerance). Design note: `docs/architecture/qe-451-phase0-expr-seam-design.md`.

use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
use qe_signal::indicator::expr::{
    eval_stream, max_lookback, BinOp, Expr, Field, Period, UnOp, WinOp,
};
use qe_signal::Sample;
use rust_decimal::{Decimal, MathematicalOps};

const CASES: u64 = 256;
const SERIES_LEN: usize = 64;
const MASTER_SEED: u64 = 0x5145_3435_3100_0001; // "QE451" tag
const MIN: i64 = 60_000;

// ---- a tiny deterministic RNG (splitmix64) — no external dep, byte-reproducible ------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn derive(master: u64, i: u64) -> u64 {
    let mut r = Rng(master ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    r.next();
    r.0
}

// ---- random FIR tree generation ------------------------------------------------------------------

fn rand_field(rng: &mut Rng) -> Field {
    match rng.below(5) {
        0 => Field::Close,
        1 => Field::High,
        2 => Field::Low,
        3 => Field::Volume,
        _ => Field::Typical,
    }
}

fn rand_unop(rng: &mut Rng) -> UnOp {
    match rng.below(3) {
        0 => UnOp::Abs,
        1 => UnOp::Sign,
        _ => UnOp::Neg,
    }
}

fn rand_binop(rng: &mut Rng) -> BinOp {
    match rng.below(4) {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        _ => BinOp::Div,
    }
}

fn rand_winop(rng: &mut Rng) -> WinOp {
    match rng.below(7) {
        0 => WinOp::Mean,
        1 => WinOp::Max,
        2 => WinOp::Min,
        3 => WinOp::Std,
        4 => WinOp::MeanAbsDev,
        5 => WinOp::Delta,
        _ => WinOp::Lag,
    }
}

fn rand_const(rng: &mut Rng) -> Decimal {
    // Small signed constant in [-5.00, 5.00].
    Decimal::new(rng.below(1001) as i64 - 500, 2)
}

/// Every leaf bottoms out at an `Input`, so a window's child always has lookback ≥ 1 (a bare `Const`
/// under a window would make the reference's warmup index arithmetic underflow). `Const` appears only
/// as a `Binary` operand.
fn gen(rng: &mut Rng, depth: u32) -> Expr {
    if depth == 0 {
        return Expr::Input(rand_field(rng));
    }
    match rng.below(5) {
        0 => Expr::Input(rand_field(rng)),
        1 => Expr::Unary(rand_unop(rng), Box::new(gen(rng, depth - 1))),
        2 => Expr::Binary(
            rand_binop(rng),
            Box::new(gen(rng, depth - 1)),
            Box::new(gen(rng, depth - 1)),
        ),
        3 => {
            let cap = 2 + rng.below(5) as Period; // 2..=6
            Expr::Window(rand_winop(rng), Box::new(gen(rng, depth - 1)), cap)
        }
        _ => Expr::Binary(
            rand_binop(rng),
            Box::new(gen(rng, depth - 1)),
            Box::new(Expr::Const(rand_const(rng))),
        ),
    }
}

fn random_series(rng: &mut Rng) -> Vec<Sample> {
    (0..SERIES_LEN)
        .map(|i| {
            let i64i = i as i64;
            let base = 100 + rng.below(50) as i64;
            let high = base + 1 + rng.below(10) as i64;
            let low = base - 1 - rng.below(10) as i64;
            let close = low + rng.below((high - low).max(1) as u64) as i64;
            let bar = Bar::new(
                Timestamp::from_millis(i64i * 5 * MIN),
                Resolution::M5,
                Price::new(Decimal::from(base)).expect("qe-451 test: valid by construction"),
                Price::new(Decimal::from(high)).expect("qe-451 test: valid by construction"),
                Price::new(Decimal::from(low)).expect("qe-451 test: valid by construction"),
                Price::new(Decimal::from(close)).expect("qe-451 test: valid by construction"),
                Qty::new(Decimal::from(1 + rng.below(100) as i64))
                    .expect("qe-451 test: valid by construction"),
                1,
            )
            .expect("qe-451 test: valid by construction");
            Sample::from_bar(bar)
        })
        .collect()
}

// ---- the independent reference (naive, no Roll fold) --------------------------------------------

/// Which reference bug (if any) to inject — used by the mutation guard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bug {
    /// The correct, independent reference.
    None,
    /// Off-by-one window: aggregate one extra, older bar (`capacity + 1`).
    WindowTooWide,
}

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
    let eps = Decimal::new(1, 9); // must mirror the interpreter's DIV_EPSILON (1e-9)
    match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y.abs() < eps {
                Decimal::ZERO
            } else {
                x / y
            }
        }
    }
}

/// Naive from-scratch aggregate over `vals` (oldest→newest), re-deriving each window op by hand.
fn aggregate(op: WinOp, vals: &[Decimal]) -> Option<Decimal> {
    if vals.is_empty() {
        return None;
    }
    let n = Decimal::from(vals.len());
    match op {
        WinOp::Mean => Some(vals.iter().copied().sum::<Decimal>() / n),
        WinOp::Max => vals.iter().copied().max(),
        WinOp::Min => vals.iter().copied().min(),
        WinOp::Std => {
            let mean = vals.iter().copied().sum::<Decimal>() / n;
            let var = vals
                .iter()
                .map(|&v| (v - mean) * (v - mean))
                .sum::<Decimal>()
                / n;
            var.sqrt()
        }
        WinOp::MeanAbsDev => {
            let mean = vals.iter().copied().sum::<Decimal>() / n;
            Some(vals.iter().map(|&v| (v - mean).abs()).sum::<Decimal>() / n)
        }
        WinOp::Delta => Some(*vals.last()? - *vals.first()?),
        WinOp::Lag => vals.first().copied(),
    }
}

/// The value of `expr` at bar `t`, recomputed independently (fresh scans, no incremental state).
fn reference_eval(expr: &Expr, samples: &[Sample], t: usize, bug: Bug) -> Option<Decimal> {
    match expr {
        Expr::Input(f) => Some(field_value(*f, &samples[t])),
        Expr::Const(c) => Some(*c),
        Expr::Unary(op, child) => {
            reference_eval(child, samples, t, bug).map(|v| apply_unary(*op, v))
        }
        Expr::Binary(op, a, b) => {
            match (
                reference_eval(a, samples, t, bug),
                reference_eval(b, samples, t, bug),
            ) {
                (Some(x), Some(y)) => Some(apply_binary(*op, x, y)),
                _ => None,
            }
        }
        Expr::Window(op, child, cap) => {
            // Not warm until the whole subtree's exact FIR span is covered.
            if t + 1 < max_lookback(expr) {
                return None;
            }
            let span = match bug {
                Bug::None => *cap,
                Bug::WindowTooWide => *cap + 1,
            };
            let start = (t + 1).saturating_sub(span);
            // Gather the child's defined values over the trailing `span` bars, oldest→newest.
            let mut vals = Vec::with_capacity(span);
            for i in start..=t {
                if let Some(v) = reference_eval(child, samples, i, bug) {
                    vals.push(v);
                }
            }
            aggregate(*op, &vals)
        }
    }
}

fn reference_stream(expr: &Expr, samples: &[Sample], bug: Bug) -> Vec<Option<Decimal>> {
    (0..samples.len())
        .map(|t| reference_eval(expr, samples, t, bug))
        .collect()
}

#[test]
fn expr_interpreter_matches_slow_reference_over_seeded_random_trees() {
    let mut non_vacuous = 0u64;
    for i in 0..CASES {
        let mut rng = Rng(derive(MASTER_SEED, i));
        let expr = gen(&mut rng, 3);
        let samples = random_series(&mut rng);

        let streaming = eval_stream(&expr, &samples);
        let reference = reference_stream(&expr, &samples, Bug::None);

        assert_eq!(
            streaming, reference,
            "case {i}: streaming interpreter != independent reference for {expr:?}"
        );
        if streaming.iter().any(Option::is_some) {
            non_vacuous += 1;
        }
    }
    // The corpus must actually warm up trees — an all-None corpus would pass vacuously.
    assert!(
        non_vacuous > CASES / 2,
        "corpus too weak: only {non_vacuous}/{CASES} cases produced any value"
    );
}

#[test]
fn slow_reference_oracle_is_non_vacuous_mutation_guard() {
    // On a corpus where the reference tracks the real optimised path exactly, a window off-by-one must
    // move the value on at least one case — otherwise the oracle would be vacuous.
    let mut caught = false;
    for i in 0..CASES {
        let mut rng = Rng(derive(MASTER_SEED ^ 0xF0, i));
        let expr = gen(&mut rng, 3);
        let samples = random_series(&mut rng);

        let streaming = eval_stream(&expr, &samples);
        let reference = reference_stream(&expr, &samples, Bug::None);
        let mutant = reference_stream(&expr, &samples, Bug::WindowTooWide);

        assert_eq!(
            streaming, reference,
            "reference must track the real interpreter (case {i})"
        );
        if reference != mutant {
            caught = true;
        }
    }
    assert!(
        caught,
        "mutation guard vacuous: a window off-by-one was never caught by the reference"
    );
}
