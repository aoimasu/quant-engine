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
