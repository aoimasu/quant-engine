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
- QE-101 — Binance public-dumps downloader — PR #14 — Approved & merged.
- QE-102 — Venue REST month-to-date backfill client — PR #15 — Approved & merged.
- QE-103 — Data-integrity & source reconciliation validation — PR #16 — Approved & merged.

---

## QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — [Ready-for-review]

- **Branch:** `qe-104/fusion-normalisation-arrow`
- **PR:** https://github.com/aoimasu/quant-engine/pull/17
- **Latest commit:** `bfb8d62`
- **Evidence/design:** `docs/architecture/qe-104-fusion-normalisation-arrow-design.md`
- **Changed surface:** `crates/ingest` — **new** `src/{canonical,derive,coalesce,fuse,arrow}.rs`,
  `src/lib.rs` (module wiring + exports), `Cargo.toml` (+`rust_decimal`; new default-off `arrow`
  feature = `arrow-array`/`arrow-schema`/`arrow-ipc`), `Cargo.lock`. Pure logic, no network. Also
  bundles the QE-103 archive (`docs/mds/reviewed/qe-103.md`) + `docs/mds/work.md` bookkeeping —
  branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Fusion is byte-reproducible for fixed inputs.
- [x] Derived fields match hand-computed references on a fixture window.

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow --all-targets -- -D warnings` — clean)
- `cargo test --workspace --locked` — **222 passed, 1 ignored**; `-p qe-ingest --features arrow`
  — 78 passed (+3 arrow tests)
- `cargo deny check` — advisories/bans/licenses/sources ok; also `cargo deny --all-features check`
  ok (covers the optional arrow tree: all Apache-2.0/MIT, no chrono/zstd/lz4)

Key AC-proving tests:
- **AC #1 (byte-reproducible)** — `fuse::tests::fuse_is_byte_reproducible_for_fixed_inputs` (two
  `fuse()` runs → identical canonical JSON); `arrow::tests::ipc_bytes_are_byte_reproducible` (two
  `corpus_to_ipc()` calls → identical Arrow IPC bytes). Columns are emitted in
  `CanonicalSeries::ALL` order and the grid is fixed.
- **AC #2 (derived fields vs hand-computed)** — `derive::tests::vwap_matches_hand_computed_reference`
  (Σ(tᵢ·vᵢ)/Σvᵢ = 22.8 over a 3-bar fixture), `typical_price_is_exact_thirds`,
  `price_factor_scales_ohlc_and_preserves_invariant`, `spread_is_signed_perp_minus_spot`.
- **Supporting:** `coalesce` merge/dedup(last-wins)/sort; `fuse::align_fills_within_bound_and_holes_beyond`
  (forward-fill within bound, holes beyond — ties to QE-103 AC #1, via `plan_fill`);
  `spread_is_hole_where_spot_missing` (no leakage where the underlier is absent).

### Design notes for the reviewer
- **Fusion consumes the QE-103 fill plan, not its own re-derivation.** `align_onto_grid` calls
  `plan_fill`; within-bound misses carry the last present value forward, over-bound/leading runs
  become `Cell::Hole` (NaN). The distinct `Hole` type introduced in QE-103 is honoured — fusion
  never confuses a fill-hole with an `integrity::Gap`.
- **Canonical series set is owned by fusion** (`CanonicalSeries`), source-abstract: the QE-101/102
  fetchers keep `DataKind`/REST endpoints and don't reference it.
- **Exact money throughout** — derived fields are `rust_decimal` (never float). The Arrow column is
  `Float64` (interchange only); the exact values live in `FusedCorpus`, which is the source of truth.
- **Arrow gated behind a default-off `arrow` feature** (the `http` precedent): keeps CI's default
  build + `cargo deny` dependency-light and green; the fusion *logic* is fully tested in the default
  build, and the Arrow path is covered under the feature and verified `deny`-clean.
- **Out of scope:** persistence into the LMDB market store (QE-105); indicators (QE-107).
  **Topology:** all additions stay within `qe-ingest`; QE-001 guard unaffected.
