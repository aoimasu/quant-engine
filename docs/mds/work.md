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
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.
- QE-012 — Instrument-universe config & point-in-time membership — PR #12 — Approved & merged.
- QE-013 — Local run & deployment-agnostic packaging — PR #13 — Approved & merged. **(P0 complete)**
- QE-101 — Binance public-dumps downloader — PR #14 — Approved & merged.
- QE-102 — Venue REST month-to-date backfill client — PR #15 — Approved & merged.

---

## QE-103 — Data-integrity & source reconciliation validation — PR #16 — [Ready-for-review]

- **Branch:** `qe-103/data-integrity-reconciliation`
- **PR:** _(set on `gh pr create`)_
- **Latest commit:** _(see PR head)_
- **Evidence/design:** `docs/architecture/qe-103-data-integrity-reconciliation-design.md`
- **Changed surface:** `crates/ingest` — **new** `src/{integrity,fill,coverage,reconcile,quality}.rs`,
  `src/lib.rs` (module wiring + exports), `Cargo.toml` (+`serde`). Pure logic, no network. Also
  bundles the QE-102 archive (`docs/mds/reviewed/qe-102.md`) + `docs/mds/work.md` bookkeeping —
  branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] No silent forward-fill across a gap larger than the configured bound.
- [x] A data-quality report is produced per vintage and fails the run on configured hard violations.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features http` — clean)
- `cargo test --workspace --locked` — `qe-ingest` 55 unit (+22: integrity 5, fill 5, coverage 4,
  reconcile 4, quality 4) + 2 integration; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (only new dep `serde`, workspace-provided)

Key AC-proving tests:
- **AC #1 (no fill across a big gap)** — `fill::tests::gap_larger_than_bound_is_not_filled`: with
  `max_gap = 2×interval`, only within-bound slots fill; the over-bound region is a **hole**, never
  filled across. `gap_exactly_at_bound_fills` pins the boundary; `leading_missing_run_is_a_hole`
  (nothing to carry forward → hole).
- **AC #2 (report + hard-fail)** — `quality::tests::gap_beyond_bound_is_a_hard_violation`,
  `duplicates_and_disorder_fail_when_disallowed`, `too_many_divergences_fail`: `evaluate(&policy)`
  returns the configured hard violations (run fails); `clean_report_passes_and_round_trips_json`
  proves a clean corpus passes and the report serialises to the per-vintage JSON artefact.
- **Supporting:** `integrity` gap/dup/out-of-order; `coverage` expected/present/missing +
  `flag_short_history` (funding/premium shorter history); `reconcile` value/missing divergence with
  abs+rel tolerance.

### Design notes for the reviewer
- **Leakage-safe fill is structural:** `plan_fill` walks the expected grid and can only fill while the
  consecutive-miss run stays `<= max_gap_ms`; beyond that the slots are emitted as `holes` and never
  filled — directly answering the reviewer's "silent NaN/forward-fill creates leakage" concern.
- **Report is the artefact + the gate:** `DataQualityReport` (serde `Serialize`) is the per-vintage
  JSON; `evaluate(&HardViolationPolicy)` turns configured violations (over-bound gap, forbidden
  duplicate/out-of-order, excess divergences) into a run-failing `Err`.
- **Pure + value-agnostic:** integrity/fill/coverage operate on the bare timestamp grid; reconcile
  uses `f64` tolerance (diagnostic, not money) — no new third-party deps, all offline-tested.
- **Out of scope:** the fusion **output format** / Arrow (QE-104); wiring the report into the run
  artefact is finalised with QE-104. **Topology:** stays within `qe-ingest`; QE-001 guard unaffected.

### Review notes

_(awaiting dedicated review agent — `start-review-ticket` against this branch/diff vs the ACs above)_
