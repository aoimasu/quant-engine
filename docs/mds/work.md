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
- **Latest commit:** _(blocker-fix follow-up — see below)_
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

**Verdict: [Reviewed].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `ce34e6b`). **Both ACs pass and are well-tested** — the issue below is a non-AC correctness/safety
bug on the explicitly-flagged retry/rate-limit path that I won't wave through.

**What's correct (verified):**
- **AC #1 (freshness).** `Backfiller::backfill` pages forward (`cursor = newest + interval_ms`) until
  `newest >= now_ms - interval_ms`, with `now_ms` injected for determinism. Traced
  `paginates_to_within_one_interval_of_now` by hand: stops at 9min for now=10min/interval=1min →
  `latest >= now - interval`. ✓
- **AC #2 (overlap retained).** Cursor starts `from_ms - overlap_ms`; the final `partition` puts
  `open_time < from_ms` into `overlap`, the rest into `fresh`. Traced `retains_overlap_region_before_from`:
  overlap = {3min, 4min}, fresh ≥ 5min incl. 10min. ✓
- **Forward-progress guard is sound.** The `progressed` flag (a page yielding no row `> last_have`) plus
  the `> last_have` dedup correctly break the loop on duplicate/non-advancing pages and on an empty page —
  no infinite loop. Retry bounding (`attempts >= max_retries` before increment) gives exactly
  `1 + max_retries` calls; `Fatal` returns on the first call. All five retry/pagination tests trace clean.
- **Port/adapter + pure parse.** `RestSource` is the only seam; `parse_klines_json` is pure and correctly
  classifies every malformed shape as `Fatal` (non-retryable). URL golden strings match the real fapi
  layout (klines `interval=5m`, fundingRate no-interval, openInterestHist `period=1h` under
  `/futures/data`). `HttpRestSource` is correctly `#[cfg(feature = "http")]`. Topology unchanged (stays in
  `qe-ingest`; only new dep `serde_json`, already in-workspace).

**Verification caveat (transparency).** The Rust toolchain is absent from this review environment (no
`cargo`/`rustc`/`rustup`), so I did not execute the gates (incl. `--features http` clippy / `cargo deny`).
The verdict rests on full static review + hand-traced execution of every test. The blocker below is a
logic/spec mismatch visible purely in the source, independent of any gate.

### Feedback

1. **[BLOCKER] Rate-limit backoff is never applied — `retry_after_ms` is dead, and the live client will
   hammer a 429 (contradicts the design's "honours Retry-After").**
   `HttpRestSource::fetch_page` computes `retry_after_ms` from the `Retry-After` header and returns
   `RestError::RateLimited { retry_after_ms }` **without sleeping**. `Backfiller::fetch_with_retry` then
   matches `e @ (RestError::RateLimited { .. } | RestError::Transient(_))` — the `{ .. }` discards
   `retry_after_ms` — and **retries immediately with zero delay**. Confirmed by grep: the only readers of
   `retry_after_ms` are its `Display` impl and a test; nothing ever waits on it, and there is no
   `sleep`/backoff anywhere in the crate. Consequences:
   (a) The design note's claim *"Backoff sleeping lives in the real adapter (honours Retry-After)"* is
   **false** — the adapter classifies but does not honour it.
   (b) On a real HTTP 429, the live `http` client issues up to `max_retries` (default **5**) back-to-back
   requests with no delay, which Binance escalates to a 418 IP ban — a real operational bug in the one
   path that does live network I/O (and is not CI-covered, so review is the only gate).
   **Resolve either way:** (i) make it true — sleep `retry_after_ms` (capped) in the adapter before
   returning, or inject a `Sleeper`/clock into the `Backfiller` and sleep on `RateLimited` (keeps the core
   testable with a fake sleeper, preserving determinism); **or** (ii) if rate-limit *waiting* is genuinely
   deferred to QE-201's shared handler, correct the design note + the `RetryPolicy` doc to say so
   explicitly ("`retry_after_ms` is surfaced for QE-201's handler; no backoff yet") and reply with
   `{ANSWER}` justifying the deferral — I'll evaluate it objectively. As written, the code and the stated
   design disagree, and the live behaviour is harmful.

2. **[Non-blocking] `PageRequest.symbol` / `BackfillRequest.symbol` are unvalidated `String`s** placed
   directly into the URL without encoding. Upstream callers pass validated `InstrumentId`s, but the type
   doesn't enforce it — consider taking `&InstrumentId` (as `source.rs`/`plan.rs` do) so the URL-safety
   invariant is carried by the type rather than by convention. Not a blocker.

### Post-review follow-up (coder) — BLOCKER fixed; status → [Ready-for-review]

Agreed with the blocker and both notes — fixed.
- **[BLOCKER] Rate-limit backoff now actually applied — DONE.** `retry_after_ms` is no longer dead.
  Introduced a `Sleeper` **port** (`RealSleeper` = `thread::sleep` in production; a recording/no-op
  fake in tests). `Backfiller::fetch_with_retry` now **waits before each retry**: the venue's
  `Retry-After` (floored at `RetryPolicy.base_delay_ms`) for a `RateLimited`, else a linear
  `base_delay_ms × attempt` for a `Transient` — so a 429 is never hammered (no 418 IP-ban risk).
  `RetryPolicy` gained `base_delay_ms`; `Backfiller<S, Sl = RealSleeper>` with `new` (real) /
  `with_sleeper` (injected). New assertions in `retries_rate_limit_then_succeeds_and_waits_retry_after`
  prove `waits[0] == 2000` (Retry-After honoured) and the transient linear backoff; `fatal_error_is_
  not_retried` now also asserts **zero** waits. The design note's "honours Retry-After" claim is now
  true (and the doc updated to describe the `Sleeper` port).
- **[non-blocking] `symbol` typed as `&InstrumentId` — DONE.** `PageRequest.symbol` and
  `BackfillRequest.symbol` are now validated `InstrumentId`s (URL uses `.as_str()`), not raw `String`s.
- Gates re-run green: fmt ok; clippy clean (default **and** `--features http`); `qe-ingest` 33 unit +
  2 integration; deny unaffected.

{ANSWER} On the blocker's "defer to QE-201?" option — I chose to **implement the wait now** rather
than defer, because the design already claimed "honours Retry-After" and shipping the live `http`
client without it is a real ban risk. The `Sleeper` port keeps it testable and is exactly the seam
QE-201's shared handler will adopt (it can swap in a smarter backoff/`Sleeper`), so this is forward-
compatible, not throwaway.
