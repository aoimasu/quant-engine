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
- [x] Round-trip + range-scan tests pass for each record kind.
      _(bars/funding/premium/futures: round-trip + `[from,to)` scan with boundary/empty cases. Key
      encoding verified correct incl. the variable-length prefix-bleed footgun — see advisory #1 re:
      adding a test for it.)_
- [x] Schema version is recorded and mismatches are detected on open.
      _(Genuine: the mismatch test seeds `999` via a raw heed env then reopens → `SchemaMismatch`.)_

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

### Review notes

**Verdict: [Approved]** — the data layer is correct across all four adversarial focus areas
(verified, including the classic key-encoding footgun empirically). Clean, well-scoped. A few
non-blocking advisories below — chiefly one high-value test to add; none is a defect, so this approves
rather than bounces.

**Independent re-verification (branch `qe-010/lmdb-market-store`):**
- `cargo fmt --all --check` clean · `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean · `cargo test --workspace --locked` **117 passed, 1 ignored** (qe-storage 11: 3 key unit + 8
  integration) · `cargo deny check` ok · QE-001 topology guard green.

**Focus 1 — key encoding / range scans (the footgun): correct.**
- `order_i64` = `(v as u64) ^ (1<<63)` big-endian is monotonic over **all** `i64` (incl. negatives) —
  byte order == time order; `unorder_i64` is its exact inverse. Pinned by the sign-bit test.
- The `0x00` delimiter is genuinely safe: `InstrumentId` is validated ASCII-alphanumeric (QE-007), so
  it never contains `0x00`, giving clean prefix boundaries. **Variable-length bleed ("BTC" vs
  "BTCUSDT") cannot happen** — a shorter instrument's prefix ends in `0x00`, which the longer one's
  bytes can't match at that position. I **verified this empirically**: stored `BTC` and `BTCUSDT`
  series, `scan(BTC)` returned exactly its 2 rows and `scan(BTCUSDT)` its 3 — no bleed.
- Scan semantics are right: `prefix_iter` yields chronological order, `t >= to → break` (exclusive +
  valid early-break), `t >= from → push` (inclusive); empty range (`from==to`) returns empty.
  `time_from_key` (trailing 8 bytes) is robust across **both** key shapes (series and bar) since the
  timestamp is always last.

**Focus 2 — schema versioning: genuine.** `open` records `SCHEMA_VERSION` on a fresh store and, on a
later open, rejects a different recorded version with `SchemaMismatch { expected, found }`. The
mismatch test is real — it fabricates a `999` version through a raw `heed` env and reopens.
`SchemaCorrupt` covers an unparseable/missing record.

**Focus 3 — `unsafe`: justified and scoped.** The single `#[allow(unsafe_code)]` wraps
`EnvOpenOptions::open` (genuinely `unsafe` in heed) with a correct `// SAFETY:` note; one `Env` per
path, mapping never handed to foreign code — the standard sound embedded-LMDB usage.

**Focus 4 — dependency hygiene: confirmed.** `heed` `default-features = false` + the `serde-json`
feature: `cargo deny check` passes and `bincode` is **absent from `Cargo.lock`** (0 matches), so
RUSTSEC-2025-0141 is avoided with no codec breakage (the `SerdeJson<T>` codec is what's used).

**Non-blocking advisory notes:**
1. **Add a variable-length prefix-isolation test (high value).** The encoding is correct (verified
   above), but the existing `bars_scan_isolates_instrument_and_resolution` uses only **same-length**
   names (BTCUSDT/ETHUSDT), so it doesn't actually exercise the prefix-bleed footgun and gives false
   confidence. For the data layer everything reads from, add a case where one instrument's name is a
   strict prefix of another (e.g. `BTC` vs `BTCUSDT`, assert each scan returns only its own rows) so a
   future key-encoding change can't silently reintroduce the bug. (I have the exact test if useful.)
2. **Document the single-open caller contract on `open`.** The `unsafe` SAFETY invariant relies on the
   same path not being opened twice in-process (LMDB advisory-lock UB); the type can't enforce it, so
   note the contract on `MarketStore::open`.
3. **Minor:** no test for the `SchemaCorrupt` (unparseable version) path, though it's implemented;
   and `scan_*` materialises the whole window into a `Vec` — fine for P0, but an iterator-returning
   API would scale better (already flagged in the design as a later optimisation).

### Post-approval follow-up (coder) — commit `c12d1cc`; status → [Ready-for-review]

Resolved the three non-blocking advisories.
- **#1 (variable-length prefix isolation) — DONE.** Added
  `bars_scan_isolates_prefix_substring_instruments`: stores `BTC` (2 rows) and `BTCUSDT` (3 rows) and
  asserts each scan returns only its own rows — exercising the actual prefix-bleed footgun (the
  earlier test used same-length names). Guards against a future key-encoding regression.
- **#2 (single-open caller contract) — DONE.** `MarketStore::open` now documents a `# Caller contract`:
  opening the same path more than once concurrently per process is UB the type can't prevent; keep one
  `MarketStore` per path and share via `Arc`.
- **#3 (SchemaCorrupt untested) — DONE.** Added `corrupt_schema_version_record_is_rejected` (seeds an
  unparseable on-disk version → `SchemaCorrupt`).
- Deferred (noted, P0-acceptable): `scan_*` materialises a `Vec`; a streaming iterator API is a later
  optimisation.
- Gates green: fmt/clippy clean; `qe-storage` 3 unit + 10 integration; deny ok.
