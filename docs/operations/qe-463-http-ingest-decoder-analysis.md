# QE-463 — Real `http` ingest decoder for Binance USDT-M — evidence note

> Written **before** coding, per the `work-on-tickets` skill. Authoritative requirements:
> `docs/mds/tickets/QE-463.md`; design ref `docs/architecture/qe-455-research-flow-design.md` §8.1
> (the decoder) and §8.5 (scope honesty — the long pole).

## 1. Current state (real file:line)

### The `http` feature + the byte-transport / REST seams
- `crates/ingest/Cargo.toml`: `http = ["dep:ureq"]`, `ureq = { version="2", default-features=false,
  features=["native-tls"], optional=true }` — a **native-tls (system OpenSSL)** transport, no rustls. The
  crate defaults to `default = []` (offline).
- `crates/ingest/src/fetcher.rs:37` — `HttpFetcher` (the `Fetcher` byte-transport port) is `#[cfg(feature
  = "http")]`; the trait `Fetcher` (`:28`) and an in-memory `FakeFetcher` (tests) are always built.
- `crates/ingest/src/rest.rs` — the **Binance USDT-M REST port already exists** (QE-102):
  - `RestEndpoint` (`:17`): `Klines(Resolution)`, `MarkPriceKlines`, `PremiumIndexKlines`,
    `FundingRate`, `OpenInterestHist` → paths `/fapi/v1/klines`, `/fapi/v1/fundingRate`, etc. (`:32`).
  - `PageRequest::url()` (`:71`) builds `…/fapi/v1/klines?symbol=BTCUSDT&interval=5m&startTime=..&limit=..`.
  - `parse_klines_json()` (`:130`) parses a page into `TimedRow { open_time_ms, raw }` — **open-time +
    the raw row JSON only; it does NOT decode OHLCV into a typed `Bar`, nor funding into a rate.**
  - `RestSource` trait (`:117`), real `HttpRestSource` (`:161`, `#[cfg(feature="http")]`) over `ureq`,
    already classifying 429/418 → `RateLimited{retry_after_ms}`, 5xx → `Transient`, other → `Fatal`.
- `crates/ingest/src/backfill.rs` — `Backfiller<S: RestSource, Sl: Sleeper>` (`:85`) pages forward with a
  bounded-retry `RetryPolicy` (`:37`), honouring the rate-limit `Retry-After` via an injected `Sleeper`
  (`:15`; `RealSleeper` prod, recording fake in tests). Splits fresh vs overlap for reconciliation.
- `crates/ingest/src/coverage.rs` — pure **gap math** already here: `coverage(timestamps, interval_ms)`
  (`:26`) → `{first,last,present,expected,missing}` where `missing = expected − present` inside
  `[first,last]`. `flag_short_history` (`:58`) marks late-starting / early-ending series.
- `crates/ingest/src/plan.rs::enumerate_targets` (`:75`) — point-in-time listing-window enumeration
  (bulk-dump path, QE-101). `reconcile.rs::diff_overlap` — QE-103 vendor↔REST reconciliation.

### The `HistoricalSource` seam + `run_ingest`
- Trait/DTO live in **`crates/hedger/src/bootstrap.rs`**: `HistoricalWindow` (`:45`; `base`, `bars:
  Vec<Bar>`, `funding/open_interest/premium/mark_price: Vec<(i64,Decimal)>`), `HistoricalSource::fetch()`
  (`:62`), `BootstrapError` (`:28`, has a `Decode(String)` arm). Re-exported to the CLI via
  `qe_runtime::*` (`crates/runtime/src/lib.rs:19` `pub use qe_hedger::*`).
- `crates/cli/src/jobs/ingest.rs:50` — `run_ingest(params, source: &mut impl HistoricalSource, progress)`
  opens the `MarketStore`, calls `source.fetch()` once, writes `put_bars` / `put_funding` / `put_premium`.
  **`open_interest` and `mark_price` are DISCARDED** (`:44` doc-comment: "Open-interest / mark-price are
  not persisted"). `SyntheticSource` (`:151`) is the in-CLI offline `HistoricalSource` impl (returns empty
  funding/premium/oi/mark). This is the model the real source follows (a `qe-cli → qe-runtime` impl, no new
  firewall edge).
- `crates/cli/Cargo.toml`: `http = []` (currently empty — the real decoders were future work). `qe-cli`
  already depends on `qe-ingest`.
- `crates/cli/src/main.rs:480` `run_ingest_command`: the non-`--synthetic` branch just `emit_error`s
  "http decoders not yet implemented" (both cfg arms). **Wiring the CLI command / trigger to the real
  source is QE-464 (out of scope here); QE-463 delivers the library + offline tests.**

### Coverage query + `coverage_bounds`
- `crates/storage/src/store.rs:186` `coverage_bounds(instrument, resolution) -> Option<(first, last,
  count)>` — **key-only** (QE-412), no `Bar` decode. `crates/storage/src/coverage.rs::coverage` builds
  `CoverageRow{symbol,resolution,from,to,bars}` (no source marker — provenance is QE-464). There is **no**
  per-key enumeration method beyond the count; locating an internal hole needs the present open-times, so
  the planner takes present keys as input (the trigger, QE-464, supplies them from the store scan).

### `ureq` dependency tree (feature on)
`cargo tree -e features -p qe-ingest --features http` → `ureq v2.12.1` → `native-tls` → `openssl v0.10.81`
(+ `openssl-sys`, system libssl). **No `ring`, `rsa`, or `rustls`** anywhere in the tree (grep returns
nothing). Licences: OpenSSL is `Apache-2.0` (v3), already allow-listed in `deny.toml`.

### Binance USDT-M endpoints/params used
- Klines: `GET /fapi/v1/klines?symbol=&interval=&startTime=&limit=` → array rows
  `[openTime, "open","high","low","close","volume", closeTime, quoteVol, trades, ...]`.
- Historical funding: `GET /fapi/v1/fundingRate?symbol=&startTime=&limit=` → objects
  `{"symbol","fundingTime":ms,"fundingRate":"0.0001","markPrice":".."}`. Funding settles on an **8h**
  cadence (28 800 000 ms).

## 2. Implementation decisions

### Calibration honesty — **klines-only + `uncalibrated` marker** (the leaner honest slice)
Chosen per the ticket's steer ("klines-only + uncalibrated marker is the leaner slice unless aggTrade is
cheap"). aggTrade is **not** cheap here: Binance aggTrades are millions of rows/day with their own
`fromId`/`aggTradeId` pagination — a whole second pagination regime — and there is **no** trade/premium
calibration consumer wired into ingest today. So QE-463 fetches **klines + historical funding only** and:
- **Does not fabricate** premium/impact/ADV inputs — the produced window's `premium`/`open_interest`/
  `mark_price` stay **empty**, so no default number is ever dressed up as measured.
- Emits an explicit `CalibrationSource::Uncalibrated` marker on the decoded result (`IngestedWindow`),
  documented as "klines-only real vintage → calibration is `uncalibrated/default`, not measured".
- **Scope flag:** actually *surfacing* that marker in the coverage/vintage inspector requires the
  provenance thread (`CoverageRow` source column, QE-464) and `VintageContent.lineage` (QE-467), both
  **out of scope**. QE-463 provides the honest marker at the ingest layer and leaves a clear seam; this is
  returned to the parent as a product note, not silently dropped.
- **`open_interest`/`mark_price` are dropped today** by `run_ingest` (flagged above) — a second reason not
  to pretend those feed a calibration.

### Closed windows only
- Klines: a bar with open-time `T` at interval `I` is **closed** iff `T + I <= now_ms` (its close-time
  `T+I−1 < now_ms`). The forming right-edge bar (`T + I > now_ms`) is **excluded**.
- Funding: a `fundingTime` `F` row is a settled interval iff `F <= now_ms`; the in-progress interval has no
  row yet and any `F > now_ms` is excluded. `now_ms` is an **injected** parameter (determinism — no
  wall-clock in the decode/plan logic), exactly like `BackfillRequest.now_ms`.

### Incremental / resume / internal-gap — one pure planner
`plan_missing(from, to, interval, present: &BTreeSet<i64>) -> Vec<(i64,i64)>` walks the aligned grid
`from..=to` step `interval` and returns the **maximal runs of ABSENT slots** as inclusive `[start,end]`
open-time windows. One function covers all three ACs:
- **Incremental** — covered slots produce no run (no re-download of covered ranges).
- **Internal-gap** — an absent run *inside* `[first,last]` is returned (found + back-filled, not just
  edge-extended).
- **Resume** — after an interruption, the still-missing right-edge slots are one trailing run → the
  download continues from coverage's end, not the start.
- **Idempotent** — all slots present ⇒ empty vec ⇒ the source fetches nothing ⇒ `run_ingest` writes
  byte-identical bars (decode is a deterministic pure function of the page bytes).

### Wiring (behind `http`, no firewall edge)
- **`qe-ingest` (pure, always compiled):** new `binance` module — `decode_klines`, `decode_funding`,
  `closed_klines`, `closed_funding`, `plan_missing`, and `BinanceHistorical<S: RestSource, Sl: Sleeper>`
  (`fetch_window(req, present_bars, present_funding) -> IngestedWindow`) that plans → pages via the
  existing `Backfiller` → decodes → closed-filters. Generic over `RestSource`, so **all** AC tests run
  **offline** with a fake source + a checked-in fixture, in **both** the default and `http` builds (no
  `http` needed to exercise the logic — `HttpRestSource` is the only network piece).
- **`qe-cli` (`http = ["qe-ingest/http"]`):** a thin `#[cfg(feature="http")]` `BinanceHistoricalSource`
  adapter implementing `HistoricalSource`, wrapping `BinanceHistorical<HttpRestSource>` and mapping
  `IngestedWindow → HistoricalWindow` (funding filled, premium/oi/mark empty). Proven to plug into
  `run_ingest` by an offline `#[cfg(all(test, feature="http"))]` test using a fake `RestSource`. The CLI
  *command* stays as-is (real-ingest trigger is QE-464).

## 3. Test plan (all offline; fixture checked in)
Fixture: `crates/ingest/tests/fixtures/binance_usdtm/` — a small klines page + funding page (real Binance
JSON shape). Tests (in `qe-ingest`, so they run in the default gate too):
1. `decodes_klines_fixture_into_typed_bars` — OHLCV + trades decode; Decimal, never float.
2. `decodes_funding_fixture_into_rate_series` — `(fundingTime, rate)` decode.
3. `closed_window_excludes_forming_right_edge` — the last (still-forming) bar/funding interval is dropped
   for a `now_ms` mid-interval; drops nothing when `now_ms` is past the edge.
4. `incremental_skips_covered_range` — present set covers the left half ⇒ only the right half is fetched.
5. `internal_gap_is_detected_and_backfilled` — a hole punched inside `[first,last]` ⇒ exactly that hole is
   re-fetched (not an edge extension).
6. `resume_continues_from_coverage_end` — an interrupted (partial) present set ⇒ the fetch continues from
   the trailing edge, reaching the same final window as an uninterrupted run.
7. `idempotent_refetch_is_byte_identical_noop` — a fully-covered window ⇒ `plan_missing` empty ⇒ window
   has no new bars; decoding the same fixture twice yields byte-identical `Bar`s.
8. `calibration_source_is_uncalibrated_for_klines_only` — the marker asserts the honest tag.
9. (`qe-cli`, `http`) `binance_source_plugs_into_run_ingest` — `BinanceHistoricalSource` + fake REST →
   `run_ingest` writes the decoded bars/funding to a scratch store.

## 4. Risks
- **API drift** (§8.5): kline array index / funding field names could change; decode is defensive
  (explicit index/field checks → `BootstrapError::Decode`/`RestError::Fatal`), and the fixture pins the
  current shape.
- **Rate limits / pagination**: delegated to the already-tested `Backfiller`/`RetryPolicy` (no new policy).
- **Internal-gap location needs present keys** the store doesn't enumerate today (only counts). QE-463
  keeps the planner pure (keys injected); the store-scan hookup rides with the QE-464 trigger — flagged.
- **Calibration surfacing** depends on QE-464/QE-467 provenance/lineage — flagged, not faked.
- **Data-licence reality** (§8.5): REST is the public API path; redistribution differs from the bulk-dump
  path — noted, no redistribution added here.
</content>
</invoke>
