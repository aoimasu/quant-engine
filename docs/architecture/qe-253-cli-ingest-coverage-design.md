# QE-253 — `qe-cli ingest` scaffold + coverage query — design & evidence

`Phase: PreP3` · `Area: runnable jobs / storage` · Plan: `docs/superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md` Task 6.

## Goal

Wire a minimal, deterministic `ingest` job that populates a `MarketStore` from the **injectable
`HistoricalSource` seam** the runtime bootstrap already defines, plus a read-only `coverage()` query the
admin server's Market-data view (QE-257) will call. Real network decoders stay behind a default-off
`http` feature and are **out of scope** here — `run_ingest` is exercised in tests with an in-memory
source, and the committed QE-251 sample store is reused for the coverage golden.

## Current-state evidence (verified against the tree)

All signatures confirmed by reading source, not the plan.

### `HistoricalSource` seam (the injectable source)
`crates/runtime/src/bootstrap.rs:62`, re-exported from `qe_runtime` (`crates/runtime/src/lib.rs:74-77`):

```rust
pub trait HistoricalSource {
    fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError>;
}

pub struct HistoricalWindow {
    pub base: Resolution,
    pub bars: Vec<Bar>,                     // any order / page-overlapping
    pub funding: Vec<(i64, Decimal)>,       // (ts_ms, rate)
    pub open_interest: Vec<(i64, Decimal)>,
    pub premium: Vec<(i64, Decimal)>,       // (ts_ms, premium)
    pub mark_price: Vec<(i64, Decimal)>,
}
```

`qe-cli` already depends on `qe-runtime` (`crates/cli/Cargo.toml`), so importing these is a no-op on the
dep graph. The window carries **no instrument id** — it is implicitly one instrument's window, so
`run_ingest` takes the target `InstrumentId` from its params.

### `MarketStore` write / scan API — `crates/storage/src/store.rs`
- `MarketStore::open(path, map_size) -> Result<Self, StorageError>` (`:52`) — takes a write txn to init schema, so tests copy the fixture to a scratch dir before opening.
- `put_bars(&self, &InstrumentId, &[Bar]) -> Result<(), StorageError>` (`:94`) — keys each bar by `bar_key(instrument, bar.resolution(), bar.open_time())`.
- `put_funding(&self, &[FundingRateSample])` (`:147`), `put_premium(&self, &[PremiumSample])` (`:190`).
- `scan_bars(&self, &InstrumentId, Resolution, from: Timestamp, to: Timestamp) -> Result<Vec<Bar>, StorageError>` (`:124`) — **half-open `[from, to)`**, returned **chronological** (`bar_key` is order-preserving; `scan_series` `:327` breaks at `t >= to`, pushes at `t >= from`).
- `pub const DEFAULT_MAP_SIZE: usize = 1 << 30` (`:17`).

### Domain / record types
- `Resolution` (`crates/domain/src/resolution.rs`): `pub const ALL: [Resolution; 8]`, `as_str() -> "1m".."1d"`, `FromStr`.
- `Timestamp` (`crates/domain/src/time.rs`): `from_millis(i64)`, `millis() -> i64`.
- `FundingRate::new(Decimal)` (`crates/domain/src/funding.rs:19`); `FundingRateSample { instrument, time, rate }` (`:38`).
- `PremiumSample { instrument, time, premium: Decimal }` (`crates/storage/src/records.rs:13`).
- `FuturesMetrics` needs 3 fields (`long_short_ratio`, `open_interest`, `taker_buy_sell_ratio`) — the window only supplies `open_interest`, so **futures/OI/mark-price are not ingested** (insufficient data; and the backtest job only scans bars/funding/premium anyway).

### Fixture reuse (QE-251)
`crates/cli/tests/fixtures/sample_store/` holds a committed LMDB store: **BTCUSDT, `1h`, 120 bars** from
`2021-01-01T00:00Z` (`START_MS = 18_628 · 86_400_000 = 1_609_459_200_000`), one bar per hour. The
coverage golden is derived from this — the fixture is **not modified** (QE-251 golden test stays green).

## Decisions

1. **Source is an explicit injectable arg, not embedded in params.** Plan sketches
   `run_ingest(params, progress)`. `HistoricalSource::fetch` takes `&mut self`, so the source is passed
   as `source: &mut impl HistoricalSource`. Signature:
   `run_ingest(params: &IngestParams, source: &mut impl HistoricalSource, progress: &mut impl FnMut(u8,&str,&str)) -> Result<(), RunError>`.
   Deviation from the plan sketch, noted here per the ticket.
2. **`coverage` returns `Result`.** Ticket sketches `coverage(store, instruments) -> Vec<CoverageRow>`,
   but scanning is fallible (`StorageError`). Final:
   `coverage(store: &MarketStore, instruments: &[InstrumentId]) -> Result<Vec<CoverageRow>, RunError>`.
   For each instrument it scans **every** `Resolution::ALL` over the full range
   (`i64::MIN..i64::MAX`) and emits one `CoverageRow` per (instrument, resolution) that has bars.
   Order is deterministic: instruments in caller order, resolutions ascending by `Resolution::ALL`.
3. **`CoverageRow` shape (QE-257-compatible).** `pub struct CoverageRow { symbol: String, resolution:
   String, from: i64, to: i64, bars: usize }`, `serde` `Serialize + Deserialize`. `from`/`to` are the
   **earliest/latest bar `open_time` in epoch-ms** (inclusive; `to` is the last bar's open time, not
   `open_time + resolution`). `resolution` is the canonical short code (`"1h"`). Lives in
   `qe_cli::jobs::ingest` so the server can call the lib (QE-257) — matches the ticket's instruction.
4. **`http` feature is a documented placeholder.** Added `[features] http = []` (default-off). The real
   Binance decoders + a `main` path that constructs a live source and calls `run_ingest` are future
   work. `main`'s `ingest` arm is fully wired (parse → dispatch → JSON progress → exit code) and, since
   no real source is built in, emits a terminal `{"t":"error"}` line and exits non-zero, with a message
   that names the `http` feature. No `cfg`-gated block references a missing symbol, so all feature
   combinations compile.
5. **Ingest persists bars + funding + premium only** — exactly what the backtest job scans. Funding
   `(ts, dec)` → `FundingRateSample`; premium `(ts, dec)` → `PremiumSample`.

## Test plan (TDD — tests written first, watched fail)

`crates/cli/tests/ingest_job.rs`:
- `coverage_over_sample_store_reports_expected_rows` — copy the committed `sample_store` to a temp dir,
  open it, `coverage(&store, &[BTCUSDT])` ⇒ exactly one row `{ symbol:"BTCUSDT", resolution:"1h",
  from: 1_609_459_200_000, to: 1_609_459_200_000 + 119·3_600_000, bars: 120 }`.
- `ingest_populates_store_from_in_memory_source` — an in-memory `HistoricalSource` yielding a small
  `HistoricalWindow` (a few `1h` bars + funding + premium); `run_ingest` into a fresh temp store; then
  `coverage()` / `get_bar()` confirm the bars landed with the expected range and count.
- Unit tests in `ingest.rs` for `CoverageRow` serde shape and the funding/premium conversion.

Also: existing `backtest_over_fixture_store_matches_golden` must stay green (fixture untouched).

## Risks

- **Server → `qe-runtime` firewall (QE-257).** `coverage`/`CoverageRow` live in `qe-cli`, which depends
  on `qe-runtime`; QE-254's `qe-server` must not reach `qe-runtime`. The ticket explicitly parks this
  here and says the server will "re-run the job or call the lib" — resolving the edge (e.g. moving
  `CoverageRow` to a shared crate, or the server shelling out) is **QE-257's** decision, out of scope.
  `CoverageRow` itself depends only on `std`/`serde`, so it is trivially relocatable later.
- **`coverage` materialises all bars to count them.** `scan_bars` returns `Vec<Bar>`; min/max/count come
  from first/last/len of the chronological vector. Fine for the fixture and realistic sizes; a
  keys-only cursor is a future optimisation if the store grows large.
- **`i64::MIN..i64::MAX` scan bounds.** Half-open `[from, to)`: a bar exactly at `i64::MAX` would be
  excluded — not a real instant, so acceptable.
