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
- **PR:** _(set on `gh pr create`)_
- **Latest commit:** _(see PR head)_
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

_(awaiting dedicated review agent — `start-review-ticket` against this branch/diff vs the ACs above)_
