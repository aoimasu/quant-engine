# QE-442 — Graded (probability-surface) signal combination

`Phase: Review R2 (P2 — panel #13, majority)` · `Area: signal` · `Depends on: QE-110, QE-107`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-442`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: Review R2.b.

## 1. The defect (current behaviour)

The quantiser (`crates/signal/src/indicator/quant.rs`) already computes an **ordinal** state
`QState(u16)` for every indicator — how far a continuous value sits into its bucket range. That ordinal
strength is then **discarded at the decision boundary**:

- `Clause::satisfied` (`genome.rs`) collapses the ordinal to a **bool**: `lo <= s <= hi`.
- `RuleSet::fires` collapses the k-of-n bank to a **bool**: `satisfied >= threshold`.
- `Genome::decide` emits a bare `Decision::Enter(dir)`; the backtester (`crates/wfo/src/backtest.rs`)
  reads a single `size_bps` gene and sizes **every** entry identically.

Consequence (Dubno, maxdama §5.5): *"signals should be probability surfaces in price and time"* — but here
two entries fire **identically** whether a feature **barely clears** its band (state at the band edge) or
**sits deep** inside it (state at the band centre), and whether a bank clears its k-of-n threshold by the
minimum or with **every** clause satisfied. The band edge is therefore a hard cliff — an **overfitting
surface of its own** — and the ordinal conviction the quantiser computed carries **no** information into
sizing.

## 2. The ordinal `QState` available

`QState::index()` is the bucket ordinal in `0..num_states`. A clause reads it (`Clause::satisfied`):

```
Some(Some(state)) => { let s = state.index(); self.lo <= s && s <= self.hi }
```

So at decision time each active clause knows **exactly where** `s` sits inside `[lo, hi]`:
- `s == lo` or `s == hi` → **band edge** (barely in band);
- `lo < s < hi`, nearer the centre → **deep in band**;
- `lo == hi` (point band) → the only satisfying state is on-target.

This ordinal is a **deterministic, point-wise, leakage-safe** function of the feature vector (QE-107: no
rolling quantiles, no dataset fit) — exactly the raw material for a graded conviction.

## 3. The graded mapping (deterministic, exact)

We add a **pure, additive** grading path in `crates/signal/src/genome.rs`. It changes **no** genome gene,
**no** serialized representation, and leaves `Genome::decide` **byte-for-byte unchanged** (`Decision` and the
directional logic are untouched). Grading lives in **new** methods:

### 3.1 Per-clause "distance into band" (exact integers)

For an **active** clause with band `[lo, hi]` and (when satisfied) state `s`:

- `cap   = 1 + (hi - lo) / 2`  — the maximum conviction weight this clause can carry (integer floor;
  a point band `lo==hi` has `cap = 1`).
- `contrib = 0` if the clause is **unsatisfied**; otherwise `1 + min(s - lo, hi - s)` — `1` at either band
  **edge**, rising to `cap` at the band **centre**.

`contrib`/`cap` are non-negative integers computed with `u16` arithmetic (`s ∈ [lo, hi]` ⇒ no underflow).

### 3.2 Bank conviction (exact rational)

`RuleSet::graded_conviction(features) = Σ contrib_c / Σ cap_c` over **active** clauses (`0` when the bank
has no active clauses). Because a **firing** bank has at least `threshold ≥ 1` satisfied clauses each
contributing `≥ 1`, its conviction is **strictly positive**; a bank satisfied on **every** clause at the
**centre** reaches `1`. This single quantity carries **both** spec-offered readings of ordinal strength:
**count of satisfied clauses** (more satisfied ⇒ larger numerator) **and** **distance into band** (a centre
state contributes more than an edge state).

### 3.3 Entry strength (exact `Decimal`, bounded)

`Genome::entry_strength(features, dir) = FLOOR + (1 − FLOOR) · conviction`, with
`FLOOR = 0.5` (`GRADED_STRENGTH_FLOOR`). This maps a firing bank's conviction into `[0.5, 1]`: a
barely-clearing entry sizes at ~½, a deep-in-band entry sizes at full. All operands are exact `Decimal`
(small-integer numerator/denominator, `FLOOR = 0.5`), so the value is a **deterministic** function of the
ordinal `QState`s — no RNG, no clock, no machine-dependent float (see §5).

### 3.4 Feeding sizing

`crates/wfo/src/backtest.rs` gains a config flag `BacktestConfig.graded` (**default `false`** — the pre-QE-442
path is byte-identical). When `true`, the entry notional is scaled by the entry strength:

```
notional = size_frac · entry_strength · equity_prev      (graded)
notional = size_frac · equity_prev                        (classic, entry_strength ≡ 1)
```

The strength is computed from the **decision bar's** features (the same bar `decide` reads), so there is
**no look-ahead**. Grading is enabled on the **training / selection / reporting** configs (`train.rs`
`train_cfg`, and the reporting `backtest.rs` config) so the metrics a genome is **selected** and **reported**
on price its graded conviction. The trade **sequence** is unchanged (grading touches size, not the
direction), so net-of-cost / trade-count invariants hold.

## 4. Why this is scoped as it is (live sizing is a documented follow-up)

The live netter (`crates/hedger/src/live_netter.rs`) sizes a deployed leg as `weight × size_bps / 10_000`
off the shared `PositionState`. Carrying graded conviction into **live** sizing would require threading an
entry-time conviction through `PositionState` + the evaluator + the netter under the order-path
`deny(panic)` firewall — the **same** train↔live *money-model* parity surface QE-435 owns and deliberately
pins with an oracle rather than a refactor. QE-442's mandate is the **signal combination** being graded and
**carrying conviction into sizing** in the **selection** fitness (where band-edge overfitting is selected
against). Live deployment sizing parity is left to QE-435; `Genome::entry_strength` is exposed on the shared
`signal` crate so QE-435 can consume the **same** function with zero new cross-crate coupling.

## 5. Parity & determinism argument

- **`Genome::decide` stays a pure function of `(genome, features, position)`.** It is **literally
  unchanged** — `Decision` keeps its shape and directional logic. Grading is additive (`entry_strength`),
  itself a pure function of `(genome, features, dir)`: no RNG, no clock, no hidden state. So the QE-001
  decoupling (one strategy-logic path shared by search and live) is preserved, and batch/streaming
  identity is trivially retained.
- **Batch == streaming, byte-identical, over the graded path.** `entry_strength` depends only on the
  `FeatureVector`, which QE-107's point-wise FIR already proves is byte-identical batch vs streaming. The
  parity test is **extended** to assert `decide` **and** `entry_strength` agree bar-for-bar between a
  batch-reconstructed and a streaming-reconstructed feature series (test:
  `graded_entry_strength_is_batch_streaming_identical`).
- **Leakage-safe.** Grading reads only the current bar's ordinal `QState`s — no rolling quantile, no
  dataset-wide fit, no look-ahead (the strength is computed from the decision bar, applied at the next-bar
  fill exactly as the boolean decision is).
- **Determinism / exact money.** Conviction is an exact integer rational; `entry_strength` is exact
  `Decimal` (`FLOOR = 0.5`, integer/integer conviction). The only rounding is `rust_decimal`'s
  platform-independent 28-digit division — deterministic, never machine-dependent float. A determinism test
  pins exact expected `Decimal` values for known bands (`entry_strength_exact_decimal_values`).
- **Backward-compatible representation.** No genome gene changes and no serialized field is added, so
  `REP_VERSION` is unchanged and **every existing genome decodes identically** (stronger than an additive
  serde field). The vintage **schema** is structurally unchanged (grading is a pipeline config, like
  `min_trades`/`windows`, not a sealed per-vintage field), so `VINTAGE_FORMAT_VERSION` need not bump; only
  the vintage **content** moves (decisions→same, sizes→graded→metrics/equity move). `qe-wfo`'s genome
  re-export is untouched.

## 6. Golden movement (expected, direction sanity)

Enabling grading on the selection/reporting configs changes **sizes**, so **trades stay the same but
metrics/equity/vintage move** — the search now prefers genomes whose entries are deep-in-band, and reports
their graded-sized performance. Direction sanity (asserted in tests): a genome that **barely clears** a band
sizes **strictly smaller** (≈½) than one **deep in-band** (→ full); grading modulates size **smoothly** in
`[0.5, 1]` rather than the hard boolean. `content_hash` before→after is recorded in the PR. The
`backtest_job` golden uses a degenerate span-1 (`[3,4]`/`[0,1]`) fixture genome whose conviction is always
`1`, so that specific golden is unaffected; the `train_job` search (random bands) moves.

### Measured before→after (fixture store, seed 42, `run_train_job`)

| path | pre-QE-442 (`graded: false`) | QE-442 (`graded: true`) |
| --- | --- | --- |
| committed `cli/tests/fixtures/golden_result.json` | *(unchanged — regenerated via real code, byte-identical: degenerate full-conviction fixture genome)* | same |
| sealed vintage `content_hash` (train pipeline) | `65f009d4e6d91f4657e79f962ea21cfbbe0173de29d17badd281e8df3bd2306e` | `fca21d3689af9169190bd9210cfe2fe9897257c71565f0e1d7edb8ae250f5f55` |
| sealed vintage id (seed-derived) | `9fba…f5b89` | `9fba…f5b89` (**unchanged** — id is lineage/seed-derived, not content) |

What moved: **decisions/trades stayed the same**; graded conviction rescaled entry **sizes**, so the
combined net returns, ensemble weights, robustness/DSR stats and equity moved → the sealed **content_hash**
moved. The vintage **id** and **format version** did not (representation and schema are structurally
unchanged). The committed `golden_result.json` did not move (its fixture genome is full-conviction), and was
re-run through the real `regenerate_fixtures` path to confirm byte-identity — no hand-edit.
