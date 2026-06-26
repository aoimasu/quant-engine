# QE-006 — Determinism & reproducibility harness — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-002`

**Goal.** Vintages must be auditable months later; reproducibility is the foundation of the
information firewall and of trustworthy validation.

**Acceptance criteria.**
- Two runs of the same stage with the same lineage produce byte-identical outputs **independent of
  core/thread count** (deterministic reductions + fixed task ordering).
- Every produced artefact carries a resolvable lineage record.

**Out of scope.** Storage of artefacts (QE-010/011/129).

## Current-state evidence

- `qe-config` (QE-002) already exposes `Config::content_hash()` (lowercase-hex SHA-256 over the
  resolved config's canonical JSON) and a `DeterminismConfig { seed: u64 }` field whose doc-comment
  already promises the seed is "plumbed through all stochastic stages (QE-006)". So the master seed
  and the config hash — two of the four lineage inputs — already exist.
- No stochastic stages exist yet (QD/DE/Thompson land in P1/P2). QE-006 therefore delivers the
  *primitives + harness* those stages will use, plus tests that prove the determinism contract on a
  representative parallel stage. It does not retrofit RNG into stages that aren't written.

## Design decisions

New crate `qe-determinism` (`crates/determinism`), three focused modules:

### `rng` — seedable, portable, per-task RNG
- `DetRng = rand_chacha::ChaCha8Rng`. ChaCha8 is fast, has no machine-dependent state, and produces
  identical streams on every platform — a precondition for *byte-identical* artefacts. (`std`'s
  `HashMap`/`ThreadRng`/float-hash randomness is explicitly avoided.)
- `seed_rng(seed)` seeds one generator; the run's master seed is `config.determinism.seed`.
- **The core trick for core-count independence:** parallel work must *not* share one RNG (draw
  order would then depend on scheduling). Instead `task_rng(master, index)` derives a private RNG
  per task from `(master, index)` via `derive_seed` (SplitMix64 mixing, so adjacent indices give
  well-separated streams). A task's stream depends only on its index — never on which thread ran it
  or in what order — so the same input set yields the same per-task draws at 1 thread or 64.

### `harness` — re-run and compare
- `reproduce(stage) -> Result<Vec<u8>, ReproError>` runs a `FnMut() -> Vec<u8>` twice and returns
  the bytes iff both runs are byte-identical; otherwise `ReproError` reports both lengths and the
  first differing offset. `is_reproducible(stage) -> bool` is the boolean convenience.
- This is the literal "re-run a stage twice and assert byte-identical artefacts" from the ticket,
  reusable from any stage's own test suite.

### `lineage` — resolvable vintage record (AC #2)
- `Lineage { config_hash, input_snapshot_id, code_commit, seeds }` — the four inputs that fully
  determine a stage's output. `Lineage::from_config(&Config, snapshot, commit, seeds)` folds in
  QE-002's `content_hash()` (the concrete QE-002 dependency).
- `Lineage::id()` is a stable lowercase-hex SHA-256 over the record's canonical JSON (fixed struct
  field order + ordered `seeds` ⇒ deterministic), usable as a primary key — i.e. *resolvable*.
- `Artifact<T> { value, lineage }` + `HasLineage` trait model "every produced artefact carries a
  resolvable lineage record": from any artefact you can reach its `lineage()` and resolve its id.

`qe-determinism` depends on `qe-config` (QE-002), `rand_chacha`/`rand_core` (RNG), `sha2`/`serde`/
`serde_json` (lineage hashing), `thiserror` (errors). `rayon` is a **dev-dependency only** — the
library stays free of a parallel runtime; rayon is used solely by the tests that exercise the
contract at 1 vs N threads. The stages that need parallelism (P1+) bring their own executor and use
`task_rng` for determinism.

## Test plan (proves both ACs)

Integration tests in `crates/determinism/tests/determinism.rs`:
1. `parallel_stage_is_byte_identical_across_thread_counts` — a toy stage draws one `u64` per task
   via `task_rng`, collected in fixed index order; output at `num_threads=1` equals `num_threads=8`
   (**AC #1, fixed task ordering**).
2. `deterministic_reduction_is_bit_stable_across_thread_counts` — parallel-computed `f64`s folded
   **sequentially in fixed index order**; the (non-associative) sum's bits are identical at 1 vs 16
   threads (**AC #1, deterministic reductions**).
3. `reproduce_returns_output_when_stable` / `reproduce_detects_nondeterminism` — the harness accepts
   a stable stage and rejects a counter-based non-deterministic one (first diff at offset 0).
4. `same_seed_same_stream` — two `seed_rng(s)` agree.
5. `lineage_id_is_stable_and_sensitive`, `lineage_from_config_uses_content_hash`,
   `artifact_carries_resolvable_lineage` — id is stable, seed-sensitive, ties to QE-002's hash, and
   is resolvable from an `Artifact` (**AC #2**).

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **New transitive deps** (`rayon`, `rand_chacha`, `ppv-lite86`, `zerocopy`, …): all MIT/Apache-2.0
  /BSD-2-Clause, already covered by `deny.toml`'s allowlist; validated with `cargo deny check`.
- **`collect()` ordering assumption:** the contract relies on rayon's `IndexedParallelIterator`
  preserving index order on `collect()` (documented behaviour). The reduction test additionally
  folds sequentially so it does not depend on reduce-tree shape.
- **Future stages must actually use `task_rng`**, not a shared RNG — enforced later by each stage's
  own reproduce-test; QE-006 supplies the primitive and the pattern.
