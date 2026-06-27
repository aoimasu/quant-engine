# QE-121 — Thompson-sampling parent selection — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-118`
`Branch: qe-121/thompson-parent-selection`

## Goal (from backlog)

Bayesian parent selection under fitness uncertainty; **reward must avoid OOS leakage**.

- Thompson sampling over parent niches; reward = in-training improvement / novelty, **never** validation
  performance.

**Acceptance criteria.**
- [ ] Parent selection demonstrably uses no held-out validation signal (leakage test).

**Out of scope.** Operator selection (QE-119 — the operator bandit; this is the *parent-niche* bandit).

## Current-state evidence

- **QE-118** (`qe_wfo::mapelites`) gives the niche structure to sample over: `MapElitesArchive` →
  per-direction `DirectionArchive` (`occupied_cells`, `cell`, `SubPopulation::elites`). QE-118's own
  `sample_parent` is **uniform** over occupied cells (diversity baseline). QE-121 replaces that with a
  Bayesian bandit that concentrates parent budget on the **productive** niches under uncertainty.
- **QE-112/119** give the *only* legitimate reward currency: `ApplicationOutcome { NewCell,
  ImprovedElite{gain}, NoImprovement }` and its `reward()` — **in-training** novelty / elite-improvement.
  The firewall (QE-001/QE-132) forbids any out-of-sample / validation signal in the search; this bandit's
  reward channel is *structurally* that in-training outcome and nothing else.

## Design

### D1 — Per-niche Gaussian (Normal–Normal) posterior over reward

Each occupied `Cell` is a bandit arm with a conjugate Normal posterior over its mean in-training reward.
Prior `N(μ0, σ0²)`; each observation is one application's `reward ∈ [0, ∞)` with known observation
variance `σ²`. The running posterior from `n` observations summing to `s` is

```
precision = 1/σ0² + n/σ²
mean      = (μ0/σ0² + s/σ²) / precision
var       = 1 / precision
```

Gaussian (not Beta) because the reward is a continuous magnitude (`ImprovedElite{gain}` can exceed 1),
and the Normal–Normal update is trivial to sample deterministically. An **unseen** cell keeps the prior
`N(μ0, σ0²)` — deliberately optimistic, so Thompson sampling still explores niches with no history.

### D2 — Thompson selection

`select_parent(archive, direction, rng)`: for **each occupied cell** in that direction, draw one
posterior sample `μ̃ ~ N(mean, var)` (standard normal via Box–Muller from the seeded `DetRng`, scaled
and shifted); pick the **argmax** cell (ties broken by sorted `Cell` order — deterministic); then sample
an elite **uniformly within** that cell. Returns `(Cell, &Genome)` so the caller can later credit the
same cell. Drawing per occupied cell from one `DetRng` in `BTreeMap` order makes selection a pure
function of the rng state (QE-006). Concentrates on high-reward niches while the posterior variance keeps
exploring uncertain ones — the Bayesian "under fitness uncertainty" the ticket asks for.

### D3 — Reward is in-training only (AC — no OOS leakage)

`record(cell, &ApplicationOutcome)` is the **only** way to update a posterior, and `ApplicationOutcome`
is the QE-112/119 in-training credit (novelty / elite-improvement) — there is **no parameter, field, or
code path** by which a held-out / validation score can enter the bandit. This is the structural
guarantee; the leakage test makes it observable: two niches are set up so the in-training reward and a
(never-passed) validation score are **anti-correlated** — the selector, fed only in-training rewards,
concentrates on the in-training winner and ignores the validation winner. If any OOS signal leaked in,
selection would track the validation winner; it does not.

## Module / API plan

New module `crates/wfo/src/thompson.rs`, re-exported:

- `NichePrior { mean, var, obs_var }` (+ `Default` = `{0, 1, 1}`); `DEFAULT_PRIOR_*` consts.
- `ThompsonParentSelector::{new, with_defaults, record, posterior_mean, select_parent}`.
- `select_parent(&MapElitesArchive, Direction, &mut DetRng) -> Option<(Cell, &Genome)>`.
- Reuses `qe_wfo::{archive::Cell, mapelites, operator::ApplicationOutcome}`; `qe_determinism::DetRng`.
  No new dependencies.

## Test plan (TDD)

1. **Leakage test (AC).** Two niches; cell A is recorded in-training-productive (`ImprovedElite`/`NewCell`)
   while cell B is not; a validation score (defined in the test, **never** passed to the selector) favours
   B. Over many selections the bandit concentrates on **A** (in-training winner), proving validation plays
   no role.
2. **Reward is in-training.** `record` raises a niche's posterior mean only for `NewCell`/`ImprovedElite`;
   `NoImprovement` leaves it at the prior — the reward currency is the in-training outcome.
3. **Bayesian behaviour.** A high-reward niche is selected far more than a low-reward one; an unseen niche
   (prior optimism) is still reachable (exploration), and selection is deterministic for a fixed seed.
4. **Edge.** Empty archive → `None`; a direction with one occupied cell always returns it.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Reward scale vs `obs_var`.** `ImprovedElite{gain}` magnitudes set the effective signal-to-noise; the
  Normal model assumes a fixed `σ²`. Acceptable for relative niche ranking; a per-niche variance estimate
  is a later refinement. The *firewall* property (no OOS) is independent of this tuning.
- **Box–Muller uses `ln`/`cos` (`f64`).** Selection is stochastic, not a byte-reproducible artefact gene;
  determinism is via the seeded `DetRng` (same seed → same selections), which is what reproducibility
  needs here.
- **Standalone selector.** Wiring it into the search loop (replacing the uniform `sample_parent`) is
  QE-122+; QE-121 delivers and proves the selector in isolation.
