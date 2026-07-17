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

### 3d. `crates/wfo/src/mapelites.rs` + `crates/cli/src/jobs/train.rs` — LIVE wiring at the graduation-champion pick
The helpers above are not left dormant. `MapElitesArchive::parsimonious_equal(genome, gate)` is the
graduation-champion selector: for a genome the ensemble selected for deployment, it gathers the archived
elites in that genome's own niche(s) that are **tied on stored selection fitness**, and returns the
**most parsimonious** among them (via `QualityGate::most_parsimonious`), to seal in the genome's place.
The train seal path (`train.rs`, just after the ensemble picks `selected`) maps each deployed member
through it:
```
let grad_gate = QualityGate::with_defaults();
let chromosomes = selected.iter()
    .map(|&i| archive.parsimonious_equal(&pool_genomes[i], &grad_gate))
    .collect();
```

**Why this is the DSR-safe home.** Traced end-to-end (evidence in §2 of the review map): the DSR
**candidate** is `in_sample_returns = combine(chromosomes)`, but the DSR **trial-variance basis**
(`cell_champion_returns` = per-cell `best()`), the **n_trials** (`archive.occupied_cells()`), and the
DSR/PBO/SPA **trial columns** (`pool` = `elite_pool`) are all derived from the *unchanged* archive and do
**not** depend on which member is deployed. `parsimonious_equal` reads only the stored **scalar** fitness
and the genotype — never a `.returns` series — and never mutates the archive, so `best()`/`elite_pool`/
`cell_champion_returns` and thus the deflation **bar** are byte-identical. Only the deployed *candidate*
may shift to an equal-fitness simpler genome, which the unchanged bar then honestly deflates. So the
escape-hatch condition ("wiring cannot avoid feeding the DSR variance basis") is **not** triggered: the
variance basis is not fed.

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

The DSR **trial-variance basis** (`cell_champion_returns` / `best()`), **n_trials**
(`archive.occupied_cells()`), and the DSR/PBO/SPA **trial columns** (`elite_pool`) are **not** modified; the
`.returns` series and `persists`/`survivors` pass/fail are unchanged; `should_replace` stays pure. The one
in-pipeline behaviour change is the graduation-champion swap (§3d), which can only move the deployed
`chromosomes` — and only between genomes of **exactly equal** stored selection fitness.

Measured on the seed-42 fixture (`train_job`), before→after the wiring:

- `content_hash` = `afda70723fdc5c2188026c85ed261c03db13b7768e9af2c0151f800b2f8caec5` **unchanged**
  (identical with the wiring stashed vs. live). No equal-fitness / unequal-complexity tie occurs among the
  ensemble-selected members on this fixture, so no swap fires — the wiring is **behaviour-preserving on the
  fixture but active in general** (guarded live by the test in §7).
- `crates/cli/tests/fixtures/golden_result.json` (a fixed pre-sealed-vintage backtest — does not run the
  search) is unaffected.

So **no golden moved** and no regeneration is required; no new hashed field was added, so
`VINTAGE_FORMAT_VERSION` is unchanged. Had a champion shifted, it would be a pure equal-fitness parsimony
tie-break (not a fitness distortion) and the golden would be regenerated via the real code path only
(`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`), never by hand.

## 6. Scope note — "decouple size from rule discovery"

The in-search parsimony operationalisation the panel's rank-#7 recommendation centres on is delivered and
**live** at the graduation-champion pick (§3d). The ticket's *second* half (two-stage search under a
size-normalised/unit-risk fitness, or an alpha-quality term) would require restructuring the
size-co-evolution inside the **DSR-facing** scalar-fitness search — the very path forbidden here — and is a
larger change than an `Effort: S` tie-break. It stays a documented **follow-up**, to be done without
injecting a size term into the DSR-facing selection fitness (tracked against the same spec ref).

## 7. Tests (TDD)

- `genome.rs`: `mdl_complexity` orders a 1-clause genome below a 4-clause genome; counts distinct features.
- `fitness.rs`: at equal robust fitness the 1-clause challenger replaces the 4-clause incumbent; a
  materially-better complex genome still replaces a simple one and a materially-worse simple genome does
  not; ruin never replaces; the MDL term is proven out of `should_replace`/the fitness scalar.
- `lifecycle.rs`: `most_parsimonious` picks the simplest among equal-robust survivors; a materially-higher
  lower-bound candidate always wins regardless of complexity; determinism.
- **`mapelites.rs` (the "wiring is LIVE" guard):**
  `parsimonious_equal_deploys_the_simpler_of_two_tied_niche_elites` — inserts two equal-fitness elites of
  different complexity into one niche (the more complex one **first**) and asserts the graduation-champion
  pick deploys the **simpler** one; also asserts a materially-better complex elite is never swapped away and
  an un-archived genome deploys unchanged. Fails if the tie-break is unwired (i.e. if `parsimonious_equal`
  degenerates to returning its input).
