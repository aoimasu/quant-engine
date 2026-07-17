# QE-445 — Permuted-parallelism (thread-count-independence) determinism test

`Phase: Review R2 (P2 — panel #16, unanimous)` · `Area: determinism` · `Depends on: QE-006`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-445`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [`docs/backlog.md`](../backlog.md) → Review R2.b
· Spec ref: maxdama §5.5 ("control 100% of state for reproducibility").
· **TEST-ONLY** — no production behaviour changes; no golden moves.

## 1. The gap this closes

The determinism contract (QE-006) rests on two designed-in properties:

- **Reproducibility** — the same lineage (seed + inputs) re-run twice emits the same bytes. This is
  enforced by [`reproduce`](../../crates/determinism/src/harness.rs) ("re-run twice and compare").
- **Scheduling-independence** — a stochastic *parallel* stage emits the **same** bytes **regardless of
  how many threads/cores run it**. This is the whole reason [`rng.rs`](../../crates/determinism/src/rng.rs)
  derives a *private* generator per unit of work via `task_rng(master, index)` instead of sharing one
  generator: a task's random stream depends only on its **index**, never on which thread happens to run
  it, so the draw order cannot depend on the scheduler.

`reproduce()` only exercises the **first** property: it re-runs the *same* closure under the *same*
conditions and never varies the thread count. The scheduling-independence property that `task_rng` is
*designed* for is therefore asserted only in the module doc-comment — a **design intent**, not an
**enforced invariant**. A regression that reintroduced a shared/thread-local RNG on a parallel stage
would still pass `reproduce()` (two same-thread-count runs still match), still pass every golden (goldens
are produced at one fixed thread count), and only manifest as a silent byte-divergence when the deploy
host's core count differs from the build host's — the worst possible time to discover it.

QE-445 turns the design intent into a CI-enforced invariant: **run a real stochastic parallel stage twice
under different rayon thread-pool sizes (1 vs N) and assert byte-identical artefacts.**

## 2. Which stage — a real MAP-Elites generation (not a toy, not the DE search)

The spec offers two candidate stages. Only one is genuinely parallel:

- **DE ensemble search** (`crates/ensemble/src/search.rs`, `run_de`) — **rejected**. It is driven by a
  *single* `DetRng` consumed **sequentially** (one `seed_rng(seed)`, drawn in a fixed loop); the crate
  does not even depend on `rayon`. Running it under different pool sizes is **vacuous**: it degenerates to
  the same serial computation whatever the pool, so it could never catch a scheduling-dependence bug. It
  would satisfy the letter of the AC while testing nothing.
- **MAP-Elites generation** (`crates/wfo/src/mapelites.rs`, `evaluate_batch` / `evaluate_and_insert`) —
  **chosen**. Batch evaluation is `genomes.par_iter().enumerate().map(|(index, g)| eval(g,
  &mut task_rng(master, index)))` — an *embarrassingly parallel* `rayon` map over a real production code
  path, each task seeded by exactly the `task_rng(master, index)` derivation QE-445 exists to protect. It
  is the real stochastic stage whose scheduling-independence the powerful search is trusted on.

The existing integration tests in `crates/determinism/tests/determinism.rs` already vary thread count, but
only over **toy inline stages** (`parallel_draw`, `parallel_float_sum`). QE-445 adds a test over the
**real MAP-Elites generation** — the artefact a vintage actually ships — so the enforced invariant now
covers production code, not a proxy.

### Wiring (test-only, dev-dependency)

The stage lives in `qe-wfo`; the harness lives in `qe-determinism`. The new test lives in
`crates/determinism/tests/permuted_parallelism.rs` and reaches the real stage via **dev-dependencies**
`qe-wfo` / `qe-signal` / `qe-domain` on `qe-determinism`. Consequences:

- **No production behaviour change.** Dev-dependencies compile only for tests/examples/benches; the
  `qe-determinism` library still has no parallel runtime and pulls in nothing new at build time.
- **The firewall is respected.** `crates/architecture` parses **only** `[dependencies]` /
  `[build-dependencies]` (see `firewall.rs` / `is_dep_path`); every `dev-dependencies` form is explicitly
  excluded. A dev edge `qe-determinism → qe-wfo` is invisible to `check_firewall`.
- **The dependency cycle is legal.** `qe-wfo → qe-determinism` is a normal edge; `qe-determinism →
  qe-wfo` is a **dev** edge. Cargo permits a cycle that passes through a dev-dependency (verified with
  `cargo metadata` full resolution: `FULL_RESOLVE_OK`), because dev-deps do not participate in the normal
  build graph.

## 3. How the thread-pool size is pinned

Rayon's global pool auto-sizes to the machine, so pinning is mandatory for a reproducible test. Each run
is executed inside an **explicitly-sized** pool:

```rust
let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build().unwrap();
pool.install(|| evaluate_and_insert(&mut archive, seed, genomes, eval));
```

`pool.install(f)` runs `f` (and every `par_iter` inside it) on **that** pool, so the same seeded stage is
evaluated once with `num_threads(1)` and once with `num_threads(N)` (N = 8). The two archives are then
serialised to a canonical byte artefact (§4) and compared for **byte-identity**. `reproduce()` is also
applied to the multi-threaded run to keep the original re-run-twice guarantee in the same test.

## 4. The artefact and why byte-identity MUST hold

The artefact is the **genome archive** produced by the generation, serialised canonically:

- iterate both direction archives (`Long`, `Short`) in fixed order;
- within each, walk **occupied cells in sorted `BTreeMap` order** (`DirectionArchive::occupied_cells`);
- for each cell emit its `Cell` descriptor and, for each stored `Elite` in stored order, the genome as
  canonical JSON (`Genome: Serialize`, the vintage-lineage form) followed by the fitness `f64` **bit
  pattern** (`to_bits().to_le_bytes()`).

Byte-identity across pool sizes must hold because **every** input to the archive is scheduling-invariant:

1. **Per-task RNG streams.** `evaluate_batch` seeds task `i` with `task_rng(master, i)` — a pure function
   of `(master, i)`. Task `i` draws the identical stream whether it runs on thread 0 of a 1-thread pool or
   thread 7 of an 8-thread pool.
2. **Index-aligned collection.** `IndexedParallelIterator::collect` returns results in **input index
   order**, so `fitness[i]` is task `i`'s result regardless of completion order.
3. **Order-deterministic insertion.** `evaluate_and_insert` inserts `(genome, fitness)` **sequentially**
   in genome order after the parallel map; Deep-Grid `consider` is a pure function of the arriving
   sequence, and ties (`worst_index`, strict-`>` displacement) break deterministically.
4. **No float reduction across tasks.** Fitnesses are stored, not summed across tasks, so there is no
   non-associative parallel `+` to reorder.

Because none of (1)–(4) reads the thread count, the serialised archive is a pure function of
`(master_seed, genomes, config)` — hence byte-identical across pools. If it ever differs, a
scheduling-dependent source of state has leaked in, which is exactly the regression this test exists to
catch.

## 5. Non-vacuity — the test proves something

Two independent guards ensure the assertion is not trivially true:

### 5.1 The stage genuinely runs in parallel (not degenerate-to-serial)

The chosen stage is a real `rayon::par_iter`, and the test **observes** concurrency at runtime: each task
increments a live-task `AtomicUsize` on entry and records the running maximum. Under the N-thread pool the
test asserts the observed **maximum concurrency `≥ 2`** — proving tasks genuinely executed *simultaneously*
on distinct threads, so the byte-identity result is earned under real parallelism, not a pool that quietly
ran everything on one worker. (A bounded cooperative wait plus real per-task work makes the overlap
reliable; under the 1-thread pool concurrency is 1, as expected, and is not asserted.)

### 5.2 The test FAILS if per-task seeding is replaced by a shared/thread-local RNG (mutation guard)

The eval is deliberately **rng-consuming** and genome-dependent (`fitness = f(genome) + draws(rng)`), so
the artefact is sensitive to *which* stream each genome sees. The guard then models the exact regression
QE-445 targets — swapping `task_rng(master, index)` for **one shared generator drawn in task-execution
order** — and shows that scheme is **order-sensitive**:

- a shared-RNG evaluator, run over the genomes in two different execution orders, produces **different**
  per-genome fitness artefacts (realigned to genome order) — i.e. the result depends on scheduling;
- the **per-task** evaluator, run over the **same** two orders, produces **identical** artefacts.

Since a real thread pool *permutes* task-execution order relative to a single worker, the shared-RNG scheme
would make the archive thread-count-dependent — the byte-identity assertion in §3 would fail — whereas the
production `task_rng` scheme keeps it invariant. This is a mutation-style proof that the assertion is
load-bearing on the per-task seed derivation, not on some incidental determinism.

## 6. No golden moves — test-only diff

This ticket adds **only**:

- `docs/architecture/qe-445-permuted-parallelism-determinism-design.md` (this note);
- `crates/determinism/tests/permuted_parallelism.rs` (the new test);
- three **dev-dependency** lines in `crates/determinism/Cargo.toml` (`qe-wfo`, `qe-signal`, `qe-domain`).

No production source is touched, no golden constant or vintage artefact is created or moved, and no
`content_hash` changes. `git diff --stat` against `main` is limited to the three items above. The full
green gate (fmt / clippy `-D warnings` on both feature sets / test / deny / architecture firewall) is the
sole gate and must pass on the exact commit.
