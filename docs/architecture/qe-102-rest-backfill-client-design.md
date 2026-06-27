# QE-102 — Venue REST month-to-date backfill client

`Phase: P1` · `Area: ① External sources` · `Depends on: QE-101`

## Goal

Close the gap between the vendor's latest published dump (QE-101) and **now**, so the training
corpus's right edge never drifts stale. Fetch Binance REST klines / mark-price / premium-index /
funding / `/futures/data` over the vendor-to-now window — **paginated, retried, rate-limit-aware** —
and **retain the vendor↔REST overlap region** for reconciliation (QE-103).

## Current state (evidence)

- QE-101 (`qe-ingest`) established the **port/adapter** pattern: a `Fetcher` trait tested offline
  with fakes, the real `ureq` client behind a default-off `http` feature, `cargo deny` green via
  native-tls. QE-102 follows the same shape with a `RestSource` port.
- QE-101's `source.rs`/`plan.rs` give the dump layout + point-in-time enumeration; QE-102 picks up at
  the **right edge** — the most recent period the dumps cover — and fills forward to now.
- `qe-domain` `Resolution`/`Timestamp` and `qe-config` are already dependencies. The shared
  rate-limit handler is formalised later (QE-201); QE-102 ships a minimal, pluggable retry policy
  that QE-201 will subsume.

## Design

Two modules added to `qe-ingest`:

### `rest.rs` — the REST port + endpoint layout

- `RestEndpoint` — `Klines{interval}`, `ContinuousKlines{interval}`, `MarkPriceKlines{interval}`,
  `PremiumIndexKlines{interval}`, `FundingRate`, `OpenInterestHist{period}` (`/futures/data`). Each
  builds its Binance `fapi`/`futures/data` path + query (`symbol`, `interval`/`period`, `startTime`,
  `limit`) — tested as golden URL strings.
- `RestError` — `RateLimited { retry_after_ms }` (HTTP 429/418), `Transient(String)` (5xx, network),
  `Fatal(String)` (4xx other than rate-limit, parse). Drives the retry policy.
- `trait RestSource { fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError>; }`
  — the single network seam. `PageRequest { endpoint, symbol, start_ms, limit }`; `TimedRow {
  open_time_ms, raw }` (the raw JSON row retained for fusion/diffing).
- `parse_klines_json(bytes) -> Result<Vec<TimedRow>, RestError>` — a **pure**, tested helper that
  extracts `open_time` (element 0) from Binance's `[[openTime, open, …], …]` array form and keeps the
  raw row. Used by the real adapter so the JSON→row extraction is covered offline.
- `HttpRestSource` (`#[cfg(feature = "http")]`) — the real `ureq` client: builds the URL, GETs,
  maps 429/418→`RateLimited` (honouring `Retry-After`), 5xx→`Transient`, parses via
  `parse_klines_json`.

### `backfill.rs` — the paginated, retried backfiller

- `RetryPolicy { max_retries }` — bounded retries on `RateLimited`/`Transient`; `Fatal` is not
  retried. (Backoff *sleeping* belongs to the real adapter / QE-201's handler; the core just bounds
  attempts, so tests stay deterministic and fast.)
- `Backfiller<S: RestSource>` — `backfill(req_template, interval_ms, from_ms, now_ms, overlap_ms) ->
  Result<BackfillResult, IngestError>`:
  1. start the cursor at `from_ms - overlap_ms` so the first pages re-cover the vendor's tail (the
     **overlap region**);
  2. loop: fetch a page (start = cursor, limit = L) through the retry policy; append rows; advance
     the cursor to `last_open_time + interval_ms`; stop when a page is empty or the last row is
     within one `interval_ms` of `now_ms`;
  3. partition rows into `overlap` (`open_time < from_ms`) and `fresh` (`>= from_ms`).
- `BackfillResult { fresh, overlap, latest_open_time_ms }` — `fresh` extends the corpus to now;
  `overlap` is the vendor↔REST diffing region (QE-103); `latest_open_time_ms` proves the right edge.

### Why this shape

- **AC #1 (latest bar within one interval of now):** the loop pages until the newest row is within
  one `interval_ms` of `now_ms`; `latest_open_time_ms >= now_ms - interval_ms` is asserted directly.
  `now_ms` is passed in (binary supplies the clock; tests pin it) for determinism.
- **AC #2 (overlap retained for diffing):** the cursor deliberately starts `overlap_ms` *before* the
  vendor's right edge, and rows with `open_time < from_ms` are returned separately as `overlap` —
  never discarded.
- **Paginated / retried / rate-limit-aware:** cursor pagination by `last_open_time + interval`; the
  retry policy bounds attempts on `RateLimited`/`Transient` and surfaces `Fatal` immediately —
  exactly the seam QE-201's shared handler will formalise.
- **Offline-testable:** all logic runs against the `RestSource` port with a scripted fake (chunked
  pages, an injected rate-limit-then-success, an always-fail); `parse_klines_json` is pure. The real
  `ureq` adapter is behind `http`, compiled + clippy-checked locally, never in CI's default build.

## Test plan (TDD)

- **`rest`** — golden URL strings for each endpoint (`klines`, `markPriceKlines`,
  `premiumIndexKlines`, `fundingRate`, `openInterestHist`); `parse_klines_json` over a sample Binance
  array (open-time extraction + raw retention) and rejection of malformed JSON.
- **`backfill`**:
  - **pagination** — a fake serving 3 chunked pages then empty assembles all rows in order;
  - **AC #1** — backfill reaches `latest_open_time_ms >= now - interval`;
  - **AC #2** — with `overlap_ms = 2 intervals`, rows before `from_ms` are returned in `overlap`, the
    rest in `fresh`;
  - **retry** — a source that returns `RateLimited` then a page succeeds within `max_retries`; a
    `Transient`-forever source errors after exhausting retries; a `Fatal` is surfaced immediately
    (not retried).

## Risks / out of scope

- **Out of scope:** live streaming (QE-202); the *fusion* of fresh+overlap into LMDB and the actual
  reconciliation diff (QE-103/QE-104) — QE-102 produces the rows + the retained overlap.
- **Risk:** the live REST path can't run in CI (no network) — it is a thin `ureq` adapter behind
  `http`; the pagination/retry/window/overlap logic it feeds is fully tested via the fake. Noted.
- **Topology:** stays within `qe-ingest` (already `→ qe-config/qe-domain`); no new internal edge,
  QE-001 guard unaffected.
