# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.

---

## QE-006 — Determinism & reproducibility harness — PR #6 — [Ready-for-review]

- **Branch:** `qe-006/determinism-harness`
- **PR:** https://github.com/aoimasu/quant-engine/pull/6
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-006-determinism-harness-design.md`
- **Changed surface:** new crate `crates/determinism` (`src/{lib,rng,harness,lineage}.rs`,
  `tests/determinism.rs`, `Cargo.toml`); root `Cargo.toml` (+`rand_core`/`rand_chacha`/`rayon`
  workspace deps, +`qe-determinism` path dep). Also bundles QE-005 archive
  (`docs/mds/reviewed/qe-005.md`) — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Two runs of the same stage with the same lineage produce byte-identical outputs
  **independent of core/thread count** (deterministic reductions + fixed task ordering).
- [x] Every produced artefact carries a resolvable lineage record.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 13 determinism tests pass (10 unit + 3 integration)
- `cargo deny check` — advisories/bans/licenses/sources ok

Key AC-proving tests (`crates/determinism/tests/determinism.rs`):
`parallel_stage_is_byte_identical_across_thread_counts` (1 vs 8 threads),
`deterministic_reduction_is_bit_stable_across_thread_counts` (fixed-order float sum, 1 vs 16),
plus `lineage`/`artifact` unit tests (stable+seed-sensitive id, `from_config` ties to QE-002,
resolvable from `Artifact`).

### Review notes

**Verdict: [Approved]** — both acceptance criteria genuinely met and independently verified,
including an adversarial check that the AC #1 tests actually discriminate. Clean, focused crate; the
RNG/harness/lineage split is well-judged and the primitives are sound for a "stages don't exist yet"
ticket.

**Independent re-verification (branch `qe-006/determinism-harness`):**
- `cargo fmt --all --check` clean · `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean · `cargo test --workspace --locked` **51 passed, 1 ignored** (qe-determinism: 13) ·
  `cargo deny check` → advisories/bans/licenses/sources **ok** (the new `rand_chacha`/`rand_core`/
  `rayon` + transitives all pass the licence/advisory audit). QE-001 `dependency_topology` guard still
  green — `qe-determinism → qe-config` adds no forbidden runtime↔training edge.

- **AC #1 — proven, and the tests genuinely discriminate (not trivial).** `task_rng(master, index)`
  derives a private RNG per task from `(master, index)`, so a task's stream depends only on its index,
  never on scheduling. The two integration tests compare real rayon pools at 1-vs-8 and 1-vs-16
  threads (byte-identical parallel draw; bit-stable fixed-order `f64` reduction), with `to_le_bytes`
  for endianness-independent output. To confirm these aren't vacuous I wrote a throwaway probe using
  the *shared-RNG anti-pattern* (`Mutex<DetRng>` drawn inside `into_par_iter`): it **diverges** across
  thread counts (`1==8 → false`). So the suite passes precisely because `task_rng` removes the
  schedule dependence, and would fail for the real failure mode. Removed the probe; tree clean.

- **AC #2 — satisfied at the primitive level.** `Lineage { config_hash, input_snapshot_id,
  code_commit, seeds }` is exactly the backlog's four-input record; `from_config` folds QE-002's
  `content_hash()` (tested); `id()` is a stable, every-field-sensitive SHA-256 over canonical JSON
  (fixed field order + ordered `seeds` Vec ⇒ byte-stable, machine-independent) → a resolvable primary
  key. `Artifact<T>` + `HasLineage` make "every produced artefact carries a resolvable lineage record"
  a type-level property. (Forcing future stages to wrap outputs in `Artifact` is per-stage, like
  `task_rng` — correct scope for a primitives ticket.)

- **Soundness of the three points raised:**
  - *SplitMix64 derivation* `splitmix64(master ^ splitmix64(index))` uses the correct Vigna constants
    and is bijective in `index` for a fixed `master` → no task-seed collisions; adjacent indices are
    decorrelated (tested), and `ChaCha8Rng::seed_from_u64` further diffuses. Sound.
  - *`collect()` index order* relies on rayon's `IndexedParallelIterator` order-preservation
    (documented) — the right tool; the reduction test additionally folds **sequentially** over the
    collected Vec, so it doesn't depend on reduce-tree shape. Sound.
  - *dev-only `rayon`* keeps the library executor-agnostic (no parallel runtime imposed on consumers);
    rayon appears only in the tests that exercise 1-vs-N. Appropriate.

**Non-blocking advisory notes (no action required):**
1. The AC #1 tests prove *same-machine* N-thread equality but don't pin the stream against drift with
   a **golden value** (a hard-coded expected byte/hash). A golden assertion would additionally catch
   an accidental change to the ChaCha8 stream or the SplitMix64 derivation (e.g. a dep bump) and
   document the cross-machine expectation the design relies on. Worth adding when convenient.
2. Cosmetic: the `hex(&[u8])` helper duplicates the equivalent in `qe-config`; fold into a shared util
   crate eventually rather than re-implementing per crate.

### Post-approval follow-up (coder)

Both advisories addressed; status set back to `[Ready-for-review]` for one confirmation pass (commit
`1987e20`).

- **Advisory #1 (golden value) — DONE.** Added platform-independent golden assertions:
  `rng::tests::golden_stream_and_derivation_are_pinned` pins `seed_rng(0).next_u64()`,
  `derive_seed(0,0)`, and `task_rng(0,0).next_u64()`; the integration test now also asserts a hard
  SHA-256 (`94d840f3…a038`) of the 4096-task parallel draw. A `rand_chacha` bump or any change to the
  SplitMix64 derivation now fails loudly instead of silently re-baselining vintages. Gates re-run
  green (fmt/clippy/`cargo test -p qe-determinism` 11 unit + 3 integration/deny).
- {ANSWER} **Advisory #2 (shared `hex` util) — deferred, not done in this PR.** Folding `hex` into a
  shared crate means introducing/locating a `qe-util` (or putting it in `qe-domain`) and rewiring
  `qe-config` too — out of scope for a determinism ticket and it would touch an already-merged crate.
  Tracking as a small follow-up; the duplication is two trivial lines and harmless meanwhile.
