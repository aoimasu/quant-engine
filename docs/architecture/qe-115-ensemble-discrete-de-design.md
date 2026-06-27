# QE-115 — SPIKE: Ensemble discrete differential evolution — design / decision record

`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-113` · **Blocks: QE-126, QE-127**
`Branch: qe-115/ensemble-discrete-de`

## Goal (from backlog)

Portfolio search must be tail-aware, wide-basin, and explicitly de-correlated; behavioural diversity ≠
return-correlation diversity.

**Scope / requirements.**
- Define discrete DE over the strategy pool; tail-aware objective (specify CVaR/CDaR estimator and its
  standard-error caveat); wide-basin (robust plateau) preference.
- Define a correlation/covariance penalty on **net-of-cost** returns and fold CV usage.
- Define a synthetic/stress tail overlay (not empirical tails alone).

**Acceptance criteria.**
- [ ] Decision record fixes objective, estimator, correlation term.
- [ ] A fixture shows two highly P&L-correlated strategies are penalised despite behavioural difference.

**Out of scope.** Capacity analysis (QE-128); the DE search *loop* (QE-126); per-regime expectancy
(QE-127); position weights (QE-128 — this SPIKE assumes equal weight).

## Current-state evidence

- **QE-113** fixes the net-of-cost, geometric, noise-robust *per-strategy* fitness philosophy. This
  ticket is the **portfolio-level** objective — tail-aware, correlation-penalised — which is a different
  object from per-strategy fitness and is computed on the **combined** ensemble return series.
- **Firewall (QE-001/QE-132).** Search ⟂ portfolio: `qe-ensemble` must **not** depend on `qe-wfo`. All
  tail/correlation math here is therefore self-contained `f64` (the portfolio reads member return series,
  not the search's internal state). Confirmed: this SPIKE adds **no** `qe-wfo` dependency.

## Decision

### D1 — Representation: a binary inclusion mask over the strategy pool

An ensemble is a fixed-length **bit mask** `EnsembleMask(Vec<bool>)` over the strategy pool (bit `i` =
"strategy `i` is a member"). Fixed length ⇒ discrete DE's per-locus difference is well-defined; binary ⇒
the search is over *subsets*, exactly the portfolio-selection problem. Weights are equal in this SPIKE
(QE-128 sets real weights).

### D2 — Discrete differential evolution operators

DE over binary vectors (Pampará-style binary DE adapted to set selection):

- **Mutant** `v = a XOR (b XOR c)` — the binary analogue of `a + (b − c)`: loci where donors `b` and `c`
  **differ** are the "difference", toggled onto base `a`. Deterministic, parameter-free.
- **Binomial crossover** `u_i = v_i if (rng < CR or i == j_rand) else target_i`, with one guaranteed
  inherited locus `j_rand` (so the trial always differs from the target). `CR` default 0.9. The RNG is the
  seeded `DetRng` (QE-006) — supplied by the caller; operators are pure given it.

The DE **loop** (selection, generations, archive) is QE-126; this SPIKE fixes the operator surface.

### D3 — Tail-aware objective (CVaR estimator + SE caveat)

The portfolio's per-period **net-of-cost** combined return is the equal-weight mean of its members'
returns. The objective rewards mean growth while penalising the left tail:

```
tail_aware_return(combined) = mean(combined) + tail_weight · CVaR_α(combined)
```

- **CVaR_α (Expected Shortfall)** = the **mean of the worst `⌈α·n⌉` returns** (the left-tail average).
  It is a *negative* number when the tail is losses, so adding `tail_weight·CVaR` (with `tail_weight > 0`)
  **lowers** the objective for fat left tails. `α` default 0.05. CVaR (coherent, sub-additive) is chosen
  over VaR (not coherent, ignores tail shape).
- **CDaR (Conditional Drawdown at Risk)** is the drawdown analogue — the mean of the worst `α` drawdowns
  of the equity curve — provided as `cdar_α` for the drawdown-sensitive variant; documented as the
  alternative tail measure for QE-126 to select per objective.
- **Standard-error caveat (explicit).** CVaR/CDaR are averages over only `⌈α·n⌉` observations, so their
  standard error is large and they are themselves noisy — the estimator returns the **tail sample count**
  alongside the value (`TailRisk { value, tail_n }`) so QE-126 can down-weight a CVaR estimated from a
  handful of points. This is the tail analogue of QE-113/D4's SE-aware caution.

### D4 — Synthetic / stress tail overlay (not empirical tails alone)

Empirical tails under-sample rare events, so CVaR is computed on the **empirical series augmented with
synthetic shock scenarios** (`stress_overlay(returns, &shocks)`): gap, funding-spike, and ADL-style
shock returns are appended before the tail is taken. The tail estimate therefore reflects plausible
worst-cases the in-sample window never contained. Shocks are config-supplied (QE-126/130 own the shock
library); this SPIKE fixes the *mechanism*.

### D5 — Correlation/covariance penalty on net-of-cost returns

Behavioural diversity (QE-111 descriptors) is **not** return-decorrelation: two structurally different
genomes can produce near-identical P&L. The objective therefore subtracts an explicit penalty on the
**return** correlation of members:

```
objective(pool, mask) = tail_aware_return(combined) − corr_weight · positive_mean_pairwise_corr(members)
```

`positive_mean_pairwise_corr` = the mean over all member pairs of `max(pearson(rᵢ, rⱼ), 0)` on their
**net-of-cost** return series (negative correlation is a *benefit*, so it is floored at 0 and does not
reduce the penalty below the independent case). `corr_weight` default 1.0. Zero-variance series ⇒ 0
correlation (guarded). This is the term the AC fixture exercises: two highly P&L-correlated strategies
incur a large penalty **despite** any behavioural difference, because the penalty reads returns, not
descriptors.

### D6 — Wide-basin (robust plateau) preference + fold CV usage

- **Wide-basin.** Prefer ensembles whose objective does **not collapse** when a single member is dropped
  — a robust plateau, not a sharp peak that depends on one lucky strategy. `leave_one_out_min(pool, mask)`
  returns the worst single-member-removed objective; QE-126 prefers ensembles whose LOO-min stays high
  (robustness floor), folded into selection.
- **Fold CV usage.** The objective is evaluated **per CV fold** (QE-113 purged/embargoed folds) and
  aggregated (mean across folds), never on one blended series — so an ensemble must be tail-aware and
  decorrelated *across* folds, not just on the full sample. This SPIKE evaluates the objective on a
  supplied return matrix; QE-126 drives it across folds.

## Module / API plan

Two modules in `qe-ensemble`, re-exported:

- `crates/ensemble/src/objective.rs`
  - `pearson(&[f64], &[f64]) -> f64`; `positive_mean_pairwise_corr(&[Vec<f64>]) -> f64`.
  - `TailRisk { value, tail_n }`; `cvar(&[f64], alpha) -> TailRisk`; `cdar(&[f64], alpha) -> TailRisk`.
  - `stress_overlay(&[f64], &[f64]) -> Vec<f64>`.
  - `combined_returns(pool: &[Vec<f64>], mask) -> Vec<f64>` (equal-weight).
  - `ObjectiveConfig { alpha, tail_weight, corr_weight }`; `objective(pool, mask, cfg) -> f64`;
    `leave_one_out_min(pool, mask, cfg) -> f64`.
- `crates/ensemble/src/de.rs`
  - `EnsembleMask(Vec<bool>)`; `de_mutant(a, b, c) -> EnsembleMask`;
    `binomial_crossover(target, mutant, cr, rng) -> EnsembleMask`.
- `Cargo.toml`: add `rand_core` (the crossover RNG bound); `qe-determinism` dev-dep (seeded RNG). **No
  `qe-wfo` dependency** (firewall).

## Test plan (TDD)

1. **Pearson / correlation penalty.** `pearson` of identical vs anti-correlated vs independent series;
   `positive_mean_pairwise_corr` floors negatives at 0; zero-variance ⇒ 0.
2. **CVaR / CDaR.** Hand-computed tail averages; `tail_n = ⌈α·n⌉`; CVaR ≤ mean; CDaR on a known equity path.
3. **Stress overlay.** Appending shock returns worsens (lowers) CVaR vs the empirical-only tail.
4. **Correlated strategies penalised (AC).** A pool with A, B≈A (corr≈1), and C independent of A with the
   same marginal mean/vol: `objective({A,C}) > objective({A,B})` and the `{A,B}` correlation penalty is
   larger — penalised **despite** A/B being (notionally) behaviourally distinct.
5. **Wide-basin.** `leave_one_out_min` drops the member whose removal most hurts; a one-strategy-dependent
   ensemble scores a lower LOO-min than a balanced one.
6. **Discrete DE operators.** `de_mutant = a XOR (b XOR c)` toggles exactly the b/c-difference loci;
   `binomial_crossover` is deterministic under a seeded RNG, always inherits ≥1 mutant locus (`j_rand`),
   and reduces to target/mutant at `CR = 0`/`1`.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-ensemble`,
`cargo test --workspace`.

## Risks

- **CVaR/CDaR estimator noise.** Few tail points ⇒ high SE; surfaced via `tail_n` and the stress overlay,
  but QE-126 must down-weight thin-tail estimates (documented contract).
- **Equal-weight assumption.** Real weights/capacity are QE-128; the objective here is weight-agnostic
  (equal). Mixing in weights later changes `combined_returns` only.
- **Correlation is linear (Pearson).** Tail dependence can exceed linear correlation; a rank/tail-dep
  measure is a documented upgrade if linear decorrelation proves insufficient (QE-127 evidence).
- **`alpha`/`corr_weight`/`CR` are pre-data constants.** Constructor params now, config (QE-002) once the
  DE loop (QE-126) runs on real pools.
