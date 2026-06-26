# QE-010 — LMDB market-data store — design / evidence

## Ticket

`Phase: P0` · `Area: ③ Storage` · `Depends on: QE-007`

**Goal.** The fused market corpus (bars, funding, spread-to-underlier, futures metrics) needs a fast,
embedded, deterministic key-value store.

**Scope / requirements.**
- LMDB schema for OHLCVT bars, funding rates, premium/spread-to-underlier, futures metrics
  (top-trader L/S, OI, taker), keyed by instrument + resolution + time.
- Versioned schema; read/write APIs with range scans; concurrency-safe reads.

**Out of scope.** Synthetic/indicator cache (QE-011); fusion logic (QE-104).

**Acceptance criteria.**
- Round-trip + range-scan tests pass for each record kind.
- Schema version is recorded and mismatches are detected on open.

## Current-state evidence

- `qe-storage` is a QE-001 scaffold (`crate_name()` only). QE-007 provides `Bar` (OHLCVT),
  `FundingRateSample`, `InstrumentId`, `Resolution`, `Timestamp`, plus `Notional`/`Qty` and the
  validated-newtype + exact-string-decimal serde discipline. `Bar` carries `resolution`/`open_time`
  but **not** `instrument` — so the store keys a bar by an explicit `(instrument, resolution, time)`.
- Premium/spread-to-underlier and futures-metrics record types were out of QE-007's scope; they are
  defined here (storage records) from qe-domain primitives.
- **LMDB choice:** `heed` 0.20 (typed LMDB; meilisearch-backed). Verified it builds in this
  toolchain (LMDB C compiles via `cc`), and every transitive licence is already on the `deny.toml`
  allowlist (heed MIT, `lmdb-master-sys` Apache-2.0) — no `deny.toml` change.

## Design decisions

Fill `qe-storage` with `MarketStore` over one LMDB `Env`, one named sub-database per record kind plus
a `meta` db.

### Schema / keys
- Sub-dbs: `bars`, `funding`, `premium`, `futures_metrics`, `meta`.
- **Order-preserving byte keys** (LMDB compares keys lexicographically, default comparator):
  - bar key = `instrument.as_str()` bytes ‖ `0x00` ‖ `[resolution_ordinal: u8]` ‖ `order(time_ms)`
  - series key (funding/premium/futures) = `instrument` bytes ‖ `0x00` ‖ `order(time_ms)`
  - `order(i64)` flips the sign bit (`v as u64 ^ 1<<63`, big-endian) so byte order == numeric order
    for **all** i64 (incl. negative). `0x00` is a safe delimiter — `InstrumentId` is validated
    ASCII-alphanumeric, so it never contains `0x00`, giving clean prefix boundaries.
- **Range scans** use heed `prefix_iter(prefix)` (instrument[+resolution]); results arrive in
  chronological order, so the scan filters `[from, to)` and **breaks early** once `time >= to`.
- **Values**: heed `SerdeJson<T>` codec (exact, since the records' decimals serialise as strings).

### Versioning + open
- `SCHEMA_VERSION: u32 = 1`, stored in `meta["schema_version"]`. `open` creates the dbs, then: if a
  version is present and `!= SCHEMA_VERSION` → `StorageError::SchemaMismatch { expected, found }`; if
  absent → write the current version; all in one write txn.

### Concurrency-safe reads
- LMDB is MVCC: many concurrent readers alongside one writer. `MarketStore` is `Send + Sync`; each
  read/scan opens a fresh `read_txn`. A test shares `Arc<MarketStore>` across threads doing
  concurrent scans.

### `unsafe`
- `heed::EnvOpenOptions::open` is `unsafe` (LMDB memory-maps the file and the caller must ensure no
  unsound concurrent mutation through a foreign mapping). The workspace denies `unsafe_code`; this is
  the single, narrowly-scoped `#[allow(unsafe_code)]` with a `// SAFETY:` note — one process owns the
  exclusive on-disk path and never hands the mapping to foreign code (standard sound embedded use).

### Records / errors
- `PremiumSample { instrument, time, premium }` and `FuturesMetrics { instrument, time,
  long_short_ratio, open_interest, taker_buy_sell_ratio }` — decimals serialise as exact strings.
- `StorageError` (thiserror): `Lmdb(#[from] heed::Error)`, `SchemaMismatch`, `SchemaCorrupt`.

### Dependencies / topology
`qe-storage`: `qe-domain`, `heed` (`serde-json`), `rust_decimal`, `serde`, `thiserror`. Dev:
`tempfile` (temp dirs). No internal edge to `qe-wfo`/`qe-ensemble` → QE-001 topology guard green.

## Test plan (proves both ACs)

- **AC #1 (round-trip + range-scan per record kind):** for bars, funding, premium, futures —
  put a series, `get` an exact key (round-trip equality), `scan` a sub-range and assert the returned
  slice equals the expected `[from, to)` window in order (incl. boundary: `to` exclusive, `from`
  inclusive, and an empty range). A cross-instrument test asserts one instrument's scan never returns
  another's rows (prefix isolation). A negative-timestamp test pins the sign-bit ordering.
- **AC #2 (schema version recorded + mismatch detected):** fresh `open` records `SCHEMA_VERSION` and
  `schema_version()` returns it; re-`open` of the same dir succeeds; a dir written with a different
  version is rejected with `SchemaMismatch` (test writes a bogus version via a second store/meta and
  re-opens).
- **Concurrency:** `Arc<MarketStore>` scanned from several threads concurrently returns consistent
  results (LMDB MVCC reads).

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **`unsafe` env open:** mitigated by the scoped allow + SAFETY note + single-owner invariant.
- **`map_size`:** LMDB needs a max map size at open; `open(path, map_size)` takes it (tests use a few
  MB, real use sizes up; documented). Exceeding it surfaces as a `heed::Error`, not silent loss.
- **Key encoding bugs** (the classic range-scan footgun): pinned by the sign-bit-ordering test, the
  boundary/empty-range tests, and the cross-instrument isolation test.
- **JSON value bloat:** acceptable for P0 correctness; a compact codec is a later optimisation
  (noted, not done).
