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

---

## QE-011 — LMDB synthetic-data store — PR #11 — [Ready-for-review]

- **Branch:** `qe-011/lmdb-synthetic-store`
- **PR:** https://github.com/aoimasu/quant-engine/pull/11
- **Latest commit:** _(post-approval advisory #2 follow-up — see below)_
- **Evidence/design:** `docs/architecture/qe-011-lmdb-synthetic-store-design.md`
- **Changed surface:** `crates/storage` — **new** `src/engine.rs` (shared LMDB plumbing — the crate's
  single `unsafe` env-open + schema helpers, extracted from `store.rs`), **new** `src/synthetic.rs`
  (`SyntheticStore`), **new** `tests/synthetic.rs` (6 integration tests); `src/store.rs` refactored to
  use the shared `engine` helpers (no behaviour change — QE-010's 10 integration tests still pass);
  `src/key.rs` (+`indicator_key`); `src/lib.rs` (module wiring + exports). Also bundles the QE-010
  archive (`docs/mds/reviewed/qe-010.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks
  direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Cached indicator states are byte-identical to freshly computed ones (parity test).
- [x] Stale-source detection invalidates dependent cache entries.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-storage` 5 unit (3 key + 2 synthetic-codec) + 10 market
  integration + 6 synthetic integration tests pass; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (no new deps; heed stays
  `default-features = false`)

Key AC-proving tests (`crates/storage/tests/synthetic.rs`):
- **AC #1 (byte-identical parity)** — `indicator_state_is_byte_identical_round_trip`: caches an opaque
  state containing a NUL, high bytes (`250, 0, 99, 255`) and separately an **empty** payload, and
  asserts `get_indicator_state` returns the *exact* bytes. The cache stores **raw state bytes** (not
  re-serialised through JSON) precisely so a cached state is bit-for-bit identical to the freshly
  computed one. Reinforced by the two `src/synthetic.rs` unit tests on the value codec
  (`encode_cache_value`/`decode_cache_value` round-trip incl. empty/binary, and truncation rejection).
- **AC #2 (stale-source detection + invalidation)** — `stale_source_is_detected_and_not_served`: an
  entry tagged `lineage-A` is **not served** when queried with `lineage-B` (returns `None` → caller
  recomputes). `invalidate_stale_indicators_evicts_only_mismatched_entries`: bulk-evicts exactly the
  entries whose source lineage differs from the current one (returns the count; leaves fresh entries
  intact; idempotent — a second call removes 0).
- Plus `recon_bars_round_trip_scan_and_lineage_check` (multi-resolution bars: lineage-checked `get`,
  chronological `[from,to)` window scan), `schema_version_recorded_and_reopen_succeeds`,
  `open_result_is_usable`.

### Design notes for the reviewer
- **Shared plumbing (`engine.rs`).** QE-010's `store.rs` carried its own `unsafe` env-open + schema
  check; QE-011 extracts these into `engine.rs` so the crate has **exactly one** `unsafe` site
  (`open_env`) reused by both stores, plus `check_or_init_schema` / `read_schema_version`. `store.rs`
  was refactored onto these helpers with **no behaviour change** — QE-010's full integration suite
  still passes (10 tests).
- **Indicator cache value layout (parity-critical):** `u32(len lineage) ‖ lineage ‖ raw_state_bytes`.
  State is stored as raw bytes (the `indicators` db is `Database<Bytes, Bytes>`), *not* via
  `SerdeJson`, so AC #1 byte-identity holds for arbitrary binary payloads (incl. NUL/empty).
- **Lineage-tagged invalidation (QE-006 link):** every entry stores the **source lineage id** it was
  derived from. A read passes the *current* lineage; a mismatch is "stale" → not served. Bulk eviction
  (`invalidate_stale_indicators`) sweeps all non-matching entries. Reconstructed bars carry the same
  lineage tag (a private `ReconBar{source_lineage, bar}` via `SerdeJson`) and are lineage-checked on
  `get`.
- **Indicator key (`key.rs::indicator_key`):** length-prefixed components
  `u16(len sym) ‖ sym ‖ [resolution] ‖ u16(len id) ‖ id ‖ u32(lookback) ‖ order(time)` — unambiguous
  for exact lookups (not prefix scans), reusing QE-010's sign-flipped `order(time)` so `time_from_key`
  still works on the trailing 8 bytes.
- No new dependencies; `qe-storage` adds no internal edge to wfo/ensemble → QE-001 topology green.

### Review notes

**Verdict: [Approved].** Both ACs are met — by construction *and* by test — and the QE-010 refactor is
behaviour-preserving. Reviewed strictly as architect + senior engineer against the full diff vs `main`
(`39de9a6`/`a24bec7`).

**AC #1 — byte-identical parity (PASS).** The `indicators` db is `Database<Bytes, Bytes>` and the value
codec stores the opaque state *verbatim*: `encode_cache_value` writes `u32(len lineage) ‖ lineage ‖
state` (raw `extend_from_slice(state)`, no JSON), and `get_indicator_state` returns `state.to_vec()`
of the trailing slice. Byte-identity therefore holds for *any* payload — NUL, high bytes, empty — not
just the tested cases. `decode_cache_value` is bounds-checked end to end (`get(0..4)`, `get(..len)`,
`get(len..)`) and degrades to a miss (`None`) on malformed/truncated bytes rather than panicking;
covered by the two `synthetic.rs` unit tests and the `indicator_state_is_byte_identical_round_trip`
integration test.

**AC #2 — stale detection + invalidation (PASS).** `get_indicator_state` serves `Some` *only* when the
stored lineage equals `current_lineage`; a mismatch (or unparseable value) is a miss → recompute.
`invalidate_stale_indicators` sweeps the db, collects keys whose lineage differs, deletes exactly those,
and returns the count — selective (fresh entries survive) and idempotent (second call returns 0).
Confirmed by `stale_source_is_detected_and_not_served` and
`invalidate_stale_indicators_evicts_only_mismatched_entries`.

**Refactor safety (PASS).** Verified the `store.rs` change at the diff level: it is a pure extraction —
`open_env(path, map_size, 8)` preserves the identical `max_dbs(8)`, and `check_or_init_schema` /
`read_schema_version` carry over the inline schema logic byte-for-byte (same `SchemaMismatch` /
`SchemaCorrupt("missing")` shapes). `engine.rs` now holds the crate's single `#[allow(unsafe_code)]`
site with the SAFETY note intact. No `MarketStore` public behaviour changed, so QE-010's suite remains
valid. `indicator_key` is length-prefixed for exact lookups (no prefix-scan ordering dependency) and
keeps the trailing `order(time)` so `time_from_key` still works on it.

**Verification caveat (transparency).** I could **not** independently re-run the cargo gates this pass:
the Rust toolchain is absent from this review environment (no `cargo`/`rustc`/`rustup`, no
`~/.cargo`/`~/.rustup`). The verdict therefore rests on (a) full static review of all changed source,
(b) diff-level confirmation that the `store.rs` refactor is behaviour-preserving, and (c) inspection of
the AC-proving tests, which exercise the exact code paths above. I did **not** rely on the PR's "all
green" claim as evidence; treat the gate results in the section above as developer-reported pending an
environment with the toolchain. Nothing in the static review contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. `invalidate_stale_indicators` uses a read txn to collect, then a separate write txn to delete. Under
   the single-writer contract this is correct; the brief gap is a benign cache TOCTOU (a concurrently
   added fresh entry isn't in the stale set; a concurrently added stale one is simply swept next round).
   A single `iter_mut` + `del_current` write txn would tighten it, but the collect-then-delete form is
   clearer and avoids the iterator/borrow friction — fine to leave as-is.
2. Optional: a key-disambiguation unit test for `indicator_key` (e.g. that `("ab","c")` and `("a","bc")`
   symbol/indicator splits produce distinct keys) would lock in the length-prefix guarantee the way
   QE-010's prefix-substring test does for `bar_prefix`. Nice-to-have, not required.

### Post-approval follow-up (coder) — advisory #2 resolved; status → [Ready-for-review]

Addressed the reviewer's non-blocking advisory #2 (strictly additive — no production logic changed).
- **#2 (indicator_key disambiguation test) — DONE.** Added `indicator_key_components_are_unambiguous`
  in `crates/storage/src/key.rs`: asserts the length-prefixed split is collision-free
  (`("ab","c") != ("a","bc")`), that lookback and resolution are part of the key identity, and that
  the trailing `order(time)` still round-trips via `time_from_key`. Locks in the length-prefix
  guarantee the way QE-010's prefix-substring test does for `bar_prefix`.
- **#1 (invalidate two-txn TOCTOU) — left as-is** per the reviewer's own guidance ("fine to leave
  as-is"): correct under the single-writer contract; the collect-then-delete form is clearer.
- Gates re-run green: fmt ok; clippy clean; `qe-storage` **6 unit** (3 key + 1 new disambiguation + 2
  synthetic-codec) + 10 market + 6 synthetic integration; deny unaffected (no dep change).
