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

---

## QE-010 — LMDB market-data store — PR #10 — [Ready-for-review]

- **Branch:** `qe-010/lmdb-market-store`
- **PR:** https://github.com/aoimasu/quant-engine/pull/10
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-010-lmdb-market-store-design.md`
- **Changed surface:** fills the `crates/storage` scaffold — `src/{lib,records,key,store}.rs`,
  `tests/store.rs`, `Cargo.toml`; root `Cargo.toml` (+`heed` with `default-features = false`,
  +`tempfile` dev). Also bundles the QE-009 archive (`docs/mds/reviewed/qe-009.md`) — branch
  protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [ ] Round-trip + range-scan tests pass for each record kind.
- [ ] Schema version is recorded and mismatches are detected on open.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-storage` 3 unit (key) + 8 integration tests pass; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (heed `default-features = false` drops the
  unmaintained `bincode`, RUSTSEC-2025-0141; only the serde-json codec is used)

Key AC-proving tests (`crates/storage/tests/store.rs`):
- **AC #1 (round-trip + range-scan per kind)** — bars/funding/premium/futures: exact `get` round-trip
  + `scan` over a sub-range with boundary checks (`to` exclusive, `from` inclusive, empty range),
  chronological order; `bars_scan_isolates_instrument_and_resolution` (prefix isolation);
  `key::tests` pin sign-bit ordering for negative timestamps.
- **AC #2 (schema version recorded + mismatch)** — `schema_version_is_recorded_and_reopen_succeeds`
  and `schema_version_mismatch_is_detected_on_open` (a dir seeded with version `999` → `SchemaMismatch`).
- Plus `reads_are_concurrent` (4 threads scanning a shared `Arc<MarketStore>` — LMDB MVCC).

### Design notes for the reviewer
- LMDB via `heed` 0.20. **Order-preserving keys**: `instrument ‖ 0x00 ‖ [resolution] ‖ order(time)`,
  where `order(i64)` flips the sign bit so byte order == numeric order for all i64; `0x00` is a safe
  delimiter (`InstrumentId` is validated ASCII-alphanumeric). Range scans use `prefix_iter` + early
  break. Values use heed's `SerdeJson<T>` (exact, decimals serialise as strings).
- **One `unsafe`**: `EnvOpenOptions::open` is `unsafe`; a single, scoped `#[allow(unsafe_code)]` with a
  SAFETY note (single-owner exclusive path). The workspace otherwise denies `unsafe_code`.
- `PremiumSample`/`FuturesMetrics` defined here (out of QE-007 scope) from qe-domain primitives.
- `qe-storage` deps (qe-domain/heed/rust_decimal) add no internal edge to wfo/ensemble → topology green.
