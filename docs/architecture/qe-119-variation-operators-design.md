# QE-119 — Variation operators + adaptive selection — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-112, QE-118`
`Branch: qe-119/variation-operators`

## Goal (from backlog)

Operators generate offspring; adaptive selection allocates budget by productivity.

- Implement the operator set (local refine, explore, fresh random) over the genome (QE-110) and the
  credit-assignment scheme (QE-112).

**Acceptance criteria.**
- [ ] Operator budget shifts toward exploration on a sparse archive and exploitation on a dense one
  (matches QE-112 design).

**Out of scope.** Backtest evaluation (QE-120) — the driver takes a caller-supplied scalar `eval`.

## Current-state evidence

- **QE-112** (`qe_wfo::operator`) defined the *vocabulary* but not the mechanics: `Operator
  {LocalRefine, Explore, FreshRandom}`, the credit-proportional `OperatorSelector` (sliding-window
  reward bandit; `probabilities()` = `max(credit,0)+ε` normalised; `select`/`record` seeded), and
  `ApplicationOutcome {NewCell, ImprovedElite{gain}, NoImprovement}` with `reward()`. The doc-comment on
  `Operator` literally says "Genome-level mechanics are QE-119." This ticket supplies them.
- **QE-118** (`qe_wfo::mapelites`) gives the archive the offspring are inserted into and the
  `InsertOutcome {NewCell, Added, ImprovedElite, Rejected}` the driver maps to credit; `sample_parent`
  is the niche parent source.
- **QE-110/111** `Genome`/`repair` (mutate-freely-then-repair) and `descriptor_for` (the genotype-derived
  `Cell` — family × timescale × holding) define what "stays in the same niche" vs "jumps niche" means.

## Design

### D1 — The three operators (mutate-freely-then-repair)

All operators return a repaired genome (`Genome::repair`, QE-110). Crucially, they differ in **whether
they preserve the descriptor cell**, which is what makes the adaptive budget shift emerge (D3):

- **`local_refine(parent, rng, schema)` — exploitation, cell-preserving.** Nudges only the *numeric*
  genes: each bank's enabled-clause `[lo,hi]` bounds by ±1 and `size_bps` by ±`LOCAL_SIZE_STEP`. It does
  **not** change which clauses are enabled, their `feature`s, or `max_holding_bars` — so the descriptor
  (family from features, timescale from referenced lookbacks, holding from the holding cap) is unchanged.
  The offspring lands in the **same cell** as the parent: a local hill-climb of an elite.
- **`explore(parent, rng, schema)` — exploration, cell-changing.** Aggressive multi-locus mutation:
  re-points a clause's `feature` (changes family/timescale), forces it enabled, randomises its band, and
  resets `max_holding_bars` (changes the holding band). The offspring typically lands in a **different
  cell** — a jump to a new niche.
- **`fresh_random(rng, schema)` — maximal exploration, no parent.** A brand-new random genome (random
  banks with ≥ 1 enabled clause each so it has a descriptor, random exit/risk). Lands in a random cell.

### D2 — Credit assignment: `InsertOutcome` → `ApplicationOutcome`

The driver maps the QE-118 archive outcome (for the direction being evolved) to the QE-112 reward:

| `InsertOutcome` | `ApplicationOutcome` | reward |
|---|---|---|
| `NewCell` | `NewCell` | `NOVELTY_REWARD` (1.0) |
| `ImprovedElite` | `ImprovedElite { gain }` | `gain ≥ 0` |
| `Added` | `NoImprovement` | 0 |
| `Rejected` | `NoImprovement` | 0 |

`Added` (a sample joining a non-full Deep-Grid cell) earns **no** credit: it neither expands the niche
frontier (novelty) nor improves an elite (quality) — only those two are productive in MAP-Elites credit
terms. `gain` is the **normalised** displaced improvement `(f_offspring − worst_displaced)/|worst_displaced|`
(the archive's worst-of-cell before insertion), keeping it on a novelty-comparable scale per the QE-112
`ImprovedElite` contract.

### D3 — Why the budget shifts (AC)

The shift is **emergent** from D1 + D2, not hard-coded — exactly the QE-112 design intent:

- **Sparse archive** (many empty cells): the cell-changing exploratory operators (`explore`,
  `fresh_random`) frequently land in **empty** cells → `NewCell` → reward 1.0. `local_refine` stays in
  the parent's cell which has room → `Added` → reward 0. So exploration out-earns exploitation and the
  credit-proportional selector shifts budget toward `Explore`/`FreshRandom`.
- **Dense archive** (cells full of near-optimal elites): exploratory jumps land in **full** cells they
  cannot beat → `Rejected` → reward 0, while `local_refine` reliably finds a small improvement of the
  elite it started from → `ImprovedElite` → positive reward. Budget shifts toward `LocalRefine`.

### D4 — Driver

`VariationDriver { selector, direction }` ties it together. `step(archive, schema, rng, eval)`:
select operator → sample a parent elite (none for `FreshRandom`, or on a cold/empty archive →
fall back to `fresh_random`) → apply → evaluate (`eval: Fn(&Genome) -> f64`) → compute the pre-insert
worst-of-cell (for `gain`) → `archive.insert` → map the direction's outcome to an `ApplicationOutcome`
→ `selector.record`. Deterministic through a seeded `DetRng` (QE-006). Returns a `StepReport
{ operator, insert_outcome, application }`.

Two small **additive** accessors are added to `qe_wfo::mapelites` (QE-118): `SubPopulation::worst` and
`MapElitesArchive::sample_parent_elite` (parent genome + its fitness, for the `gain`). No behaviour change.

## Module / API plan

New module `crates/wfo/src/variation.rs`, re-exported:

- `local_refine`, `explore`, `fresh_random` (`<R: RngCore>`); `LOCAL_SIZE_STEP`.
- `VariationDriver::{new, selector, direction, step}`; `StepReport`.
- Additive in `mapelites.rs`: `SubPopulation::worst`, `MapElitesArchive::sample_parent_elite`.
- No new dependencies (`rand_core`, `qe-determinism` already present).

## Test plan (TDD)

1. **Operators repair to validity** and have the intended descriptor effect: `local_refine` preserves the
   parent's `Cell`; `explore`/`fresh_random` produce a valid genome with a descriptor.
2. **`InsertOutcome` → `ApplicationOutcome` mapping** (incl. `Added`→`NoImprovement`, `gain` sign).
3. **Sparse archive → exploration budget (AC).** A sparse archive driven for a short run leaves
   `P(Explore)+P(FreshRandom) > P(LocalRefine)` and `credit(FreshRandom) > credit(LocalRefine)`.
4. **Dense archive → exploitation budget (AC).** A dense archive (cells full of near-optimal elites)
   driven leaves `P(LocalRefine)` the maximum and `credit(LocalRefine) > credit(Explore)/credit(FreshRandom)`.
5. **Determinism.** Same seed → identical operator/outcome stream; different seed differs.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Test landscape realism.** The dense-archive AC uses a smooth single-optimum `eval` so local
  refinement reliably improves while random jumps usually cannot — a faithful stand-in for the QE-120
  backtest. Documented; the *mechanism* (credit-proportional selection) is metric-agnostic.
- **`Added` earns zero credit** is a deliberate decision (only novelty + elite-improvement are productive
  in MAP-Elites). Recorded so QE-124 can revisit if Deep-Grid sample density should earn partial credit.
- **`explore` cell-change is probabilistic** (a re-pointed feature *usually* changes family/timescale);
  the budget argument is statistical over a run, not per-step guaranteed.
