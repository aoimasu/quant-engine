# QE-451 Phase 0 — `Expr`/`Kernel` tree-interpreter seam proof (design + evidence)

*Scope: **Phase 0 only** of the QE-450 GP-indicator epic — the seam proof. No search, no evolution,
no archive, no deflation. Behind an explicit opt-in (default-off); the production catalogue is
UNCHANGED.*

Spec of record: [`qe-450-gp-indicator-evolution-design.md`](./qe-450-gp-indicator-evolution-design.md)
— see §4.1 (Expr genome), §4.2 (primitive grammar), §6 (invariants), §9 (Phase 0 row), §10 AC 1–4.

---

## 1. Goal (the load-bearing claim being proved)

QE-450's single load-bearing decision is that **variable indicator structure lives behind the existing
`Indicator`/`Kernel` trait**, never in the strategy genome. Phase 0 proves that seam is real and free:

- an expression tree `Expr` compiles to a `Kernel`, so it rides the existing
  `impl<K: Kernel> Indicator` blanket impl (`indicator/mod.rs:105`) and the one `update()` path that
  *is* `compute_batch` → **batch = streaming parity for free**;
- a pure `max_lookback(&Expr)` recursion yields the **exact** FIR span → feeds `IndicatorSpec.lookback`
  → purge/embargo stays correct with zero `cv.rs` changes;
- the tree interpreter is **`rust_decimal` only, no `f64`** on any evaluation path;
- a subset of the hand-written 22-indicator catalogue is reproduced **byte-identically** (bar-for-bar
  equal `QState` stream) by `Expr`-backed indicators;
- an independent **slow-reference oracle** (naive O(n·window) recompute, sharing no code with the
  streaming `Roll`-folding interpreter) equals the streaming interpreter bar-for-bar.

Everything is **default-off**: nothing in the default pipeline references the seam, so no golden moves
and `CATALOGUE_VERSION` does not bump.

## 2. Current-state evidence

- `crates/signal/src/indicator/mod.rs` — `trait Kernel { id/lookback/quantiser/observe/warm/raw/clear }`
  and `impl<K: Kernel> Indicator for K`; `update()` = `observe` → `if warm() { raw().map(quantise) }`.
  `compute_batch` is literally the `update` loop, so **any** `Kernel` gets batch==streaming by
  construction. `CATALOGUE_VERSION = 1`. ac1 (batch==streaming) and ac2 (warmup / out-of-window
  independence) tests iterate `catalogue(&cfg)`.
- `crates/signal/src/indicator/roll.rs` — `Roll`, a fixed-cap ring buffer over exact `Decimal` with
  `mean/max/min/std_pop/mean_abs_dev/first/last/is_full`. The FIR substrate the Expr windows reuse.
- `crates/signal/src/indicator/price.rs` — the hand-written value functions. Reproduced subset:
  `sma_ratio` = `(close/mean(close) − 1)·100`; `roc` = `(close_last/close_first − 1)·100`;
  `stoch_k` = `(close − min(low))/(max(high) − min(low))·100`; `volume_ratio` = `volume/mean(volume)`.
- `crates/signal/src/indicator/quant.rs` — the point-wise stateless `Quantiser` (`Linear`/`Bands`);
  **unchanged** — the Expr indicators carry the catalogue's own quantiser (Phase 0 reproduces the
  catalogue's direct quantisation; the strongly-typed rank/zscore *root* is a Phase-1 search constraint).
- `crates/signal/src/feature.rs` — `FeatureSchema`/`CatalogueIdentity` derive identity from
  `catalogue(cfg)`. Leaving `catalogue()` untouched keeps `CatalogueIdentity::current().id_hash`
  byte-identical → **no golden moved**.
- `crates/wfo/tests/qe432_slow_reference_oracle.rs` — the QE-432 pattern this phase mirrors for trees:
  an independent naive re-derivation, property-tested `optimised == reference` over seeded random
  inputs, plus a **mutation guard** that proves the oracle is non-vacuous.
- Workspace lints (`Cargo.toml`): `clippy::unwrap_used = "deny"` on production code (tests exempt).
  The interpreter uses no `.unwrap()` / `f64`.

## 3. The `Expr` type and grammar (Phase 0, FIR-only)

```rust
pub enum Field { Close, High, Low, Volume, Typical }   // price terminals only (flow gated to Phase 1)
pub enum UnOp  { Abs, Sign, Neg }
pub enum BinOp { Add, Sub, Mul, Div }                  // Div is protected: |denom| < ε ⇒ 0
pub enum WinOp { Mean, Max, Min, Std, MeanAbsDev, Delta, Lag }   // all strictly-causal FIR

pub enum Expr {
    Input(Field),
    Const(Decimal),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Window(WinOp, Box<Expr>, Period),   // Period = the window's Roll capacity, in bars
}
```

**Unit convention (makes `max_lookback` a single uniform recursion).** `Period` is the *roll capacity*
in bars for every window op. For `Mean/Max/Min/Std/MeanAbsDev` that is the window length `n`. For the
temporal ops the capacity encodes the reach: `Lag(x, k)` / `Delta(x, k)` (value/difference `k` bars ago)
is stored with `Period = k + 1` (current bar + `k` of history). This lets one rule cover all window ops.

**`max_lookback(&Expr)` (pure recursion, exact FIR span):**

```
Input(_)            → 1
Const(_)            → 0
Unary(_, c)         → max_lookback(c)
Binary(_, a, b)     → max(max_lookback(a), max_lookback(b))
Window(_, c, cap)   → (cap − 1) + max_lookback(c)
```

FIR-only: no EWMA/IIR, no expanding/cumulative/all-time ops, no forward/lead ops, no transcendentals
beyond the `Roll::std_pop` `sqrt` already golden-tested in the catalogue. Flow terminals
(Funding/OI/Premium) are **excluded** in Phase 0 (their lookback is in present-scalars, not bars, until
dense forward-fill lands — QE-450 §4.2 / Risk 5).

## 4. Compilation to a `Kernel` (parity by construction)

`Expr::compile(id, quantiser)` lowers the tree into a mutable `ExprKernel` whose `Window` nodes each own
a `Roll`. One post-order pass per `observe(sample)` folds every node:

- `eval(node) -> Option<Decimal>` (single pass, **no short-circuit** — both Binary children are always
  evaluated so every nested window advances its roll every bar):
  - `Input(f)` → current bar field (always `Some`); `Const(c)` → `Some(c)`;
  - `Unary` → child value mapped; `Binary` → combine both child values (protected div);
  - `Window(op, child, _)` → let `v = eval(child)`; if `Some(v)` push into the roll; then
    `if roll.is_full() { Some(aggregate(op, roll)) } else { None }`.
- `observe` caches the root's `eval` as the current raw; `warm()` = cached `is_some()`;
  `raw()` = cached; `lookback()` = the pre-computed `max_lookback`.

A `Window` only advances when its child is defined, so nested windows fill exactly on the
`max_lookback` schedule (inner warm at `cap_inner`, outer `cap_outer` values later
= `(cap_outer−1)+cap_inner`), keeping `warm()` aligned with the declared lookback (ac2's `⊇` direction).
Because a `Kernel` is driven only through `update()`, batch == streaming holds structurally (ac1), and
the value→state map is the unchanged point-wise `Quantiser` (QE-450 §4.4 / §6 invariant, `quant.rs`
zero lines changed).

## 5. Reproduced catalogue subset (byte-identity proof)

Five indicators, chosen because their value function is a total pure-Decimal FIR expression that matches
the catalogue's exact operation order on the standard positive-price test series (so the `QState` stream
is bar-for-bar identical, warmup `None`s included):

| catalogue id | lookback | `Expr` (schematic) |
|---|---|---|
| `sma_ratio_20`    | 20 | `(Close / Mean(Close,20) − 1) · 100` |
| `volume_ratio_20` | 20 | `Volume / Mean(Volume,20)` |
| `return_1`        | 2  | `(Close / Lag(Close,k=1) − 1) · 100` |
| `roc_10`          | 11 | `(Close / Lag(Close,k=10) − 1) · 100` |
| `stoch_k_14`      | 14 | `(Close − Min(Low,14)) / (Max(High,14) − Min(Low,14)) · 100` |

Each carries the catalogue's own quantiser (`lin(−10,10)`, `lin(0,4)`, `lin(−5,5)`, `lin(−25,25)`,
`lin(0,100)`). The equivalence test builds both the hand-written catalogue indicator and its `Expr`
twin and asserts `compute_batch` is element-wise equal over the shared `series(120)` used by ac1/ac2.
Declared lookback equals the catalogue's declared lookback for each, so purge/embargo sizing is
unchanged.

## 6. Test plan

All tests run in the **default** `cargo test --workspace` gate (they are not feature-gated), so the seam
proof is covered by the mandated green gate.

1. `expr` unit tests (`indicator/expr.rs`):
   - `max_lookback` recursion on hand-worked trees (leaf/const/unary/binary/window/nested).
   - ac1 generalised: `compute_batch` == streaming for every seeded Expr indicator.
   - ac2 generalised: emits `None` until exactly `lookback`, then `Some`; and perturbing a sample older
     than `lookback` leaves the latest `QState` byte-identical.
   - **equivalence**: each seeded Expr indicator == its catalogue twin bar-for-bar.
   - default-off: the seed ids are absent from `catalogue()`; `catalogue()` length/ids and
     `CatalogueIdentity::current()` are unchanged; `CATALOGUE_VERSION == 1`.
2. `crates/signal/tests/qe451_expr_slow_reference_oracle.rs` (QE-432 style integration test):
   - an independent naive recompute (`reference_eval`, O(n·window) fresh scans, no `Roll` folding)
     equals the streaming `ExprKernel` raw output bar-for-bar over seeded random FIR trees
     (all window ops incl. Std/MeanAbsDev/Delta and nested windows);
   - a **mutation guard**: a bugged reference (e.g. window includes one bar too many) is caught on at
     least one case, proving the oracle is non-vacuous.

## 7. Default-off / no-golden decision (explicit)

The `Expr` machinery is compiled unconditionally (so its equivalence + oracle tests run in the standard
green gate — the proof must be *in* the mandated gate), but it is **opt-in by construction**: the seed
indicators are produced only by an explicit `expr::seed_catalogue_subset(...)` constructor that **nothing
in the default pipeline calls**. `catalogue()`, `FeatureSchema`, `CatalogueIdentity`, and the
`FeatureAssembler` are untouched. This is the "config off by default" gate QE-450 §9 asks for: the
default catalogue is byte-identical, so no golden fixture moves and `CATALOGUE_VERSION` stays `1`. A
dedicated test pins this (seed ids absent from the default catalogue; identity unchanged).

## 8. Explicitly out of scope (Phase 1a/1b — later PRs)

MAP-Elites tree archive, evolution/variation operators + `ExprTree::repair`, GP-aware deflation
(QE-439/434/436/430), cross-asset pooling, cost/turnover/capacity gates, freezing K formulas into
`CatalogueIdentity`, flow terminals, and the strongly-typed rank/zscore normalising root. None are built
here.

## 9. Risks

- *Divergence on degenerate inputs* — protected `Div` returns `0` where the catalogue returns `None`
  (zero denominator). Mitigated: the reproduced subset never hits a zero denominator on the positive
  test series (prices/volumes are ~100/~10), so the tested `QState` streams are byte-identical; the
  divergence is documented and confined to inputs the FIR indicators never see.
- *Warmup alignment on nested windows* — handled by the "advance only when child defined" rule, proved
  by the ac2 generalisation over nested trees and the slow-reference oracle.
- *`f64` creep* — the interpreter is `Decimal`-only; the oracle is `Decimal`-only; asserted by review +
  the QE-450 AC4 no-`f64` intent (the whole eval path is `Option<Decimal>`).
</content>
</invoke>
