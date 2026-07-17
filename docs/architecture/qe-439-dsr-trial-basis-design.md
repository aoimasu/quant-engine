# QE-439 — Coherent DSR trial basis + log-N `expected_max_sharpe` fix

`Phase: Review R2 (P2 — panel #10)` · `Area: validation` · `Spec of record:` [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-439`](../reviews/2026-07-16-maxdama-panel-review.md) · `GP direction:` [`qe-450-gp-indicator-evolution-design.md §5`](./qe-450-gp-indicator-evolution-design.md)

## 1. Purpose

The panel's rank-#10 item (a **hard blocker for the GP program**, low urgency for the current
hand-catalogue engine) asks that the Deflated-Sharpe (DSR) deflation **basis** — the number `N` of
independent trials the best-of-`N` noise bar is computed against, and the population whose Sharpe
dispersion sets that bar — be made *coherent*, and flags a latent numerical degeneracy.

This note records (a) the **exact current basis** as it stands post-QE-414, (b) the concrete `+∞`
bug in `expected_max_sharpe`, (c) the fix, and (d) precisely what is delivered under this ticket
(the v1 gate) versus deferred to the GP program (QE-451).

## 2. The exact current basis (post-QE-414)

Two independent quantities feed the deflation, assembled in `crates/cli/src/jobs/train.rs`:

- **Trial count `N`** — `effective_trials(cells, generations, windows)` = `cells · generations ·
  windows` (`crates/validation/src/dsr.rs`). In `run_train_job` this is
  `effective_trials(archive.occupied_cells(), generations, train_cfg.windows)` (train.rs:461). It
  drives the noise bar `E[max SR]` via `expected_max_sharpe(trial_variance, N)`.

- **Trial-Sharpe dispersion `V`** — `trial_sharpe_variance(variance_returns)`, where
  `variance_returns = cell_champion_returns(&archive, …)` (train.rs:469): the net-of-cost return
  series of the **best elite in every occupied cell**, across both directions. This is the QE-414
  fix — one representative Sharpe per behavioural niche, an **uncensored** cross-niche sample, whose
  size is exactly `archive.occupied_cells()`, the same cell factor `N` counts. So `N` and `V` share
  the **cell** population.

  A separate, *censored*, top-`MAX_POOL=10` `pool` population feeds the CSCV/PBO and SPA columns
  (`trial_returns` / `excess_over_benchmark`) — deliberately kept distinct from `variance_returns`
  (see `VintageStats` doc).

The deflation bar itself (`expected_max_sharpe`, Bailey & López de Prado 2014):

```text
E[max SR] = √V · [ (1 − γ)·Φ⁻¹(1 − 1/N) + γ·Φ⁻¹(1 − 1/(N·e)) ]   (γ = Euler–Mascheroni)
```

### Where the basis is coherent, and where it is only conservative

- **Cell axis (QE-414): coherent.** `N`'s cell factor and `V`'s population are the *same* occupied
  cells. A censored top-N `V` would under-disperse and *inflate* DSR; the full-cell `V` does not.
- **Gens / windows axes: conservative, not coherent.** `N` multiplies the cell count by
  `generations · windows`. Serial mutations of one persistent elite across generations, and
  re-evaluations of one strategy across windows, are **not** independent trials — so the product
  **over-counts** hypotheses. Over-counting raises the noise bar ⇒ **over-deflates** ⇒ **false-reject**,
  which is the safe direction (under-deflation / false-accept is the dangerous one). Math#2's
  round-2 verdict: a *tightening opportunity*, not a live risk. This ticket does **not** loosen it.

## 3. The `+∞` degeneracy (the load-bearing bug)

`expected_max_sharpe` computes `Φ⁻¹(1 − 1/N)` by forming `1.0 - 1.0 / n` in `f64` and calling
`normal_ppf`. For `n ≳ 4.5e15` (`1/n` below the ULP of `1.0`, `2⁻⁵³ ≈ 1.11e-16`), `1 − 1/n` rounds
to **exactly `1.0`**, and `normal_ppf(1.0)` returns `+∞` by contract (`stats.rs:106`). Then:

```text
E[max SR] = √V · [ (1−γ)·(+∞) + γ·(+∞) ] = +∞
```

The DSR = `PSR(+∞)` = `Φ(−∞)` = **0** — the bar **degenerates**: every candidate is rejected
regardless of edge, and the statistic carries no information. Today `N = cells·gens·windows` on the
hand catalogue is far below `4.5e15`, so the engine never hits it — but a GP program (QE-450: a
crypto asset's ≈1,800 `T_eff` returns can be data-mined by `N ~ 1e18–1e20` distinct formulas) sits
squarely in the degenerate regime. This is a **real** latent `+∞`, and removing it is the
must-land change.

## 4. The fix — a log-space path (`~√(2 ln N)`)

Add `expected_max_sharpe_ln(trial_variance, ln_n)` that computes the two upper-tail quantiles in
**log space**, never forming `1 − 1/N`:

- The desired quantile is `Φ⁻¹(1 − p)` with tail probability `p = 1/N` (resp. `1/(N·e)`), so
  `ln p = −ln N` (resp. `−ln N − 1`).
- Acklam's inverse-normal lower tail uses `q = √(−2 ln p)`; for `p = 1/N` this is exactly
  `√(2 ln N)` — the asymptotic the spec names. By symmetry `Φ⁻¹(1 − p) = −Φ⁻¹(p)`, so we reuse
  Acklam's low-branch rational on `q` and negate. **This is the identical formula `normal_ppf`
  already runs in its `p > P_HIGH` upper branch** — the log path only avoids the catastrophic
  cancellation of forming `1 − 1/N` and re-deriving `ln(1/N)`, so it is *continuous* with the exact
  path, not a new approximation.

`expected_max_sharpe(V, n)` keeps the **exact** path (direct `normal_ppf` on `1 − 1/n` and
`1 − 1/(n·e)`) whenever both arguments are `< 1.0`, and switches to the log path only when forming
either argument rounds to `1.0` (i.e. exactly the degenerate regime, `n ≳ 4.5e15`). Consequences:

- **Small/moderate `N` (all current runs, the fixture): byte-identical.** No DSR value moves, no
  golden moves, `content_hash` is untouched (DSR lives in the `TrainResultDoc` sidecar, never in
  the sealed `VintageContent`).
- **Large `N`: finite and self-capping.** The bar grows like `√(2 ln N)` — `√(2 ln 1e20) ≈ 9.6` —
  so it stays in the ~8–13 band the spec predicts, even at `N ~ 1e20`, instead of `+∞`.

The direction remains conservative: the log bar is *finite but still large*, so a real GP-scale
search is deflated hard (not accidentally passed by a `+∞→DSR=0` that a caller might special-case).

## 5. Uncensored dispersion (item 3, extends QE-414)

The spec's item 3 asks the DSR dispersion population move toward the **full evaluated population**.
Absent a GP, **no full evaluated population is retained**: MAP-Elites keeps only the per-cell
champion (`SubPopulation::best()`); the transient variation candidates that lost their niche
tournament are discarded and never backtested for a return series. The **coherent** population that
*is* retained is precisely the cell-champion set QE-414 already feeds — the broadest uncensored
cross-niche sample the archive holds, and the one whose size matches `N`'s cell factor. Feeding a
*wider* sample would require materialising every evaluated genome's returns, which only the GP
program's evaluation ledger provides (QE-450 §5: "count every evaluated formula … record
`distinct_evaluations`"). So under this ticket the cell-champion population **is** the coherent
choice; broadening it is deferred to QE-451. Documented, not silently narrowed.

## 6. Delivered vs deferred

**Delivered (v1 gate, this ticket):**

1. `expected_max_sharpe_ln(ln_n)` log-space path; `expected_max_sharpe` routes the degenerate
   large-`N` regime through it. Removes the `+∞`. **Load-bearing.**
2. Basis made coherent **and documented**: `effective_trials` / `VintageStats::n_trials` doc'd as
   the **analytic floor** (`cells·gens·windows`) — the current, deliberately conservative
   independent-trials basis — with the gens/windows over-count called out as safe-direction and the
   coherent tightening deferred.
3. Uncensored dispersion confirmed coherent (QE-414 cell champions) with the reason the full
   evaluated population is unavailable absent GP recorded here.

**Deferred to the GP program (QE-451 / QE-450 §5):**

- `N = max(distinct-CANONICAL formulas ever scored, cells·gens·windows floor, complexity floor)`
  with canonicalisation + content-hash dedup, and `distinct_evaluations` in `RobustnessReport`
  (needs a GP evaluation ledger — nothing to count on the hand catalogue).
- Complexity-stratified `trial_sharpe_variance` (per node-count band) — needs `ExprTree` node
  counts; the retracted `B^{κn}` N-multiplier stays retracted.
- Full-evaluated-population `variance_returns` + stratified uncensored PBO as the primary GP gate.
- The label-shuffle / block-bootstrap κ-calibration null (extends `nulls.rs`).

## 7. Tests (TDD)

- **Headline:** `expected_max_sharpe(V, N)` is **finite** and in ~[8·√V, 13·√V] for `N ∈ {1e15,
  1e18, 1e20}` (was `+∞`); at `V=1` the bar is ≈9.6 at `N=1e20`.
- **Small-N unchanged:** `expected_max_sharpe` below the switch threshold equals the direct exact
  `normal_ppf` formula bit-for-bit; the existing dispersion/monotonicity tests stay green.
- **Continuity:** `expected_max_sharpe_ln` matches `expected_max_sharpe` to <1e-9 across moderate
  `N` (the two paths agree where both are valid).
- **Monotone + self-capping:** the bar keeps rising with `N` but stays `< 14·√V` through `N=1e20`.
