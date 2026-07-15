# QE-403 — Enforce net-of-cost truth: funding-coverage gate + non-zero size-impact in selection

`Phase: P1` · `Area: trading / net-of-cost (QE-109)` · Authoritative spec: `docs/reviews/2026-07-15-team-improvement-review.md → ### QE-403`

## Problem (current state, file:line)

1. **Funding accrues only where a stamp lands.** `crates/wfo/src/backtest.rs:224`
   (`if let Some(rate) = bar.funding_rate { cash += -pos_qty * price * rate; }`) — a bar with no
   funding stamp contributes zero funding, silently.
2. **Funding is attached by exact open-time equality, no coverage check.**
   `crates/cli/src/jobs/features.rs:78-115` (`to_decision_bars`) maps `funding_by_ms.get(&ms)` onto each
   bar; a bar with no matching stamp carries `funding_rate = None`. Nothing asserts how many bars got one.
3. **The train job scans funding but never asserts presence/completeness.**
   `crates/cli/src/jobs/train.rs:191` (`let funding = store.scan_funding(...)`) — the scan result is
   fed straight into `to_decision_bars` and never inspected. An empty/sparse funding series means every
   genome is selected, DSR/SPA-assessed, and G1-gated on **funding-free** returns — exactly the
   funding-negative strategies QE-109 exists to reject.
4. **Size-impact defaults to ZERO.** `crates/wfo/src/friction.rs:73`
   (`impact: Decimal::ZERO, // off by default`). Selection cost is `notional · (half_spread + impact·qty)`;
   with `impact = 0` a large-`size_bps`, high-turnover genome pays only a fixed half-spread, so selection
   is blind to size-dependent slippage.

## Design

### 1. Funding-coverage gate (fail-fast, config-driven threshold)

- **Where:** `crates/cli/src/jobs/train.rs`, immediately after `to_decision_bars`, **before** the search —
  so a coverage failure errors rather than sealing (the AC).
- **Coverage metric:** over the span of the **actual decision bars** `[t_first, t_last]` (not the requested
  `[from, to]`, which can exceed the data on hand — the committed fixture has 120 h of bars inside a 9-day
  request window). Funding on Binance USDT-M is every 8 h, so
  `expected = floor((t_last − t_first) / 8h) + 1` and `present = count of decision bars with
  funding_rate.is_some()`. `coverage = present / expected` (clamped to `[0,1]`; `expected ≥ 1` always
  because the train window needs ≥ 2 bars).
- **Threshold:** config-driven via a new `[selection]` section, field `funding_coverage_min` (a fraction in
  `[0,1]`), **default `0.90`**. Plumbed `Config.selection.funding_coverage_min → TrainParams.funding_coverage_min`.
- **Behaviour:** below threshold ⇒ **`RunError::FundingCoverage { present, expected, coverage_pct,
  threshold_pct }`** with message `funding coverage {coverage_pct}% over the training window is below the
  required {threshold_pct}% (present N of expected M 8h stamps): refusing to seal on funding-free returns`.
  A no-funding window yields `coverage 0%` and this error — the AC verbatim.
- **Why fail-fast, not just flag:** the whole point is that selection/validation/G1 must not run on
  funding-free returns. Erroring before the search is the strongest enforcement and keeps determinism
  trivial (no sealed artefact is produced).

### 2. Funding visibility in the sidecar

- Add `funding: Decimal` to `qe_wfo::backtest::BacktestResult`, accumulated from the same
  `-pos_qty·price·rate` ledger term (purely additive; `returns`/`net_pnl`/`fitness` unchanged).
- In `train.rs`, after sealing, backtest the sealed ensemble over the train window and record in
  `TrainResultDoc`:
  - `funding_pnl: f64` — realised funding cashflow of the selected ensemble over the train window
    (weight-summed across chromosomes).
  - `funding_fraction_of_net: f64` — `funding_pnl / net_pnl` (0.0 when `net_pnl == 0`). A funding-free run
    shows `0.0`, making it visible in the artefact QE-261 consumes.

### 3. Non-zero default size-impact in selection friction

- `crates/wfo/src/friction.rs`: `SlippageModel::default().impact` **`Decimal::ZERO` → `Decimal::new(1, 4)`
  = 0.0001** (per unit qty), same order as the 1 bp half-spread. Selection friction is
  `BacktestConfig::default().friction` (used by the train search's `train_cfg`), so the search now pays a
  size-dependent term `notional · impact · qty` (quadratic in position size), penalising large/high-turnover
  genomes. Rationale for the magnitude: a conservative, clearly non-zero coefficient that bites on size
  without dominating a typical fill; the exact value is a policy knob a later ticket can route through the
  QE-128 capacity/impact model.

## Fixtures that move — and the procedure

- **`crates/cli/tests/fixtures/golden_result.json` (QE-251 CLI-backtest golden): kept STABLE.** The CLI
  backtest job (`crates/cli/src/jobs/backtest.rs::backtest_config`) previously inherited slippage via
  `..FrictionConfig::default()`, so a default-impact change *would* have moved it. To avoid regenerating a
  binary LMDB fixture and to keep this ticket scoped to **selection** friction, `backtest_config` now sets
  its slippage **explicitly** (`SlippageModel { impact: Decimal::ZERO, ..default() }`), pinning the
  reporting backtest to its prior numeric behaviour. The golden is therefore unchanged. (Follow-up: route
  the reporting backtest's impact through a CLI flag / the QE-128 model — out of scope here.)
- **Train golden / determinism:** the `train_job.rs` integration test builds `TrainParams` and its
  `Lineage` **directly** (`Lineage::new(...)`, not `Lineage::from_config`), so the new `[selection]` config
  field does **not** change its vintage id. It asserts *determinism* (same seed ⇒ same id+hash) and
  *verifiability*, not a pinned golden id — changing which genomes selection picks does not break it. The
  committed sample store has **full** funding coverage (stamps every 8 h across all 120 bars ⇒ 100%), so it
  clears the 0.90 gate. The test sets `funding_coverage_min` explicitly on `TrainParams`.
- **`crates/determinism/tests/determinism.rs`:** RNG/thread determinism only — no friction — unaffected.
- **`crates/wfo/src/friction.rs` unit test `ac1_turnover_one_shows_fee_drag`:** asserts exact slippage for
  the default model at qty = 1; updated to the new default (`slippage 0.02 → 0.04`, `net −0.12 → −0.14`),
  recomputed by hand from `notional·(half_spread + impact·qty)`, not eyeballed.

## Determinism

- No wall-clock/RNG added. The coverage metric is a pure function of the scanned bars/funding. The impact
  change is a fixed `Decimal` constant. Same inputs + seed ⇒ same selection, same sidecar, same (or, on a
  funding-free window, deterministically absent) sealed vintage.

## Tests added

- **(a)** `crates/cli/tests/train_job.rs`: a training run over a store with **no funding** trips
  `RunError::FundingCoverage` (coverage 0%) and seals nothing.
- **(b)** `crates/wfo/src/backtest.rs`: a high-turnover genome's fitness **strictly drops** when `impact > 0`
  vs `impact == 0`, with identical trade counts (cost-only drag).

## Risks

- The impact magnitude is a judgement call — flagged for reviewer sign-off; it is a default, overridable by
  constructing an explicit `SlippageModel`.
- The reporting backtest deliberately retains `impact = 0` (see fixtures) — an intentional scope boundary,
  documented so a reviewer does not read it as selection/report inconsistency by accident.
