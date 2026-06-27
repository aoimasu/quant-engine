# QE-112 — SPIKE: Adaptive operator selection — design / decision record

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-110` · **Blocks: QE-119**
`Branch: qe-112/adaptive-operator-selection`

## Goal (from backlog)

Operators (local refine → aggressive explore → fresh random) compete for budget; the
credit-assignment scheme must not leak out-of-sample performance.

**Scope / requirements.**
- Define the operator set and a bandit/credit-assignment scheme (multi-emitter MAP-Elites, Colas 2020),
  favouring exploration when sparse, exploitation when dense.
- Credit signal = in-training improvement / archive novelty — **never** OOS/validation reward.

**Acceptance criteria.**
- [ ] Decision record fixes operators and credit signal.
- [ ] A simulated sparse archive shows the scheme shifting budget toward exploration.

**Out of scope.** Parent selection (QE-121); the actual genome variation operators (QE-119); archive
insertion (QE-118).

## Current-state evidence

- **QE-110** names the operator family the genome supports: `repair`-backed free mutation (local
  refine / aggressive explore) and fresh-random construction. QE-112 fixes *which* operators compete
  and *how budget is assigned*; QE-119 implements their genome-level variation.
- **QE-111** gives the archive the credit signal reads: a genome occupies a [`Cell`]; an operator's
  offspring either **fills a new cell** (novelty) or **improves an elite** in an occupied cell
  (in-training quality) — both genotype/in-training, never OOS.
- **QE-006** (`qe_determinism::DetRng`, ChaCha8) is the seeded RNG every stochastic stage draws from;
  operator selection must be reproducible through it (no `ThreadRng`, no hash-randomised iteration).

## Decision

### D1 — Operator set (three emitters)

| Operator | Role | Character |
|----------|------|-----------|
| `LocalRefine` | exploit | small genome perturbation around a parent (fine-tune an elite) |
| `Explore` | explore | aggressive multi-locus mutation (jump to a new niche) |
| `FreshRandom` | explore (max) | a brand-new random genome (no parent; escape the current basin) |

`LocalRefine` is **exploitative**; `Explore` and `FreshRandom` are **exploratory**
(`Operator::is_exploratory`). This is exactly the spec's "local refine → aggressive explore → fresh
random" progression, modelled as competing emitters (Colas 2020 multi-emitter MAP-Elites). Their
genome-level mechanics are QE-119.

### D2 — Credit-assignment: sliding-window reward bandit with an exploration floor

A multi-armed bandit over the three operators (each an *emitter arm*), Colas-2020 style:

- **Reward of one application** = the in-training effect of its offspring on the archive
  ([`ApplicationOutcome`]):
  - `NewCell` ⇒ `NOVELTY_REWARD` (= 1.0) — filled a previously-empty niche (archive novelty);
  - `ImprovedElite { gain }` ⇒ `gain` (≥ 0) — raised an occupied cell's elite (in-training fitness
    improvement, normalised);
  - `NoImprovement` ⇒ 0.
- **Credit** of an operator = the **mean of its last `WINDOW` rewards** (a sliding window, so the bandit
  tracks each operator's *current* productivity, not its lifetime average — essential because operator
  value is non-stationary as the archive fills).
- **Selection probability** ∝ `max(credit, 0) + EPSILON`, normalised over the three operators. The
  `EPSILON` floor (= 0.05) guarantees every operator keeps a minimum share, so a temporarily-unlucky
  operator is always re-sampled and can recover (adaptive-pursuit-style minimum exploration). At cold
  start (no rewards) all credits are 0 ⇒ probabilities are uniform.
- **Deterministic**: `select(&mut DetRng)` samples the categorical via one RNG draw, so the whole
  search is byte-reproducible (QE-006).

### D3 — Exploration-when-sparse / exploitation-when-dense is *emergent*, not hard-coded

The scheme is **not** told the archive density. The behaviour falls out of the reward:

- **Sparse archive** — most niches empty, so exploratory operators (`Explore`, `FreshRandom`)
  frequently return `NewCell` (reward 1.0) while `LocalRefine` mostly refines within the few occupied
  cells. Exploratory credit rises ⇒ the bandit shifts budget to exploration.
- **Dense archive** — new cells are rare, so exploratory operators mostly return `NoImprovement` while
  `LocalRefine` keeps returning `ImprovedElite`. Exploitative credit rises ⇒ budget shifts to
  exploitation.

The sliding window makes the shift responsive as density changes across a run. This is the AC's
"shifting budget toward exploration" on a sparse archive — demonstrated by simulation (test plan #3).

### D4 — Information firewall: credit is in-training only (no OOS)

[`ApplicationOutcome`] has **no field** for validation / holdout / live performance — by construction
the bandit *cannot* be fed an OOS reward, only archive novelty or in-training fitness gain. This is the
QE-112 slice of the firewall (the spec's "credit signal = in-training improvement / archive novelty —
never OOS"); QE-121 enforces the same for parent selection and QE-132 makes it a CI guard. Documented
and structurally enforced here.

## Module / API plan

New module `crates/wfo/src/operator.rs`, re-exported from `qe-wfo`:

- `Operator { LocalRefine, Explore, FreshRandom }`; `OPERATORS`; `Operator::{index, is_exploratory}`.
- `ApplicationOutcome { NewCell, ImprovedElite { gain: f64 }, NoImprovement }`; `reward()`.
- `OperatorSelector` — sliding-window credit bandit:
  - `new(window, epsilon)` / `with_defaults()` (WINDOW = 64, EPSILON = 0.05, NOVELTY_REWARD = 1.0);
  - `record(Operator, &ApplicationOutcome)`;
  - `credit(Operator) -> f64`, `probabilities() -> [f64; 3]`;
  - `select(&mut R: RngCore) -> Operator` (deterministic categorical draw).
- `Cargo.toml`: add `rand_core` (the `RngCore` bound) and `qe-determinism` (dev-dep, seeded `DetRng`
  for the simulation tests).

## Test plan (TDD)

1. **Cold start uniform.** No rewards ⇒ `probabilities()` are all `1/3`; `select` over a seeded RNG is
   deterministic and reproducible.
2. **Credit tracks reward.** `record` of `NewCell` / `ImprovedElite{gain}` / `NoImprovement` moves an
   operator's `credit` as specified; the sliding window forgets old rewards beyond `WINDOW`.
3. **Sparse archive shifts to exploration (AC).** A seeded simulation: each round `select` an operator,
   an archive-response model returns `NewCell` for exploratory ops / `NoImprovement` for `LocalRefine`
   (sparse), `record`. After N rounds assert `P(Explore) + P(FreshRandom)` rose well above the uniform
   `2/3` and `P(Explore) > P(LocalRefine)`.
4. **Dense archive shifts to exploitation.** Mirror model (`LocalRefine`→`ImprovedElite`, exploratory→
   `NoImprovement`) ⇒ `P(LocalRefine) > P(Explore)`. Same code, opposite regime — shows the shift is
   data-driven, not hard-coded.
5. **Epsilon floor.** Even a zero-credit operator keeps probability ≥ `EPSILON / (sum)` > 0 (never
   starves); a starved-then-rewarded operator recovers.
6. **No-OOS shape.** `ApplicationOutcome` exposes only novelty / in-training gain (compile-time: there
   is no OOS constructor/field) — a guard test documenting the firewall.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Reward scale coupling.** `ImprovedElite{gain}` competes with `NOVELTY_REWARD = 1.0`, so the
  in-training fitness gain must be **normalised** to a comparable scale before it is recorded
  (QE-118/120's responsibility); otherwise one term dominates. Flagged as the integration contract.
- **Window / epsilon are pre-data constants.** `WINDOW = 64`, `EPSILON = 0.05` are seeded from the
  Colas-2020 regime and made constructor params; tune against real archive-fill curves (config via
  QE-002 when the search loop lands in QE-119).
- **Stationarity assumption.** A sliding-window mean assumes density changes slower than `WINDOW`
  applications; pathological fast oscillation could lag. Acceptable for the P1 search; revisit if the
  fill curve is jumpy.
- **Greedy-collapse guard.** Without the epsilon floor a lucky early operator could monopolise budget;
  `EPSILON` is precisely the guard, and test #5 pins it.
