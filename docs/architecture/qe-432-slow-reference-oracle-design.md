# QE-432 — Independent slow-reference oracle for the reconstruct roll-up & net-of-cost fitness

`Phase: Review R2 (P1)` · `Area: wfo reconstruct / backtest / friction` · `Spec of record:`
[`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-432`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: Review R2.a · `Spec ref: maxdama §5.5 (Dubno: "make a backtester that is slow but works,
then verify the optimised version matches it exactly").`

## 1. Problem / why (evidence)

Every current "parity" guarantee in the engine is **same-code-vs-same-code**:

- The determinism harness (`crates/determinism/src/harness.rs`) re-runs **one** closure and asserts
  byte-identical artefacts — that proves *reproducibility*, not *correctness*.
- Batch/streaming parity (`crates/signal/src/reconstruct.rs`, test `batch_equals_streaming_parity`)
  drives the **same** `BarReconstructor` fold in both modes — a tautology by construction (the module
  doc literally says "batch is literally streaming fed the whole slice … byte-identical by
  construction").

So a shared logic bug in the roll-up or in the net-of-cost fitness would mis-rank every genome
**identically**, reproduce byte-for-byte, and be baked into every vintage — garbage-in that no
downstream DSR/PBO/SPA statistic can catch. Dubno §5.5's principle (a slow-but-obviously-correct
reference verified against the optimised path) is the one the repo has **not** yet honoured. This
ticket adds that oracle. It is **test/dev-only** — no production or hot-path change.

### Current-state code under oracle

**(a) Multi-resolution bar roll-up** — `crates/signal/src/reconstruct.rs`.
`reconstruct_batch(base_bars, base, target)` rolls base bars up into a coarser resolution via an
**incremental fold** (`BarReconstructor` + `Window::open_from`/`fold`/`finish`): epoch-aligned windows
(`window_start = millis.div_euclid(target_ms) * target_ms`), `open` = first bar folded into the
window, `close` = last, `high` = running max, `low` = running min, `volume`/`trades` = running sums.
A completed coarser bar is emitted when an incoming bar crosses into a new target window; the last
in-progress window is flushed by `finish`.

**(b) Net-of-cost geometric fitness** — `crates/wfo/src/backtest.rs` + `crates/wfo/src/friction.rs`
+ `crates/wfo/src/fitness.rs`.
`backtest(genome, bars, cfg)` walks the bar series with a single interleaved loop and a `Pending`
enum: bar `i`'s `Genome::decide` schedules an order that **fills at bar `i+1`** (no look-ahead).
`apply_fill` moves an exact `Decimal` cash/mark ledger — `cash -= signed_qty·price`, then
`cash -= (fee + slip)` where `fee = notional_abs · taker_rate · cost_multiplier` and
`slip = notional_abs·(half_spread + impact·qty_abs) · cost_multiplier`. Funding accrues as
`cash += −pos_qty·price·rate`. Each bar marks `equity = cash + pos_qty·price` and (from bar 1) pushes
the net-of-cost per-bar return `f64((equity/equity_prev) − 1)`. The return series is split into
`cfg.windows` contiguous sub-windows (`split_windows`) and summarised by
`NoiseRobustFitness::from_windows`, whose per-window statistic is
`log_growth = mean_i ln(1 + r_i)` (with `≤ −100%` ⇒ `−∞`, absorbing ruin). `net_pnl = equity_final − 1`.

## 2. Design of the reference oracles (independent, naive)

Both references are written **from scratch** in the test file — they do **not** call the fold /
`apply_fill` / `split_windows` / `NoiseRobustFitness` / `log_growth` code they check. Independence is
structural: a plain re-derivation of the same *contract*, not a re-use of the same *code*.

### (a) `reference_rollup` — fresh O(n) window scan

For the set of epoch-aligned window starts present in the input (ascending), **rescan the whole base
slice once per window** and aggregate the members whose
`open_time.millis().div_euclid(target_ms) * target_ms` equals that start:
`open = members.first().open()`, `close = members.last().close()`, `high = max(highs)`,
`low = min(lows)`, `volume = Σ volume`, `trades = Σ trades`. This is deliberately `O(n · windows)` —
the naive nested scan, versus the optimised single-pass incremental fold.

*Domain covered.* Inputs are **strictly-ascending** M5 base bars with random gaps (missing bars /
skipped windows). Ascending order is the documented expectation and exactly the space the pipeline
roams; with ascending input each window is contiguous, so grouping-by-window-start and the
single-current-window fold agree. (Out-of-order input can split a window in the optimised fold; that
is out of the roamed space and out of scope — noted, not tested.) Targets are drawn from every valid
coarser multiple of M5: M15, M30, H1, H4, H12, D1.

### (b) `reference_backtest` — dead-simple trade-by-trade loop

An independent replay of the ledger and fitness. It **reuses `Genome::decide`** — the shared
signal-layer decision primitive is *not* part of the cost path under test (it is the same source of
truth training and live already share); the oracle targets the **cost-ledger + net-of-cost fitness**
roll-up, which it recomputes independently:

- next-bar fill scheduling (`decide` at `i` → fill at `i+1`), `notional = size_frac · equity_prev`,
  `qty = notional / price`;
- exact `Decimal` ledger: `cash -= signed_qty·price`; `cash -= fee + slip` with the fee/slip formulas
  re-derived from `FrictionConfig`; funding `cash += −pos·price·rate`;
- per-bar `equity = cash + pos·price`, net return `f64((equity/equity_prev) − 1)` from bar 1;
- `net_pnl = equity_final − 1`; independent window split + independent `mean_i ln(1+r_i)` per window,
  averaged → fitness mean.

### Equivalence assertions & tolerance

- **Exact (`==`)** for the `Decimal` ledger outputs (`net_pnl`, realised `funding`) and the integer
  `trades` — exact-decimal money must be byte-identical.
- **Exact** for the reconstructed `Bar` vectors in (a) (all fields are exact `Decimal`/int).
- **f64 tolerance `1e-9`** for the per-bar `returns` vector and the fitness `mean`. Rationale: the
  reference sums logs / marks equity in an independently-ordered float computation; IEEE-754
  non-associativity permits last-ULP differences. `1e-9` is far tighter than any fitness gap the
  search resolves (elite replacement works in units of standard error, typically `1e-2`–`1e-4`) yet
  loose enough to absorb reordering noise. In practice the observed difference is `0.0` for most
  cases; the tolerance is the documented safety margin. Ruin (`−∞`) is compared as an exact category
  (both `−∞` or both finite).

## 3. Mutation guard (proves the oracle is non-vacuous)

For **each** path the test defines a local `mutant_*` reimplementation identical to the optimised
logic **except for one injected bug**, and asserts on a random corpus that the reference **agrees with
the real optimised path** but **disagrees with the mutant** (on at least one case). This is the
faithful no-touch-production form of "a deliberately injected bug in the optimised path is caught by
the reference": were the bug in the real path, the property test would fail.

- Roll-up mutant: `volume = max` instead of `Σ volume` (a plausible fold typo).
- Fitness mutant: drop the `cost_multiplier` on slippage (a plausible net-of-cost regression — exactly
  the systematic per-trade cost bias the Deflated Sharpe cannot remove).

## 4. Test plan

`crates/wfo/tests/qe432_slow_reference_oracle.rs` (new integration test; **no** manifest change — all
of `qe-signal`, `qe-determinism`, `qe-domain`, `rust_decimal`, `rand_core` are already `qe-wfo`
`[dependencies]`, so no firewall edge and no new crate enters the workspace):

1. `reconstruct_oracle_matches_over_seeded_random_cases` — `CASES` seeded random ascending M5 corpora
   × random target; assert `reconstruct_batch == reference_rollup`.
2. `net_of_cost_fitness_oracle_matches_over_seeded_random_cases` — `CASES` seeded random genome + bar
   series + friction config; assert `backtest` ≡ `reference_backtest` (Decimal exact, f64 within
   tol).
3. `reconstruct_oracle_is_non_vacuous_mutation_guard` — reference == real, reference ≠ volume-mutant.
4. `net_of_cost_oracle_is_non_vacuous_mutation_guard` — reference == real, reference ≠ cost-mult mutant.
5. Determinism: seeds derived via `qe_determinism::{seed_rng, derive_seed}`; the whole test is a pure
   function of the fixed master seed, so it is byte-reproducible and re-run-stable.

`CASES = 256` per property (≥ 512 randomised equivalence cases per run across the two paths, plus the
mutation-guard corpora). Documented here so the count is auditable.

## 5. Risks & scope guards

- **No production behaviour change** — test-only; no `src/` edit, no golden regenerated, no
  `content_hash` touched. If any golden moved, that would be a red flag to investigate (none should).
- **Firewall** — no new dependency; `search ⟂ portfolio ⟂ live` untouched.
- **Determinism** — fixed master seed + `qe-determinism` derivation → the suite stays byte-stable and
  the determinism harness is unaffected.
- **Out of scope** — a second live edge/venue execution path; replacing the determinism harness or the
  batch/streaming parity test (this **complements** them).

## 6. Acceptance criteria trace

- Property tests assert equivalence (Decimal byte-exact; f64 documented `1e-9` tol) of optimised vs
  reference for **both** the reconstruct roll-up and the net-of-cost fitness, over ≥ 512 seeded
  randomised cases/run → tests 1, 2.
- Mutation guard catches a deliberately injected bug on each path → tests 3, 4.
- Determinism harness + full green gate (fmt / clippy `-D warnings` ×2 / test / deny / firewall) pass
  on the exact commit; no golden moves.
