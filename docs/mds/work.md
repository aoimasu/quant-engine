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
- **PR:** https://github.com/aoimasu/quant-engine/pull/16
- **Latest commit:** _(post-approval advisory follow-up — see below)_
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

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `bd1b81b`) — read all five new modules and every test. Both ACs are met and **correct**; the crate
stays pure/offline.

**AC #1 — no silent fill across an over-bound gap (PASS).** The leakage-safety is **structural**:
`plan_fill` updates `last_present` **only on a genuinely present slot** (never on a fill), so the fill
distance `slot - src` is always measured from the last *real* sample and grows monotonically across a
miss-run. The first slot with `slot - src > max_gap_ms` falls into the hole branch, and because
`last_present` is unchanged every subsequent missing slot also exceeds the bound — a hole can only close
on a real present slot, so **no fill can ever occur inside an over-bound region**. I traced
`gap_larger_than_bound_is_not_filled` (filled = {1m,2m}, hole [3m,5m), nothing filled in the over-bound
region), the inclusive boundary (`gap_exactly_at_bound_fills`), `leading_missing_run_is_a_hole`, and the
`make_hole` accounting (`from` = first unfilled slot, `missing = (to-from)/interval`, no `-1`). All correct.

**AC #2 — report + hard-fail (PASS).** `DataQualityReport` is `Serialize` with `to_json` (the per-vintage
artefact); `evaluate(&HardViolationPolicy)` collects over-bound gaps (`span_ms() > max_gap_ms`, the strict
complement of fill's inclusive `<=`), forbidden duplicates/out-of-order, and excess divergences into a
`Vec<Violation>` — non-empty ⇒ `Err` ⇒ run fails. Defaults are sensibly conservative (structural
dup/disorder always fail; gap/divergence tolerances opt-in). All four tests trace clean, incl. the JSON
round-trip.

**Supporting modules (correct).** `integrity::check_series` computes gaps on the **sorted-unique** view so
a duplicate/out-of-order row can't fake a gap (verified `detects_duplicates`/`detects_out_of_order` show no
phantom gap), while dup/order are flagged on raw arrival order. `coverage` derives expected/present/missing
and flags strictly-shorter history (start-late/end-early). `reconcile::Tolerance::within` correctly encodes
"diverge iff it exceeds **both** abs and rel" (`within = diff<=abs || diff<=rel·max`), with value/missing
divergences over the union, ascending. Topology unchanged (only new dep `serde`, workspace-provided).

**Verification caveat (transparency).** The Rust toolchain is absent from this review environment (no
`cargo`/`rustc`/`rustup`), so I did not execute the gates. The verdict rests on full static review +
hand-traced execution of every test (this is pure, deterministic logic, which traces cleanly on paper). I
did not rely on the PR's "all green" claim; treat the reported gate results as developer-reported. Nothing
in the review contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. **`Gap` is reused by two producers with *different* `from_ms` semantics, and the field doc matches only
   one.** In `integrity.rs`, `Gap.from_ms` = the last **present** timestamp before the gap (matching the
   struct doc *"Last present timestamp before the gap"*). In `fill.rs::make_hole`, the same `Gap` is built
   with `from_ms` = the first **missing** (unfilled) slot — contradicting that doc. Both compute `missing`
   correctly and each module is internally consistent + tested, and crucially the two conventions never
   collide inside QE-103 (`evaluate` reads only `integrity.gaps`, never `fill.holes`). But `FillPlan.holes`
   is a serialized public type the **QE-104 fuser** will consume; a consumer trusting `Gap`'s documented
   `from_ms` would misread fill holes by one interval and with the wrong present/missing sense. Recommend
   **before QE-104 consumes holes**: either give fill its own `Hole` type, or unify the `from_ms` convention
   and update the `Gap` field docs to describe both producers. Latent trap, not a current bug.
2. **(Trivial) `duplicates` counts occurrences, not distinct values** — `[0,0,0]` yields `duplicates =
   [0,0]` so `len() == 2`. Fine for the diagnostic detail string; noted only for precision.

### Post-approval follow-up (coder) — advisories resolved; status → [Ready-for-review]

Resolved both non-blocking advisories (strictly additive; no AC behaviour changed).
- **#1 (shared `Gap` semantics trap before QE-104) — DONE.** `FillPlan.holes` is now a **distinct
  `Hole` type** (not the reused `integrity::Gap`). `Hole.from_ms` is documented as the first
  *unfilled* slot (inclusive) with `missing = (to-from)/interval`, vs `Gap.from_ms` = last present
  sample — so the QE-104 fuser consuming `holes` can't confuse the two conventions. Exported as
  `qe_ingest::Hole`.
- **#2 (duplicates counts occurrences) — DONE (doc).** `SeriesIntegrity.duplicates` now documents
  that it lists each duplicate *occurrence* (a thrice-seen timestamp appears twice).
- Gates re-run green: fmt ok; clippy clean (default **and** `--features http`); `qe-ingest` 55 unit +
  2 integration; deny unaffected.
