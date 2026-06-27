# QE-131 — Statistical robustness suite — design note

`Phase: P1` · `Area: ⑤/⑥ validation` · `Depends on: QE-120, QE-126`
`Branch: qe-131/statistical-robustness-suite`

## Goal (from backlog)

*(Reviewer-added; milestone definition-of-done.)* A large QD archive is a multiple-testing machine; an
undeflated OOS Sharpe is the *expected* output of the search, not evidence of edge.

- **Deflated Sharpe Ratio** with effective trials = archive cells × generations × windows.
- **PBO** via CSCV; **White's Reality Check / Hansen's SPA** vs a best-of-N null.
- **Benchmark/null comparison:** BTC-HODL and turnover-matched random-entry nulls.

**Acceptance criteria.**
- [ ] The suite computes DSR/PBO/SPA for a vintage; results gate G1 (QE-134).

**Out of scope.** Reporting layout (QE-133).

## Current-state evidence & placement

- QE-120 (backtester) and QE-126 (DE search) produce the per-strategy net-of-cost **return series** the
  suite consumes; the methods are pure statistics over `&[f64]` / `&[Vec<f64>]` matrices.
- **A new crate `qe-validation`** (Area ⑤/⑥). It is *downstream* validation — like QE-129's vintage, it
  records/judges outputs, so it does not touch the search⟂portfolio firewall. It operates purely on
  return matrices + trial counts and depends only on `qe-determinism` (deterministic bootstrap & null
  RNG, QE-006) and `serde` (the report feeds G1). **No `qe-wfo`/`qe-ensemble` dep** — the suite never
  needs their types, only return series the caller extracts.
- No reusable `normal_cdf`/Sharpe/skew/kurtosis exists in the workspace (signal rolls are `Decimal`), so
  the `f64` stats primitives live here.

## Design

### D1 — Stats primitives (`stats.rs`)
`mean`, `variance` (ddof), `std_dev`, `skewness` (m3/m2^1.5), `kurtosis` (m4/m2², **non-excess**, normal =
3), `sharpe_ratio` (per-period mean/std), `normal_cdf` (Abramowitz–Stegun erf, ≤1.5e-7), `normal_ppf`
(Acklam inverse-CDF), `EULER_MASCHERONI`. Deterministic, pure.

### D2 — Deflated Sharpe Ratio (`dsr.rs`, Bailey & López de Prado 2014)
- `probabilistic_sharpe_ratio(returns, sr_benchmark)` =
  `Φ[ (SR − SR*)·√(T−1) / √(1 − γ3·SR + ((γ4−1)/4)·SR²) ]` (SR sample Sharpe, γ3 skew, γ4 kurtosis).
- `expected_max_sharpe(trial_variance, n_trials)` =
  `√V · [ (1−γ)·Z⁻¹(1−1/N) + γ·Z⁻¹(1−1/(N·e)) ]` — the Sharpe the *best of N independent trials* is
  expected to show under a zero-edge null (γ = Euler–Mascheroni).
- `deflated_sharpe_ratio(returns, trial_variance, n_trials)` = `PSR(expected_max_sharpe(V, N))` — the
  probability the strategy's true Sharpe exceeds what best-of-N noise alone would produce.
- `effective_trials(cells, generations, windows) = cells·generations·windows`.

### D3 — PBO via CSCV (`pbo.rs`, Bailey–Borwein–López de Prado–Zhu 2017)
`pbo_cscv(matrix /* T×N */, blocks S /* even */, metric)`: split the T rows into S contiguous blocks; for
each of the `C(S, S/2)` IS/OOS partitions, find the IS-best strategy `n*`, take its OOS rank `ω∈[1,N]`,
form the relative rank `ω̄ = ω/(N+1)` and logit `λ = ln(ω̄/(1−ω̄))`. **PBO = fraction of partitions with
`λ ≤ 0`** — the probability the IS-best is below the OOS median (overfit). Returns `PboReport { pbo,
n_combinations, logits }`.

### D4 — Reality Check / SPA (`spa.rs`, White 2000 / Hansen 2005)
`reality_check_pvalue(excess /* per-strategy series vs benchmark */, cfg)`: statistic `V = maxₖ √T·d̄ₖ`;
**stationary bootstrap** (Politis–Romano, geometric blocks, deterministic via `task_rng`) gives
recentred `V*ᵦ = maxₖ √T·(d̄*ₖ − d̄ₖ)`; **p = #{V*ᵦ ≥ V}/B**. The `studentize` flag divides each `d̄ₖ` by
its bootstrap std (Hansen's SPA refinement). High p ⇒ the best strategy's edge is indistinguishable from
best-of-N data-snooping.

### D5 — Nulls (`nulls.rs`)
- `buy_and_hold_returns(prices)` = simple period returns of the benchmark (BTC-HODL).
- `random_entry_returns(market_returns, target_turnover, seed)` = a random long/flat position series whose
  switch probability matches `target_turnover`, earning the market return while in position
  (turnover-matched random-entry null). Deterministic via `task_rng(seed, ·)`.

### D6 — Suite entry point (`lib.rs`)
`assess(inputs: &VintageStats, cfg) -> RobustnessReport { dsr, pbo, spa_pvalue, benchmark }` bundles D2–D5
for one vintage. `RobustnessReport` is `serde` so G1 (QE-134) can consume/record it. `VintageStats`
carries the candidate returns, the trial return matrix, the effective-trial counts, and the benchmark
price/return series the caller extracts.

## Module / API plan

New crate `crates/validation` (`qe-validation`), `[workspace.dependencies]`-registered:
`stats`, `dsr`, `pbo`, `spa`, `nulls` modules; `RobustnessReport`, `VintageStats`, `assess`,
`ValidationError` (e.g. odd block count, empty matrix). Deps: `qe-determinism`, `rand_core`, `serde`.

## Test plan (TDD)

1. **Stats** — `normal_cdf(0)=.5`, `normal_cdf(1.96)≈.975`, `normal_ppf` inverts it; skew≈0/kurtosis≈3 on
   a symmetric set; a positive constant-ish series has a large Sharpe.
2. **DSR** — `expected_max_sharpe` increases with N (more trials ⇒ higher noise bar); a strong, long track
   record ⇒ DSR→1; raising N lowers DSR; PSR monotone in SR.
3. **PBO** — a genuinely-robust matrix (same column best IS & OOS) ⇒ PBO≈0; an overfit matrix (IS-best
   engineered to be OOS-worst) ⇒ PBO≈1; symmetric noise ⇒ ≈0.5. Odd `S` ⇒ `ValidationError`.
4. **SPA/RC** — a genuine positive-edge strategy among noise ⇒ low p; a pure best-of-many noise winner ⇒
   high p; determinism (same seed ⇒ same p).
5. **Nulls** — `buy_and_hold_returns` reproduces hand-computed returns; `random_entry_returns` is
   deterministic per seed and its realised turnover ≈ target.
6. **`assess` (AC)** — produces a populated `RobustnessReport` (DSR, PBO, SPA) for a fixture vintage;
   round-trips through serde.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-validation`,
`cargo test --workspace`, `cargo deny check`.

## Risks

- **Approximation accuracy.** `erf`/inverse-CDF are standard closed-form approximations (≤1.5e-7 / ~1e-9);
  tests assert tolerances. Adequate for a gate; a special-function crate is a later swap behind the same
  API.
- **Bootstrap cost.** `C(S,S/2)` and `B` resamples are bounded by config (defaults small enough for CI);
  documented. Determinism via `task_rng` keeps p-values reproducible (QE-006).
- **Inputs are caller-extracted.** The suite trusts the caller to supply aligned return matrices and the
  true effective-trial counts (cells×generations×windows) — the deflation is only as honest as N. The
  assembly layer (G1/QE-134) wires the real counts; documented.
- **RC vs SPA.** White's Reality Check is the baseline; the `studentize` flag gives Hansen's SPA
  refinement. Both share the bootstrap; the consistent (recentred) variant is the default.
