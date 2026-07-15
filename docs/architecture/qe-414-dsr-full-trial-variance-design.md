# QE-414 — Deflated-Sharpe trial variance from the full trial population

**Ticket:** QE-414 (P2, statistical-validation rigor). Spec: `docs/reviews/2026-07-15-team-improvement-review.md` → `### QE-414`.
**Branch:** `qe-414/dsr-full-trial-variance`.

## Problem (current state)

The Deflated Sharpe Ratio (DSR) deflation bar `E[max SR]` scales with the cross-trial Sharpe
**dispersion** `trial_variance` (`crates/validation/src/dsr.rs:54-63` `expected_max_sharpe`,
`:79-85` `trial_sharpe_variance`). The train job estimates that dispersion from the **top-`MAX_POOL = 10`
elites by fitness**:

- `crates/cli/src/jobs/train.rs:302` `elite_pool(&archive)` → top-10 by fitness (`:476-495`, `MAX_POOL=10` at `:54`).
- `:307-310` backtests each pool genome into `pool: Vec<Vec<f64>>` (per-elite return series).
- `:355` `assess_robustness(&pool, …)` → `:572-578` builds `VintageStats { trial_returns: pool, … }`.
- `crates/validation/src/lib.rs:91` `trial_sharpe_variance(stats.trial_returns)` — variance over the **top-10** Sharpes.

Meanwhile `n_trials = effective_trials(archive.occupied_cells(), generations, windows)`
(`train.rs:352`) counts **all** occupied cells × generations × windows. So the deflation is asymmetric:
`n_trials` reflects the whole search, but `trial_variance` is estimated from a censored, tightly-clustered
tail (the 10 globally-highest-fitness survivors). A censored sample under-estimates dispersion → `E[max SR]`
too low → **DSR inflated**. G1 requires `DSR > 0.95` (`crates/gate/src/lib.rs:158`), so the bias favours
promoting over-fit vintages.

## What the archive exposes

The MAP-Elites archive (`crates/wfo/src/mapelites.rs`) retains, per occupied cell, a `SubPopulation`
of up to `SUBPOP_SIZE = 8` **elite genomes** (`Elite { genome, fitness }`) — `crates/wfo/src/archive.rs:24`.
It does **not** retain raw per-evaluation Sharpes or return series; only the surviving elite genomes.
API: `MapElitesArchive::direction(dir)` → `DirectionArchive::occupied_cells()` (BTreeMap-sorted `&Cell`),
`.cell(&Cell) -> Option<&SubPopulation>`, `SubPopulation::best()` / `.elites()`.

So the natural, least-censored trial population the archive retains is **every occupied cell's champion**
(its best elite), across both directions — one representative Sharpe per behavioural niche. Backtesting
each champion over the train window yields its net-of-cost return series → its Sharpe.

## Chosen trial population

**Best elite of every occupied cell, both directions** (the *cell champions*), backtested over the train
window. Rationale:

- It is the "Sharpes of every occupied archive cell" the spec names — the full cross-niche dispersion, not
  the top-10-by-fitness collapse.
- Its size is exactly `archive.occupied_cells()`, which is the **cell factor** in
  `n_trials = occupied_cells × generations × windows`. So `n_trials` and `trial_variance` are now derived
  from the **same population** (the occupied cells), fixing the asymmetry the ticket calls out.
- It is uncensored across niches (every niche is represented); only lightly censored within-niche (the
  champion), versus the old sample which dropped ~every niche except the 10 globally best.

Deterministic order: `[Long, Short]`, occupied cells in BTreeMap order, champion via `SubPopulation::best()`
(max fitness, `total_cmp`). Backtest is deterministic → same seed ⇒ same population ⇒ same variance ⇒ same DSR.

The top-10 `pool` is **unchanged** and still feeds the ensemble portfolio search, CSCV/PBO, and SPA
(those are separate concerns — the ensemble objective is ~cubic in pool size, `train.rs:49-54`). Only the
**trial-variance input** to the DSR moves to the full cell-champion population.

## Code changes

1. `crates/validation/src/lib.rs`
   - `VintageStats`: new field `variance_returns: &'a [Vec<f64>]` — the uncensored trial population whose
     Sharpe dispersion sets the deflation bar (distinct from `trial_returns`, which feeds CSCV/SPA).
   - `RobustnessReport`: new fields `trial_variance: f64` (the dispersion used) and
     `variance_trials: usize` (the source-population size), so the deflation basis is auditable.
   - `assess()`: `trial_variance = trial_sharpe_variance(stats.variance_returns)`; record both new fields.

2. `crates/cli/src/jobs/train.rs`
   - New `fn cell_champion_returns(archive, bars, cfg) -> Vec<Vec<f64>>` — the full population above.
   - `assess_robustness` takes `variance_returns` and sets `VintageStats.variance_returns`; the conservative
     fallback records `trial_variance: 0.0, variance_trials: 0`.
   - `n_trials` unchanged (`effective_trials(occupied_cells, generations, windows)`).

3. Field-exhaustive `RobustnessReport` constructors updated: `crates/gate/src/lib.rs` (test),
   `crates/report/src/lib.rs` (test), and the validation round-trip test.

## Report / auditable basis

`RobustnessReport` (serialised into `TrainResultDoc.robustness`, the QE-261 sidecar) now carries:
`n_trials` (deflation count), `trial_variance` (dispersion used), `variance_trials` (Sharpes it was
estimated from). The wire `Gate` ProgressLine (`dsr`, `n_trials`) is **unchanged** — out of scope and would
break the run-protocol agreement test.

## Regression test (AC)

`crates/validation/src/dsr.rs`: on a **fixed** population of 20 trial series with differing Sharpes (the
"cells"), with `top10` = the 10 highest-Sharpe of them, `trial_sharpe_variance(full) ≥ variance(top10)` and
therefore `deflated_sharpe_ratio(full-var) ≤ deflated_sharpe_ratio(top10-var)` for the same `n_trials`.
`crates/validation/src/lib.rs`: `assess()` over that full population records `variance_trials == full.len()`
and `trial_variance == variance(full)` (basis reported). The `train_job` integration test additionally
asserts that on the **real fixture archive** the variance population is broader than the old top-10
(`variance_trials > pool_size`) and that `trial_variance` is recorded — proving the change is live on a real
archive with > 10 cells.

## Fixture DSR / G1 / seal impact

Changing `trial_variance` changes DSR. On the 120-bar fixture the archive fills far more than 10 cells, so
the full-population variance ≥ the top-10 variance ⇒ DSR **decreases** (or is unchanged if the fixture
variance happens to be equal). The `train_job` tests assert **determinism** (same seed ⇒ same id + hash
across two runs) and structural facts, not a hardcoded DSR or content hash — so a changed-but-deterministic
DSR keeps them green. No golden embeds a DSR value or a sealed hash (`crates/cli/tests/train_job.rs`;
`grep` found no TrainResultDoc/robustness snapshot). The run-protocol/server `Gate`-line tests use literal
values, not a real train, so they are untouched. The measured DSR-delta on the fixture is recorded in the PR
body and the VERIFICATION return.

## Determinism

Cell-champion population is gathered in fixed order (`[Long, Short]`, BTreeMap cells, `best()` by
`total_cmp`); `backtest` is deterministic; `trial_sharpe_variance` is order-independent. Same seed ⇒ same
DSR ⇒ same G1 verdict ⇒ same sealed bytes.

## Risks

- DSR drop could flip a fixture that *passed* G1 to *failing* (or vice-versa). The 120-bar fixture is not
  expected to pass strict G1 (`train_job.rs:156`), so seal happens regardless of the verdict; the sealed
  content does not embed the DSR, so the hash is stable under a DSR change. Verified by the determinism test.
- Extra backtests (one per occupied cell champion) — bounded by occupied cells; the search already
  backtests every candidate during evolution, so this is a modest, deterministic add on the fixture.
