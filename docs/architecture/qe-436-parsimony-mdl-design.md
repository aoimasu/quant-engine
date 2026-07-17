# QE-436 — In-search parsimony (MDL) tie-break + decouple size from rule discovery — design/evidence note

`Phase: Review R2.b (P2 — panel #7, majority)` · `Area: wfo / signal` · `Effort: S`
Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-436`](../reviews/2026-07-16-maxdama-panel-review.md#qe-436).
Spec ref: maxdama §5.4 ("better to have fewer parameters than many") + §4.9 (Voodoo Spectrum: variable
selection at the dangerous end).

## 1. Problem (evidenced against the tree)

Nothing in the search rewards a *simpler* genome, so a 1-clause and a 4-clause genome that produce the
same robust fitness compete on fitness alone and the search drifts to maximal complexity over correlated
indicators (over-fitting surface). Grep-confirmed against `main`:

- `crates/wfo/src/fitness.rs` — `log_growth`, `NoiseRobustFitness`, `should_replace`: purely
  return-driven; no clause/feature term anywhere.
- `crates/wfo/src/lifecycle.rs` — `QualityGate::persists` gates on the lower-bound
  `mean − k_sigma·se ≥ threshold`; no complexity term.
- `crates/wfo/src/regularise.rs` — `BehaviouralRegulariser` is *behavioural novelty* (niche crowding),
  **not** structural complexity. It does not reward fewer clauses.
- `crates/signal/src/genome.rs` — already exposes the raw structural counts we need:
  `RuleSet::active_count()` (enabled clauses) and `Genome::referenced_features()` (distinct enabled
  feature indices across both banks). No MDL/complexity scalar exists yet.

## 2. The hard constraint: MDL must stay OUT of the DSR-facing fitness

Traced the fitness → DSR path so the penalty cannot interact with the deflation stage:

- The **scalar archive fitness** is `fold_isolation_fitness(g, …).mean` (`crates/cli/src/jobs/train.rs`),
  a plain `f64` inserted via `MapElitesArchive::insert(genome, fitness)`.
- The archive's elite-per-cell winner is chosen by a **raw scalar strict-greater** comparison
  (`SubPopulation::consider`, `mapelites.rs`) and `SubPopulation::best()` (max scalar). `should_replace`
  and its noise band are **not** wired into this path today.
- **Which** genomes' return series reach DSR is driven by that scalar: `cell_champion_returns` uses
  `sub.best()` and `elite_pool` ranks by `elite.fitness` → top `MAX_POOL` → ensemble members →
  `validation::assess(...)` → `deflated_sharpe_ratio(candidate_returns, trial_variance, n_trials)`
  (`crates/validation/src/dsr.rs`). The DSR operates on the raw net-of-cost `BacktestResult.returns`.

**Therefore:** injecting an MDL term into the scalar archive fitness (or into `.returns`, or into
`consider`/`best()`/`elite_pool` ordering) would change which champions/pool members reach DSR and move
the deflation basis. That directly violates the ticket constraint ("keep the MDL λ OUT of the per-genome
fitness that feeds DSR"). We must **not** touch the scalar archive path.

The DSR-safe surfaces the ticket names are exactly the two that carry the SE/noise band and are *not* the
scalar-fitness champion selector:

1. `fitness::should_replace` — the documented SE-aware replacement primitive (has both means + SEs, so a
   genuine noise band). Pure fitness today; we keep it pure and add a **sibling** parsimonious variant.
2. `lifecycle::QualityGate` — the graduation gate, which holds `NoiseRobustFitness` (mean + SE).

## 3. Design (tie-break only, minimal behaviour change)

### 3a. `crates/signal/src/genome.rs` — MDL complexity metric
Add `Genome::mdl_complexity(&self) -> u32`, a description-length proxy:

```
mdl_complexity = enabled_clauses(long) + enabled_clauses(short) + distinct_referenced_features
```

i.e. `long_entry.active_count() + short_entry.active_count() + referenced_features().len()`. Lower ⇒ more
parsimonious. Pure function of the genome, deterministic. This is the only "magnitude" MDL uses; it never
enters any fitness scalar.

### 3b. `crates/wfo/src/fitness.rs` — parsimony tie-break inside the noise band
- Keep `should_replace` **unchanged and pure** (add a doc note that MDL is deliberately excluded so the
  DSR-facing decision is never distorted).
- Add `within_noise_band(incumbent, challenger, k_sigma) -> bool`: true when *neither* genome clearly wins
  on fitness, i.e. `|challenger.mean − incumbent.mean| ≤ k_sigma·combined_se` (and both finite). This is
  the "equal robust fitness within the noise band" region.
- Add `should_replace_parsimonious(incumbent, challenger, inc_complexity, chal_complexity, k_sigma) ->
  bool`, **lexicographic**:
  1. If `should_replace(incumbent, challenger, k_sigma)` (challenger clearly better) ⇒ `true`.
  2. Else if the challenger clearly loses (incumbent clearly better, or challenger non-finite) ⇒ `false`.
  3. Else (a statistical tie inside the noise band) ⇒ break toward parsimony:
     `chal_complexity < inc_complexity`.

  The MDL term is consulted **only** in branch 3 — it can never override a material fitness difference and
  never enters a fitness value. DSR-safe by construction.

### 3c. `crates/wfo/src/lifecycle.rs` — parsimony tie-break at the gate
The gate's boolean `persists` pass/fail set is left **unchanged** (changing it would move the graduation
set and hence goldens). Add *additive* selection helpers that operationalise "tie-break toward parsimony
at equal robust fitness" when a caller must pick a single champion among equal-robust survivors:

- `QualityGate::robust_lower_bound(fitness) -> f64` = `mean − k_sigma·se` (the lifecycle lower bound,
  named).
- `QualityGate::graduation_cmp((fit_a, cx_a), (fit_b, cx_b)) -> Ordering`: primary = robust lower bound
  (higher is better); when the two are within the noise band (a tie), secondary = complexity ascending
  (fewer clauses wins); final deterministic tie-break preserved by the caller's stable order.
- `QualityGate::most_parsimonious<'a, T>(candidates) -> Option<&'a T>`: among candidates that share the
  best robust lower bound (within the noise band), returns the lowest-complexity one; deterministic.

## 4. Why this is a strict parsimony tie-break, not a fitness distortion

- The MDL magnitude (`mdl_complexity`) is an integer read off the genome; it is **never** added to,
  subtracted from, or multiplied into any `log_growth` / `NoiseRobustFitness.mean` / scalar archive
  fitness / `BacktestResult.returns`.
- It is consulted **only** when two genomes are statistically indistinguishable on robust fitness (inside
  the `k_sigma` noise band). A material fitness difference is always decided on fitness alone — MDL cannot
  flip it.
- It is **deterministic** (pure integer comparison; existing lowest-index / stable-order tie-breaks
  preserved), so byte-reproducibility is untouched.

## 5. Golden / vintage impact

The scalar archive path (`consider`/`best`/`elite_pool`), the `.returns` series, and the DSR inputs are
**not** modified. `should_replace` stays pure; `persists` pass/fail is unchanged. The new APIs
(`mdl_complexity`, `should_replace_parsimonious`, `within_noise_band`, gate selection helpers) are
additive and are **not** wired into the vintage-producing pipeline in this ticket, so:

- `crates/cli/tests/fixtures/golden_result.json` (a fixed sealed-vintage backtest) is unaffected.
- `crates/cli/tests/train_job.rs` reproducibility / `content_hash` assertions are unaffected.
- The determinism harness and parity tests are unaffected.

Expected `content_hash` movement: **none**. Verified by the full green gate below. Had a golden moved, we
would regenerate via the real code path only
(`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`), never by hand, and
bump `VINTAGE_FORMAT_VERSION` only if a new hashed field were added (none is).

## 6. Scope note — "decouple size from rule discovery"

The ticket's second half (two-stage search under a size-normalised/unit-risk fitness, or an alpha-quality
term) would require restructuring the size-co-evolution in the scalar-fitness search — which is exactly the
DSR-facing path we are forbidden to touch here, and is a larger change than an `Effort: S` tie-break. The
parsimony tie-break delivered here is the in-search parsimony operationalisation the panel's rank-#7
recommendation centres on; the size/rule decoupling is left as a follow-up that must be done without
injecting a size term into the DSR-facing selection fitness (tracked against the same spec ref).

## 7. Tests (TDD)

- `genome.rs`: `mdl_complexity` orders a 1-clause genome below a 4-clause genome; counts distinct features.
- `fitness.rs`: at equal robust fitness the 1-clause challenger replaces the 4-clause incumbent; a
  materially-better complex genome still replaces a simple one and a materially-worse simple genome does
  not; ruin never replaces; the MDL term is proven out of `should_replace`/the fitness scalar.
- `lifecycle.rs`: `most_parsimonious` picks the simplest among equal-robust survivors; a materially-higher
  lower-bound candidate always wins regardless of complexity; determinism.
