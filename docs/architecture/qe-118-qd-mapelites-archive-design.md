# QE-118 — QD MAP-Elites archive implementation — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-111, QE-117`
`Branch: qe-118/qd-mapelites-archive`

## Goal (from backlog)

The archive maintains behavioural diversity **by construction** (per-direction, Deep-Grid
sub-populations).

- Implement per-direction archives with the descriptors/resolution from QE-111 and sub-populations for
  noise robustness; niche sampling for parents.
- Embarrassingly-parallel evaluation across cores (rayon/threads), preserving determinism.

**Acceptance criteria.**
- [ ] Archive fills distinct niches; sub-populations bound per-cell.
- [ ] Parallel runs remain deterministic under the seeded harness (QE-006).

**Out of scope.** Operator credit assignment (QE-119); persistence / quality gate (QE-123). The insert
path returns an outcome enum so QE-119 can read it, but does not itself assign credit.

## Current-state evidence

- **QE-111** (`qe_wfo::archive`) gives the niche substrate this ticket fills: `Cell` (family × timescale ×
  holding), `descriptor_for(genome, direction, schema)` (genotype-derived → window-stable),
  `grid_cells()`/`CELLS_PER_DIRECTION = 45`, and `SUBPOP_SIZE = 8` (the Deep-Grid bound). QE-118 does
  **not** redefine descriptors — it stores elites in those cells.
- **QE-006** (`qe_determinism`) gives the determinism contract: parallel work must never share one
  generator; each task derives its own stream from `(master, index)` via `task_rng`, so results are
  **independent of core/thread count**. That is exactly the lever for the parallel-evaluation AC.
- **QE-110** `Genome` is the stored artefact; `qe_domain::Direction { Long, Short }` selects the bank.
- **rayon** is already a workspace dependency (used by `qe_determinism`).

## Design

### D1 — Per-direction archives, Deep-Grid sub-populations (AC1)

`MapElitesArchive` holds two `DirectionArchive`s (Long, Short). A genome is placed into a direction's
archive at the `Cell` its *that-direction* bank descriptes (`descriptor_for`); it may occupy both, one,
or neither. Per-direction archives keep short niches first-class so the ensemble (QE-126) is not net-long
by construction (QE-111/D4).

Each `DirectionArchive` is a `BTreeMap<Cell, SubPopulation>` (sorted `Cell` order ⇒ deterministic
iteration). A `SubPopulation` is a `Vec<Elite>` **bounded to `SUBPOP_SIZE`** — the Deep-Grid noise-robust
cell (Flageat & Cully 2020): more than one elite so a single noisy evaluation cannot evict a genome,
small so the archive stays compact. `Elite { genome, fitness: f64 }` (fitness is a score, not money).

### D2 — Insertion / elite replacement

`SubPopulation::consider(elite)` →
- empty cell ⇒ insert, `NewCell`;
- room (`len < SUBPOP_SIZE`) ⇒ insert, `Added`;
- full ⇒ replace the **worst** (min-fitness, lowest-index tie-break) iff the candidate is *strictly*
  better, `ImprovedElite`; else `Rejected` (strict `>` avoids churn / keeps determinism).

`MapElitesArchive::insert(genome, fitness)` computes the Long and Short descriptors and returns an
`Insertion { long, short }` of `Option<InsertOutcome>` (None ⇒ no descriptor in that direction, not
stored there). The outcome enum is the hook QE-119 reads for credit; QE-118 assigns none.

### D3 — Niche sampling for parents

`sample_parent(direction, rng)` is Deep-Grid parent selection: pick a non-empty cell **uniformly** (not
proportional to occupancy — this is what preserves behavioural diversity, sparse niches reproduce as
often as crowded ones), then an elite **uniformly** within it. Deterministic: iteration is over the
sorted `BTreeMap`, indices drawn from a `RngCore`. `None` if the direction's archive is empty.

### D4 — Embarrassingly-parallel, deterministic evaluation (AC2)

`evaluate_batch(master_seed, genomes, eval)` maps each genome to its fitness across cores with
`rayon::par_iter`, **but each task seeds its own `DetRng` from `task_rng(master_seed, index)`** — the
stream depends only on the genome's index, never on which thread runs it (QE-006). `eval: Fn(&Genome,
&mut DetRng) -> f64 + Sync`. `par_iter().map(...).collect()` preserves input order, so the returned
`Vec<f64>` is index-aligned and **byte-identical regardless of the rayon pool size**. That is the whole
determinism lever: no shared RNG, no scheduling-order dependence, no hash-randomised collection.
`evaluate_and_insert` is the convenience that evaluates in parallel then inserts sequentially (insertion
is cheap and order-deterministic).

## Module / API plan

New module `crates/wfo/src/mapelites.rs`, re-exported:

- `Elite { genome: Genome, fitness: f64 }`.
- `InsertOutcome { NewCell, Added, ImprovedElite, Rejected }`; `Insertion { long, short: Option<InsertOutcome> }`.
- `SubPopulation` (bounded `Vec<Elite>`; `len`, `is_full`, `best`, `worst`, `consider`).
- `DirectionArchive` (`BTreeMap<Cell, SubPopulation>`; `occupied_cells`, `len`, `total_elites`).
- `MapElitesArchive::{new(schema), insert, sample_parent, direction, occupied_cells, total_elites}`.
- `evaluate_batch(master_seed, &[Genome], eval) -> Vec<f64>`; `evaluate_and_insert(...)`.
- New dep: `rayon` (workspace); `qe-determinism` promoted to a normal dependency (`task_rng`).

## Test plan (TDD)

1. **Fills distinct niches (AC1).** A spread of genomes spanning several families/timescales/holdings
   lands in distinct `Cell`s; `occupied_cells` matches the expected set; cross-direction placement works.
2. **Sub-populations bounded per cell (AC1).** Inserting > `SUBPOP_SIZE` genomes into one cell keeps
   `len == SUBPOP_SIZE`; the retained elites are the top-`SUBPOP_SIZE` by fitness; replacement returns
   `ImprovedElite`/`Rejected` correctly; strict-better semantics.
3. **Niche sampling.** Uniform-cell sampling reaches sparse cells; deterministic for a fixed seed;
   `None` on an empty archive.
4. **Parallel determinism (AC2).** `evaluate_batch` under a 1-thread rayon pool and an N-thread pool
   yields identical results; two runs with the same seed are byte-identical; a different seed differs.
5. **Edge.** No-descriptor genome inserts into neither direction; empty batch; insertion outcome enum.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Determinism under rayon.** The only safe pattern is per-task seeding + order-preserving collect;
  any shared RNG or `for_each` into a shared structure would re-introduce scheduling dependence. Pinned
  by the 1-vs-N-thread test.
- **`f64` fitness churn / tie-breaks.** Strict-better replacement + lowest-index worst tie-break keep
  insertion order-deterministic; documented. The real noise-robust fitness is QE-113/QE-120 — the
  archive is metric-agnostic (takes a scalar).
- **Uniform-cell parent sampling** trades exploitation for diversity by design (Deep-Grid); QE-119/QE-124
  may bias it later, but uniform is the diversity-preserving baseline.
