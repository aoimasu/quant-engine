# QE-415 — Wire the purged/embargoed CV into selection fitness

**Ticket:** QE-415 (P2 leakage control). Authoritative spec: `docs/reviews/2026-07-15-team-improvement-review.md`, section `### QE-415`.

**Goal.** `PurgedKFold` (`crates/wfo/src/cv.rs`) and `WalkForward` (`crates/wfo/src/walkforward.rs`) are correct and
tested but **unused in selection**. Fold purged out-of-sample cross-validation into the *fitness path* that
MAP-Elites and the DE score on, so an elite's recorded fitness reflects **purged OOS validation** rather than a
single in-sample geometric-growth number.

---

## 1. Current state (file:line)

### 1.1 The in-sample fitness path

`crates/cli/src/jobs/train.rs`:

- **`:263-267`** — the small-budget backtest config used for the whole search (`min_trades: 1`, `windows: 2`).
- **`:276`** — the selection fitness closure:
  ```rust
  let eval = |g: &Genome| backtest(g, train_bars, &train_cfg).elite_fitness();
  ```
  `backtest(...).elite_fitness()` = `fitness.mean` = the mean per-window `log_growth` over **contiguous** sub-windows
  of the **entire** `train_bars` series (one continuous backtest, returns sliced by `split_windows`).
- **`:279-299`** — the MAP-Elites search loop: `long.step` / `short.step` call `eval` on each offspring; the archive
  stores the scalar fitness and replaces cell elites by that scalar.
- **`:307-310`, `:340-360`, `:521-536`, `:565-580`** — the *pool / ensemble / DSR-variance / funding* stages, which
  legitimately need **continuous whole-window** return series (CSCV/SPA/DSR columns). **Unchanged by this ticket.**

### 1.2 The "noise-robust windows" are contiguous slices (not OOS folds)

`crates/wfo/src/backtest.rs:118-137` — `split_windows(returns, k)` splits the continuous net-return series into `k`
**adjacent** contiguous chunks. `NoiseRobustFitness::from_windows` (`crates/wfo/src/fitness.rs:63-95`) then reduces
them to `mean ± SE` of per-window `log_growth`. Because the chunks come from **one continuous backtest**, a position
opened in chunk *k−1* carries into chunk *k* (cross-window position/indicator carry) — they are not isolated OOS folds.

### 1.3 The leakage-free CV objects that were unused

- `crates/wfo/src/cv.rs` — `PurgedKFold { n_folds, lookback, label_horizon, embargo }`. `folds(n_bars)` yields `k`
  balanced contiguous **test** blocks that partition `0..n_bars`, each with a purge+embargo-excluded **train** set.
  `Fold::windows_disjoint(lookback, label_horizon)` proves every `(train, test)` pair has
  `|tr − te| > lookback + label_horizon`.
- `crates/wfo/src/walkforward.rs` — `WalkForward` / `Window::windows_disjoint` (same invariant, anchored/rolling).

### 1.4 Golden fixtures — what actually depends on selection fitness

- `crates/cli/tests/fixtures/sample_vintage.json` (and the identical `crates/server/tests/fixtures/…`) and
  `crates/cli/tests/fixtures/golden_result.json` are produced by `regenerate_fixtures`
  (`crates/cli/tests/backtest_job.rs:260`). `write_sample_vintage` seals a **hand-built, fixed** genome
  (`fixture_genome()`) — **not** search output — and `golden_result.json` is the backtest of that fixed vintage.
  **Neither depends on the selection fitness function.** They depend only on `Vintage::seal` and `run_backtest`
  (the core `backtest()` engine), which this ticket does **not** touch.
- `crates/cli/tests/train_job.rs` runs the *real* train job but asserts **determinism** (same seed ⇒ same
  id+hash) and **structural** properties (coverage > 0, folds emitted, G1 has 5 criteria, QE-414 variance-trial
  breadth). It does **not** pin the sealed vintage hash to a committed golden constant.

**Consequence:** because the change is confined to the *selection* eval and does **not** modify `backtest()` /
`split_windows` / `Vintage::seal` / `run_backtest`, the committed goldens (`sample_vintage.json`,
`golden_result.json`) remain **byte-identical** and require **no regeneration**. See §5.

---

## 2. New purged-OOS fitness design

New module `crates/wfo/src/cv_fitness.rs`:

```rust
pub const DEFAULT_CV_FOLDS: usize = 4;      // mirrors DEFAULT_WINDOWS; ≥2 gives a real SE
pub const DEFAULT_LABEL_HORIZON: usize = 1; // QE-120 realises a decision's P&L from the next bar

// Fold geometry the selection uses (purge+embargo = the QE-113/D5 documented default embargo = lookback).
pub fn selection_kfold(n_folds, lookback, label_horizon) -> PurgedKFold
    = PurgedKFold::with_default_embargo(n_folds.max(2), lookback, label_horizon);

// Precompute the K OOS test ranges once (fold construction is genome-independent — no per-genome realloc).
pub fn oos_test_ranges(cv: &PurgedKFold, n_bars) -> Vec<Range<usize>>;

// Purged OOS fitness: score the genome on each disjoint test fold IN ISOLATION (flat start), then
// reduce the per-fold return series to mean ± SE exactly as the noise-robust evaluator does.
pub fn purged_oos_fitness(genome, bars, test_ranges: &[Range<usize>], cfg) -> NoiseRobustFitness {
    let windows = test_ranges.iter()
        .map(|r| backtest(genome, &bars[r.clone()], cfg).returns) // isolated OOS segment
        .collect::<Vec<_>>();
    NoiseRobustFitness::from_windows(&windows)
}
```

**Selection scalar** = `purged_oos_fitness(...).mean` (same units — mean per-fold `log_growth` — as the old
`elite_fitness()`, so the archive's scalar comparison and all downstream fitness plumbing are unchanged in type/scale).

### Why this is "purged OOS", and how folds map to `windows_disjoint`

The genome is a **fixed rule-set**; there is no per-fold *fitting*, so OOS-ness here is not "train on fold-train,
test on fold-test". It is achieved by two properties:

1. **Per-fold isolation.** Each fold's test block is backtested **independently, flat-start**. This removes the
   cross-window **position/indicator carry** that the old contiguous `split_windows` leaked (a position opened in
   window *k−1* no longer bleeds P&L into window *k*). A genome that only looked good by riding a single contiguous
   stretch, or by a lucky cross-boundary carry, is now scored on each fold on its own merits.
2. **Leakage-free fold geometry (AC-b).** The folds are `PurgedKFold` folds built with the **real** feature lookback
   (`schema.max_lookback()`) and `DEFAULT_LABEL_HORIZON`, with the documented default embargo (= lookback). Every
   fold therefore satisfies `Fold::windows_disjoint(lookback, label_horizon)` — the QE-113 invariant that train and
   test information windows are disjoint *including the lookback*. The selection consumes the `.test` blocks of these
   exact folds.

Aggregating per-fold `log_growth` with the **SE penalty** (`NoiseRobustFitness`) means a genome whose edge is
concentrated in one fold (high dispersion across folds, or zero growth in most folds) scores **below** a genome that
generalises across all K folds — which is exactly AC-(a).

**Methodology note for the reviewer (sanity-check):** with `PurgedKFold` the K test blocks *partition* the window, so
the purge/embargo parameters shape the (unused-here) *train* partition and define the disjointness invariant we verify;
they do **not** re-slice the OOS test blocks. We deliberately consume test blocks only, because with the real catalogue
(`max_lookback = 34`) over the fixture's ~87 train bars the purge zone (35) nearly empties the fold *train* sets — a
train-based OOS scheme would be degenerate at this budget. The OOS discrimination therefore comes from per-fold
isolation + SE-penalised aggregation over leakage-free fold geometry, not from re-gapping the test blocks. This is the
conservative, non-fragile choice and is what AC-(a)/(b) test.

### Config knob + default

`cv_folds` is **config-driven** via `SelectionConfig` (same section as QE-403's `funding_coverage_min`):

- `crates/config/src/schema.rs` — `SelectionConfig.cv_folds: usize`, `#[serde(default = "default_cv_folds")]`,
  default **4**.
- `crates/config/src/lib.rs` — validated `cv_folds >= 2` (needs ≥2 folds for a real cross-validated SE).
- `crates/cli/src/jobs/train.rs` — `TrainParams.cv_folds`; the fixture test config (`train_job.rs`) sets **2** to
  keep `cargo test` fast.

---

## 3. Determinism argument

The new fitness is a pure, deterministic function of `(genome, bars, cfg, fold geometry)`:

- Fold boundaries are fixed by `(n_folds, n_bars)` — `oos_test_ranges` is computed **once** before the search,
  genome-independent.
- Per-fold `backtest(...).returns` is already a pure function (proved by `backtest_is_pure_and_has_no_same_bar_fill`).
- Reduction order is fixed (fold index `0..K`), and `NoiseRobustFitness::from_windows` is order-deterministic.
- The `eval` closure **touches no RNG**; `VariationDriver::step`'s RNG stream is unchanged, so the search trajectory
  is a deterministic function of the seed.

Proven by **`train_is_deterministic_for_a_fixed_seed`** (`crates/cli/tests/train_job.rs`): two runs at the same seed
produce a byte-identical sealed vintage (same id **and** content hash) under the new fitness; a different seed differs.

---

## 4. Performance impact

Per-genome selection cost is **essentially unchanged**. `PurgedKFold` test blocks *partition* the train window, so the
sum of test-block lengths = the train-window length: the K isolated backtests do the **same total bar-work** as the one
old whole-window backtest (Σ|test_k| = n), plus negligible per-fold setup. Total selection eval remains ~one pass over
the train bars.

- Fixture configs use `cv_folds = 2` → 2 short backtests per genome; the train integration tests stay sub-second.
- Real runs default to `cv_folds = 4` → still ~one pass over the train window per genome (no multiplicative blow-up).
- The pool/ensemble/DSR/funding stages are unchanged (still whole-window backtests).

No test-suite runtime balloon; no real-run slowdown beyond negligible per-fold setup.

---

## 5. Golden-regeneration procedure

**No committed golden required regeneration** (see §1.4): `sample_vintage.json` / `golden_result.json` derive from a
fixed hand-built genome through `Vintage::seal` / `run_backtest`, both untouched; no test pins the *train* output hash.

Verification performed (all must stay green, no fixture edits):
- `backtest_over_fixture_store_matches_golden` (cli) — golden backtest unchanged.
- `Vintage::load` schema-assert + hash verify on the committed fixtures (server `read`/`runs`, cli `train`/`backtest`).
- `train_is_deterministic_for_a_fixed_seed` (cli) — same seed ⇒ same NEW sealed hash byte-for-bit.

Had a train-output golden existed, the reproducible procedure would be the existing
`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact` (the only sanctioned regenerator),
run through the real seal/backtest code, then eyeballed and committed. It **was run** to confirm this ticket leaves
the goldens unchanged: after the run, `crates/cli/tests/fixtures/sample_vintage.json` and
`crates/cli/tests/fixtures/golden_result.json` were **byte-identical** (`git status` clean for both JSON goldens).
The regenerator also rewrites the LMDB `sample_store/` (whose on-disk page layout is not byte-deterministic across
rewrites and is unrelated to this ticket); those store files were reverted (`git checkout`), and
`backtest_over_fixture_store_matches_golden` re-passes against the committed store.

---

## 6. Risks & rollback

- **Risk:** the new fitness changes which genomes are selected → different sealed vintage content/hash for *real*
  runs. Mitigated: no golden pins that hash; determinism preserved; goldens byte-identical.
- **Risk:** degenerate folds at tiny budgets (empty fold-train when `max_lookback` ≈ train length). Mitigated: the
  fitness consumes **test** blocks (always a valid partition); `n_folds` is floored at 2; the AC-(b) test uses a
  non-degenerate configuration to prove the invariant non-vacuously.
- **Firewall:** all new logic lives in `qe-wfo` (dep `qe-signal`, already present) and `qe-cli`/`qe-config`
  (pre-existing edges). No new cross-crate edge.
- **Rollback:** revert the `eval` closure at `train.rs` to `backtest(...).elite_fitness()` and drop the module + the
  `cv_folds` field; fully self-contained.
