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

---

## QE-102 — Venue REST month-to-date backfill client — PR #15 — [Ready-for-review]

- **Branch:** `qe-102/rest-backfill-client`
- **PR:** https://github.com/aoimasu/quant-engine/pull/15
- **Latest commit:** `ce34e6b`
- **Evidence/design:** `docs/architecture/qe-102-rest-backfill-client-design.md`
- **Changed surface:** `crates/ingest` — **new** `src/rest.rs` (`RestEndpoint`/`PageRequest`/URL
  builder, `RestSource` port, `parse_klines_json`, `RestError`; real `HttpRestSource` behind `http`),
  **new** `src/backfill.rs` (`Backfiller`, `RetryPolicy`, `BackfillRequest`/`Result`); `src/lib.rs`
  (module wiring, exports, +`IngestError::Rest`); `Cargo.toml` (+`serde_json`). Also bundles the
  QE-101 archive (`docs/mds/reviewed/qe-101.md`) + `docs/mds/work.md` bookkeeping — branch protection
  blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] The fused corpus's latest bar is within one bar-interval of "now" at run time.
- [x] Vendor/REST overlap region is retained for diffing.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features http` — the ureq REST adapter — clean)
- `cargo test --workspace --locked` — `qe-ingest` 33 unit (+10: rest 5, backfill 5) + 2 integration;
  workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps beyond
  `serde_json`, already in the workspace)

Key AC-proving tests:
- **AC #1 (latest bar within one interval of now)** — `backfill::tests::paginates_to_within_one_
  interval_of_now`: pages a chunked dataset (limit = 2) until `latest_open_time_ms >= now - interval`.
- **AC #2 (overlap retained)** — `retains_overlap_region_before_from`: with `overlap_ms = 2×interval`,
  rows before `from_ms` come back in `overlap` (`[3m, 4m]`), the rest in `fresh`.
- **Pagination / retry / rate-limit** — cursor pagination by `last_open_time + interval`;
  `retries_rate_limit_then_succeeds` (RateLimited→Transient→ok within `max_retries`),
  `gives_up_after_exhausting_retries`, `fatal_error_is_not_retried` (one call, no retry).
- **`rest`** — golden URL strings (klines `interval=`, funding no-interval, OI `period=`);
  `parse_klines_json` over Binance array + `/futures/data` object forms; malformed-JSON rejection.

### Design notes for the reviewer
- **Same port/adapter pattern as QE-101:** all backfill logic runs against the `RestSource` trait
  (the only network seam), tested offline with a scripted fake; the real `HttpRestSource` (ureq +
  system TLS) is **behind the default-off `http` feature** — never in CI's default build. The JSON→row
  extraction (`parse_klines_json`) is a **pure, tested** function the adapter reuses.
- **AC #1:** `Backfiller::backfill` pages forward (cursor = `last_open_time + interval_ms`) until the
  newest row is within one `interval_ms` of `now_ms`; `now_ms` is passed in (binary supplies the
  clock; tests pin it) for determinism.
- **AC #2:** the cursor starts `overlap_ms` *before* the vendor right edge; rows with
  `open_time < from_ms` are partitioned into `overlap` (retained for QE-103), never discarded.
- **Rate-limit-aware retry:** `RetryPolicy` bounds attempts on `RateLimited`/`Transient` and surfaces
  `Fatal` immediately — the seam QE-201's shared handler will formalise. Backoff *sleeping* lives in
  the real adapter (honours `Retry-After`), so the core stays deterministic.
- **Out of scope:** live streaming (QE-202); fusion + the actual reconciliation diff (QE-103/QE-104)
  — QE-102 produces the rows + retained overlap. **Topology:** stays within `qe-ingest`; QE-001
  guard unaffected.

### Review notes

_(awaiting dedicated review agent — `start-review-ticket` against this branch/diff vs the ACs above)_
