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

---

## QE-101 — Binance public-dumps downloader — PR #14 — [Ready-for-review]

- **Branch:** `qe-101/binance-dumps-downloader`
- **PR:** https://github.com/aoimasu/quant-engine/pull/14
- **Latest commit:** _(post-approval advisory follow-up — see below)_
- **Evidence/design:** `docs/architecture/qe-101-binance-dumps-downloader-design.md`
- **Changed surface:** `crates/ingest` — fills the scaffold: **new** `src/{source,checksum,fetcher,
  cache,downloader,drift,plan}.rs`, rewritten `src/lib.rs` (`IngestError` + re-exports), **new**
  `tests/downloader.rs` (2 integration tests), `Cargo.toml` (+`qe-config`/`sha2`/`thiserror`,
  +`zip` deflate-only, +optional `ureq` behind the `http` feature; dropped unused `qe-storage`).
  Also bundles the QE-013 archive (`docs/mds/reviewed/qe-013.md`) + `docs/mds/work.md` bookkeeping —
  branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Re-running the downloader fetches nothing already present and verified.
- [x] Corrupt/checksum-mismatched files are rejected and re-fetched.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features http` — the ureq adapter — clean)
- `cargo test --workspace --locked` — `qe-ingest` 22 unit + 2 integration; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (the optional `ureq` uses **native-tls**
  to avoid ring's non-allowlisted OpenSSL licence; `zip` is deflate-only, pure-Rust)

Key AC-proving tests:
- **AC #1 (idempotent / resumable)** — `downloader::tests::fetches_then_reruns_skip_everything` and
  the integration `enumerate_download_and_rerun_is_idempotent`: a clean fetch then a re-run reports
  all-skipped and issues **zero** network hits for already-verified files (shared hit counter).
- **AC #2 (corrupt → rejected + re-fetched)** — `corrupt_transfer_is_rejected_and_refetched` (one
  corrupt transfer → re-fetched to success, two hits) and
  `persistently_corrupt_file_errors_and_is_not_cached` (never verifies → `ChecksumMismatch`, not
  cached). Cache `tampered_file_is_not_verified` proves a truncated cache entry isn't trusted.
- **Supporting:** `source` path/URL/checksum golden strings; `checksum` SHA-256 KAT + `.CHECKSUM`
  parse; `plan` point-in-time enumeration (listing/delisting windows, daily/monthly granularity,
  count-agnostic); `drift` added/removed/reordered + ZIP-header extraction + cross-month registry.

### Design notes for the reviewer
- **Port/adapter split:** the downloader logic runs against a `Fetcher` trait (the only network
  seam), so the whole crate is tested offline with an in-memory fake. The real `HttpFetcher` (ureq +
  system TLS) is **behind a default-off `http` feature** — CI's default build/test never compiles
  the TLS stack; the live path is a thin adapter compiled + clippy-checked locally.
- **Idempotent / resumable (AC #1):** `sync_file` skips any file present **and** whose bytes re-hash
  to the stored `.sha256` sidecar; a re-run after interruption re-skips completed files. The skip
  recomputes SHA-256, so a truncated cache entry is re-fetched rather than trusted.
- **Corrupt → reject + re-fetch (AC #2):** verification happens after every fetch; a digest mismatch
  is never cached and triggers exactly one re-fetch before erroring `ChecksumMismatch`.
- **Point-in-time (QE-012):** `enumerate_targets` intersects each instrument's `[listed, delisted)`
  window with the requested month window — never requesting data before listing or after delisting.
  Reuses `qe_config::universe::parse_iso_date` so the crate adds **no** civil-date math.
- **Schema drift:** `csv_header` extracts the header from the dump ZIP (pure-Rust `zip`, deflate);
  `SchemaRegistry` baselines the first month per kind and flags later differing columns.
- **Minimal deps / licences:** `sha2` (already present) + `zip` (deflate); HTTP/TLS optional. `cargo
  deny` green. **Topology:** new `qe-ingest → qe-config` edge (→ `qe-domain` leaf); not `runtime`, so
  the QE-001 guard is unaffected.

### Review notes

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `f6ad1bd`) — read every new src file and both integration tests. Both ACs are met and **correct**,
the port/adapter boundary is clean, and the dependency/topology hygiene holds.

**AC #1 — idempotent / resumable (PASS).** `cache.is_verified` re-reads the cached bytes and re-hashes
them against the trimmed `.sha256` sidecar (a missing file/sidecar or hash mismatch → `false`), so a
truncated or tampered entry is **never trusted** (`tampered_file_is_not_verified`). `sync_file` returns
`Skipped` on a verified hit **before any fetch**, so a re-run touches no network. The integration test
`enumerate_download_and_rerun_is_idempotent` proves this with a shared `Rc<RefCell<usize>>` hit counter:
after a 10-file fetch, the second `sync_all` reports `skipped == 10`, `fetched == 0`, and the counter is
**unchanged** (zero network hits). I independently verified the 10-target count (BTC open-ended 3mo×2
kinds = 6; ETH listed 2020-02-01 → Feb+Mar ×2 = 4).

**AC #2 — corrupt → rejected + re-fetched (PASS).** `sync_file` fetches the authoritative checksum
sidecar once, then loops `0..2`: each transfer is SHA-256-verified, a mismatch is discarded (**never
cached**) and retried exactly once; a persistent mismatch returns `ChecksumMismatch` with nothing
written. `corrupt_transfer_is_rejected_and_refetched` (1 corrupt + 1 good → `Refetched`, 2 hits, then
cached) and `persistently_corrupt_file_errors_and_is_not_cached` (errors, not cached) confirm both
branches.

**Port/adapter split (PASS).** All orchestration runs against the `Fetcher` trait — the only network
seam — and is fully exercised offline via in-memory fakes. The real `HttpFetcher` (ureq + native-tls) is
behind `#[cfg(feature = "http")]` with `default = []`, so the default build/test never compiles the TLS
stack; 404 vs other failures are distinguished at the `FetchError` boundary.

**Point-in-time enumeration (PASS).** `plan::overlaps` reduces to the exact half-open interval-intersection
test `p_start < delisted && listed < p_end` (period `[p_start, p_end)` ∩ listing `[listed, delisted)`),
with monthly `p_end` = next-month-start and daily `p_end` = start + 86_400_000. I traced both tests by
hand — monthly ETH `[Mar15, Jun1)` → Mar/Apr/May (3); daily Feb-2020 listed Feb 10 → days 10..=29 (20).
Date→timestamp reuses `qe_config::universe::parse_iso_date` (no civil-date conversion duplicated).

**Schema drift + golden paths (PASS).** `detect_drift` classifies added/removed/reordered correctly;
`csv_header` extracts row 0 from the deflate ZIP with proper error mapping; `SchemaRegistry` baselines
first-seen-per-kind and flags later differences. `source.rs` golden strings match the real
`data.binance.vision` `futures/um` layout (daily klines with interval segment, monthly `fundingRate`
without, daily `metrics`), URLs and `.CHECKSUM` sidecars included.

**Deps / topology (PASS).** `Cargo.toml`: `http` feature-gated `ureq` with `default-features = false` +
`native-tls` (avoids ring's non-allowlisted licence), `zip` deflate-only pure-Rust, dropped the unused
`qe-storage`. `qe-ingest`'s only deps are `{qe-config, qe-domain, sha2, thiserror, zip, ureq}` — no
`wfo`/`ensemble`/`runtime` — so the new `qe-ingest → qe-config (→ qe-domain leaf)` edge cannot affect the
QE-001 `runtime ↮ wfo/ensemble` guard. Confirmed structurally from the manifest.

**Verification caveat (transparency).** I could **not** independently re-run the cargo gates this pass:
the Rust toolchain is absent from this review environment (no `cargo`/`rustc`/`rustup`), so the
default-vs-`http` clippy and `cargo deny` claims were not executed here. The verdict rests on full static
review of all changed source, hand-verification of the interval/enumeration math and the golden paths,
and manifest-level confirmation of the dependency topology. I did not rely on the PR's "all green" claim
as evidence; treat the reported gate results as developer-reported. Nothing in the static review
contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. **404 handling doesn't yet match its documented intent.** `fetcher.rs` distinguishes
   `FetchError::NotFound` *"so the planner can treat a missing period as no data rather than a hard
   error"*, but `sync_file` maps it to `IngestError::NotFound` and `sync_all` records it in
   `report.failed` like any failure. For windows that enumerate an as-yet-unpublished month (e.g. the
   current month, or a gap), a benign 404 will surface as a "failure." No AC impact (the run continues
   and idempotency/corruption both hold), but wire NotFound → a benign `missing`/skipped classification
   before `report.failed` is used for alerting.
2. **`csv_header` reads the entire decompressed entry into a `String`** just to take row 0 — for bulk
   monthly CSVs (1m klines ≈ multi-MB decompressed) that's wasteful. A `BufReader::read_line` over the
   ZIP entry reads only the header.
3. **`SchemaRegistry` keys baselines on `format!("{kind:?}")`** (the Debug string) — functional but
   couples map identity to Debug formatting. `DataKind` already derives `Eq/Copy`; derive `Hash` and key
   on `DataKind` directly.
4. **Minor duplication / DRY (trivial):** `plan::month_days` re-implements the leap/days-in-month rule
   that `qe_config::universe` already has, and `checksum::sha256_hex` is the third hand-rolled per-byte
   hex formatter in the workspace (lineage, cli vintage) — candidates for a shared helper.

### Post-approval follow-up (coder) — advisories #1–#3 resolved; status → [Ready-for-review]

Addressed the reviewer's non-blocking advisories (no AC behaviour weakened).
- **#1 (NotFound intent/code gap) — DONE.** A 404 now means "no dump published for this period" and
  yields the new `FileOutcome::Missing` (counted in `SyncReport.missing`), **not** a `failed` entry.
  `sync_file` uses a `fetch_opt` that maps 404 → `Ok(None)`; the now-unreachable `IngestError::NotFound`
  variant was removed. New test `missing_period_is_not_a_failure` (404 → Missing, nothing cached).
- **#2 (csv_header read whole entry) — DONE.** `csv_header` now reads only the first line via a
  `BufReader::read_line`, not the whole decompressed CSV.
- **#3 (registry keyed on Debug string) — DONE.** Derived `Hash` on `DataKind` (its `Resolution`
  already derives `Hash`) and keyed `SchemaRegistry` on `HashMap<DataKind, _>` directly.
- **#4 (trivial leap-year/hex DRY) — left as-is:** `plan::month_days` can't reuse `qe-config`'s
  private leap helper without widening that crate's API; the duplication is 4 lines and well-tested.
- Gates re-run green: fmt ok; clippy clean (default **and** `--features http`); `qe-ingest` 23 unit
  (+1) + 2 integration; deny unaffected.
