# QE-101 — Binance public-dumps downloader

`Phase: P1` · `Area: ① External sources` · `Depends on: QE-010, QE-012`

## Goal

Download the bulk long-range history from `data.binance.vision` (klines, funding, premium-index,
and `/futures/data` metrics) for the configured universe over **max-available point-in-time**
history. Checksum-verified, **resumable, idempotent**, raw files cached locally, with **schema-drift
detection** across months.

## Current state (evidence)

- `qe-ingest` is a scaffold (`crate_name()` placeholder). It depends on `qe-domain` + `qe-storage`.
- `qe-config::Universe` (QE-012) gives the point-in-time roster (`all_known()` + per-instrument
  `listed`/`delisted` windows) and a tested ISO-date→`Timestamp` parser
  (`qe_config::universe::parse_iso_date`) we reuse so this crate adds **no** civil-date math.
- `qe-domain` owns `InstrumentId` / `Resolution` / `Timestamp`. `sha2` is already a workspace dep
  (used for config/lineage hashing) — the natural choice for the `.CHECKSUM` SHA-256 verification.
- No network/HTTP dependency exists in the workspace, by design (minimal-dep ethos: hand-rolled date
  math, native LMDB, dropped bincode).

## Design

Separate the **orchestration** (the ticket's real value — caching, checksum, idempotency,
resumability, drift) from the **byte transport** (a thin HTTP GET). The transport is a `Fetcher`
port so the whole downloader is tested offline against an in-memory fake; a real `ureq`-based
`HttpFetcher` lives behind a default-off `http` feature.

### Modules (`crates/ingest/src/`)

- **`source.rs`** — the `data.binance.vision` layout. `DataKind` (`Klines(Resolution)`,
  `PremiumIndexKlines(Resolution)`, `FundingRate`, `Metrics`), `Period` (`Daily(Date)` /
  `Monthly(YearMonth)`), and `DumpFile { instrument, kind, period }` →
  `relative_path()` (also the cache key), `url(base)`, and `checksum_*` (the `<path>.CHECKSUM`
  sidecar). Periods format themselves as ISO strings; `period_start()` reuses
  `qe_config::universe::parse_iso_date` to get a `Timestamp` (no duplicated date math).
- **`checksum.rs`** — `sha256_hex(bytes)`, `parse_checksum_file(text)` (Binance `.CHECKSUM` is
  `"<hex>  <filename>"`), `verify(bytes, checksum_text)`.
- **`fetcher.rs`** — `trait Fetcher { fn get(&self, url) -> Result<Vec<u8>, FetchError>; }` with
  `FetchError::{NotFound, Transport}`. The real `HttpFetcher` (ureq, `#[cfg(feature = "http")]`)
  maps 404 → `NotFound`, other failures → `Transport`.
- **`cache.rs`** — `RawCache { root }`: mirrors the remote layout under a configurable,
  volume-friendly root (QE-013). Stores each blob plus a `<file>.sha256` sidecar recording the
  verified digest; `is_verified(file)` recomputes and compares so a half-written/corrupt cached file
  is **not** trusted.
- **`downloader.rs`** — `Downloader<F>` orchestration. `sync_file`:
  1. if the cache holds the file **and** its sidecar digest recomputes correctly → `Skipped`
     (idempotent / resumable — AC #1);
  2. else fetch the `.CHECKSUM`, fetch the file, compute SHA-256, compare;
  3. **mismatch → reject and re-fetch once** (a corrupt transfer); still mismatching →
     `ChecksumMismatch` error (AC #2);
  4. match → store blob + sidecar → `Fetched` / `Refetched`.
  `sync_all` runs a target list, accumulating a `SyncReport { skipped, fetched, refetched, failed }`
  — resumable because every file independently consults the cache first.
- **`drift.rs`** — `csv_header(zip_bytes)` (unzip first entry, first line, split on `,`) and
  `detect_drift(baseline, observed) -> DriftStatus` (`InSync` | `Drift { added, removed, reordered }`).
  `SchemaRegistry` records the first header seen per `DataKind` and flags any later month that
  differs.
- **`plan.rs`** — `enumerate_targets(universe, kinds, [from, to))`: for each instrument, emit the
  per-`(kind, period)` `DumpFile`s whose period overlaps **both** the requested window **and** the
  instrument's `[listed, delisted)` window — so we never request data from before listing or after
  delisting (point-in-time, max-available within the window).

### Why this shape

- **Idempotent / resumable (AC #1):** `sync_file` skips any file already present *and* digest-valid;
  an interrupted run resumes by re-skipping completed files. The skip recomputes the SHA-256, so a
  truncated cache entry is re-fetched rather than trusted.
- **Corrupt → rejected + re-fetched (AC #2):** verification happens *after* every fetch; a mismatch
  is never stored and triggers one re-fetch before erroring.
- **Offline-testable:** all orchestration is exercised with a fake `Fetcher` (in-memory
  url→bytes, with a "corrupt-once" variant) and in-memory ZIPs built by the `zip` crate — no network
  in CI. Default `cargo build/test` never compiles the TLS stack.
- **Point-in-time (QE-012):** `enumerate_targets` intersects with each instrument's listing window,
  inheriting survivorship-bias-free membership.
- **Minimal-dep ethos:** `sha2` (already present) + pure-Rust `zip` (deflate only); the HTTP/TLS
  stack is optional and behind `http`. native-tls (system TLS) avoids ring's non-allowlisted OpenSSL
  licence, so `cargo deny` stays green.

## Test plan (TDD)

- **`source`** — golden relative-path/URL/checksum strings for each kind+period (daily kline,
  monthly funding, daily metrics); period ISO formatting + `period_start` timestamps.
- **`checksum`** — `sha256_hex` known-answer; `.CHECKSUM` parse; `verify` accept/reject.
- **`cache`** — store/read round-trip under a tempdir; `is_verified` false for a tampered blob.
- **`downloader` (the ACs)** — with a fake fetcher:
  - re-running `sync_all` over already-present+verified files fetches nothing (counts: all skipped);
  - a fetcher that returns corrupt bytes once is rejected then re-fetched to success; a permanently
    corrupt file ends `ChecksumMismatch` and is **not** cached.
- **`drift`** — equal headers → `InSync`; an added/removed/reordered column → the right `Drift`;
  `csv_header` over a constructed in-memory ZIP; `SchemaRegistry` flags a later differing month.
- **`plan`** — an instrument listed mid-window yields only periods from its listing on; a delisted
  instrument yields nothing after delisting; count-agnostic over the universe.

## Risks / out of scope

- **Out of scope:** month-to-date REST gap (QE-102); fusion into LMDB (QE-104) — this caches **raw**
  files only.
- **Risk:** the live HTTP path can't be exercised in CI (no network) — it is a thin `ureq` adapter
  behind `http`, compiled and clippy-checked locally; the orchestration it feeds is fully tested via
  the fake. Noted explicitly.
- **Topology:** `qe-ingest` gains a `qe-config` edge (→ `qe-domain` leaf); it is not `runtime`, so
  the QE-001 guard is unaffected.
