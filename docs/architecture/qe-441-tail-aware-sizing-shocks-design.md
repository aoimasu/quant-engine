# QE-441 — Bar-level tail-aware scenario shocks in the single-strategy sizing fitness

`Phase: Review R2 (P2 — panel #12, majority)` · `Area: wfo / ensemble / determinism` ·
`Depends on: QE-130, QE-120, QE-006` · Spec of record:
[`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-441`](../reviews/2026-07-16-maxdama-panel-review.md)

## 1. Problem (current state, with evidence)

The single-strategy fitness that sets `size_bps` sees only the raw historical net path:

- `crates/wfo/src/backtest.rs` — `size_frac = size_bps / 10_000` scales the notional at fill
  time (`backtest_with_trades`, `Pending::Enter`: `notional = size_frac * equity_prev`). The
  per-bar net return is `equity/equity_prev − 1` where `equity = cash + pos_qty·price`.
- `crates/wfo/src/fitness.rs` — `log_growth` is `mean ln(1+r)`; a single `r ≤ −1` ⇒ `−∞`
  (ruin is absorbing).
- `crates/cli/src/jobs/train.rs:348` — the MAP-Elites / DE **selection fitness** is
  `eval = |g| fold_isolation_fitness(g, train_bars, &cv_ranges, &train_cfg)`. This is the
  fitness that decides which genome (and therefore which `size_bps`) wins each niche.

Because leverage is fit to crypto's empirically thin crash sample, the search self-selects a
higher leverage than a tail-aware Kelly would. The ensemble stress overlay
(`crates/ensemble/src/stress.rs`: `Gap` / `FundingSpike` / `Adl` synthetic shocks) exists but
runs **after** selection, so it never reaches the size-setting fitness.

**Why not append shocks to the post-size return series?** (the naive reading) — a flat additive
loss on the already-computed `returns` is size-independent: it only trips the `−inf` ruin
absorber and rejects every genome uniformly. It cannot make a *larger size* produce a *larger*
drawdown, so it cannot pull the optimum down. The fix must be **bar-level**, applied to the
held notional so the loss scales with `pos_qty` (= size).

## 2. Design

### 2.1 Mechanism — mark shock on the held position (scales with size)

At each bar `i`, deterministically decide (seeded RNG) whether a synthetic shock fires and of
what shape. If a position is held (`pos_qty ≠ 0`), apply an adverse fraction `e` of the held
**notional** as a cash loss **before** the bar is marked:

```
shock_loss = |pos_qty · price| · e      // e = adverse fraction of notional (Decimal, exact)
cash      -= shock_loss                 // enters equity ⇒ enters the per-bar net return ⇒ log_growth
```

`|pos_qty·price| = size_frac · equity_prev`, so the loss is **linear in `size_frac`**: a bigger
`size_bps` takes a proportionally bigger hit, deepening the drawdown and, past a leverage
threshold, driving the bar return `≤ −1` → ruin → `−∞`. This is algebraically identical to an
adverse **price move** of fraction `e` on the held position (a bar/price-level mark shock),
which is exactly the exposure-scaled shape `stress.rs` uses (`compound(d0, e·gross)`), and it is
applied **before** `size_frac` propagates into the notional. Dama §6.1 (imaginary Black-Swan
PnL before optimizing f) / §6.4 (a heavy left tail pulls Kelly down), done correctly.

The three shapes mirror `stress.rs` magnitudes: gap `0.10`, funding-spike `0.005 × 8`, ADL
`0.05`. A `u64` roll `% 3` picks the shape; the adverse fraction is a pure integer→`Decimal`
map (no float money).

### 2.2 Seeded / reproducible (SSE mitigation)

Shocks are drawn from the **portable** ChaCha8 RNG (`qe_determinism::seed_rng`). One `DetRng` is
seeded **inside each backtest call** from the frozen `ShockConfig::seed`, and drawn in bar order.
`backtest` therefore stays a pure function of `(genome, bars, cfg)` — byte-identical regardless
of thread count, and identical across repeated calls. Two fixed draws are consumed **every** bar
(fire? + shape), unconditionally, so the shock schedule is a pure function of `(seed, bar count)`
and is **position-independent**: all genomes on the same window hit the same shock bars, only the
per-bar loss differs by their size. A determinism test asserts same seed → identical shocked
fitness; different seed → different.

### 2.3 Frozen / pre-registered / content-addressed (Math#2 mitigation)

The shock severity/frequency are un-deflated researcher degrees-of-freedom. They must be
**frozen and content-addressed**, not a per-run knob:

- `ShockConfig` is a new **content-addressed** type in `qe-risk` (the shared leaf that already
  hosts the content-addressed `SlippageCalibration` (QE-431) and `PortfolioSizer` (QE-433)).
  It carries `content_hash()` (canonical-JSON SHA-256, same pattern as its siblings).
- The frozen default (`ShockConfig::default()`) is what the sizing fitness uses; its `seed` is a
  **fixed pre-registered constant**, **not** derived from the run seed — so it cannot be tuned
  per run to flatter results.
- It is **sealed into the vintage**: `VintageContent` gains a `shocks: ShockConfig` field, so the
  exact shock set that shaped `size_bps` rides the vintage's reproducible lineage/hash, exactly
  like `slippage`/`sizer`. This adds a hashed field ⇒ **`VINTAGE_FORMAT_VERSION` bumps 6 → 7**.

### 2.4 Firewall (search ⟂ portfolio)

`backtest.rs` in `qe-wfo` must **not** import the shock *shapes* from `qe-ensemble`
(`qe-wfo → qe-ensemble` is a forbidden edge; `crates/architecture/tests/firewall.rs`). So:

- the frozen **parameters** (`ShockConfig`) live in the shared leaf **`qe-risk`** (both `qe-wfo`
  and `qe-vintage` already depend on it; `qe-risk` reaches only `qe-domain`/`qe-error`);
- the shock **generator/application** (per-bar RNG draw + notional perturbation) is **`qe-wfo`
  local** (`backtest.rs`), consuming `qe_risk::ShockConfig` + `qe_determinism::DetRng`.

This **replicates** the `stress.rs` shapes (documented as a deliberate duplication) rather than
importing `qe-ensemble` — no new dependency edge, firewall stays green.

### 2.5 Scope of injection

Shocks are enabled **only** on the selection fitness (`train.rs:348` `eval`, via a `search_cfg`
with `shocks: Some(ShockConfig::default())`). All downstream backtests — elite pool / ensemble /
DSR trial columns, the G1 holdout, the QE-433 Kelly-sizer input, and the vintage **replay /
reporting** job — stay on the honest historical path (`shocks: None`, the `BacktestConfig`
default). This keeps `backtest`'s reporting golden meaningful (it replays history, it does not
inject shocks) and confines the change to the size-setting fitness the ticket targets.

## 3. Test plan (TDD)

1. `qe-risk`: `ShockConfig::content_hash` is stable and **changes** when any field changes
   (content-addressed); defaults match the `stress.rs` magnitudes.
2. `qe-wfo` backtest: a **larger `size_bps`** produces a **strictly deeper shocked drawdown**
   (same window + seed) — the mechanism scales with size.
3. `qe-wfo` backtest: the **fitness-maximising `size_bps`** over a sweep is **strictly lower with
   shocks than without** (tail-aware Kelly pulls leverage down).
4. `qe-wfo` backtest: **monotone in severity** — a **heavier** shock set pulls the optimum
   **at-or-below** a milder one, and both below the no-shock optimum.
5. `qe-wfo` backtest: **reproducible** — same seed ⇒ identical shocked `returns`/fitness; a
   different `ShockConfig::seed` ⇒ different; and `shocks: None` reproduces the pre-QE-441 path
   byte-for-byte.
6. `qe-vintage`: a `shocks` change moves the `content_hash`; `format_version == 7`.

## 4. Golden / determinism impact

- `crates/cli/tests/fixtures/sample_vintage.json` and `golden_result.json` embed
  `content_hash` + `format_version`, so both move (new hashed field + version bump). Regenerated
  **only** via the real `regenerate_fixtures` path
  (`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`), never
  hand-edited.
- `train_job.rs` asserts same-seed determinism (still holds — pure function of seed + frozen
  config) and a fixed number of G1 criteria; no committed train-vintage hash to update.
- `crates/determinism/tests/determinism.rs` golden pins the **RNG stream** only (unchanged).

## 5. Risks

- **Over-severe defaults reject every genome uniformly** (the failure mode the spec warns of):
  mitigated by a modest frozen default frequency/magnitude and by scoping shocks to the tiny
  fixture search only via the selection fitness; verified by the train_job integration test
  still producing elites + a non-empty ensemble.
- **Content-hash drift** from non-idempotent `Decimal`: mitigated by quantize+normalize (same
  `_SCALE` discipline as `sizer.rs` / `slippage.rs`).
