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
- QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — Approved & merged.
- QE-105 — Persist fused market data to LMDB — PR #18 — Approved & merged.
- QE-106 — Multi-resolution bar reconstruction (batch) — PR #19 — Approved & merged.
- QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — PR #20 — Approved & merged.

---

## QE-108 — Feature vector assembly → synthetic store — PR #21 — [Ready-for-review]

- **Branch:** `qe-108/feature-vector-assembly`
- **PR:** https://github.com/aoimasu/quant-engine/pull/21
- **Latest commit:** `dd2c314`
- **Evidence/design:** `docs/architecture/qe-108-feature-vector-assembly-design.md`
- **Changed surface:** `crates/signal` (**new** `src/feature.rs`, `lib.rs` wiring, `QState::from_index`
  in `indicator/quant.rs`), `crates/ingest` (**new** `src/features.rs` + `tests/features.rs`, `lib.rs`
  wiring). No new third-party deps. Also bundles the QE-107 archive (`docs/mds/reviewed/qe-107.md`) +
  `docs/mds/work.md` bookkeeping — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Feature vectors are reproducible and parity-safe (batch == streaming).

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow` — clean)
- `cargo test --workspace --locked` — **263 passed, 1 ignored** (qe-signal feature 5, ingest
  features integration 3)
- `cargo test -p qe-cli --test dependency_topology` — passes (`qe-signal` stays `qe-domain`-only)
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps)

Key AC-proving tests:
- **AC (reproducible + batch == streaming)** — `feature::tests::ac_batch_equals_streaming`:
  `assemble_batch` equals the streaming `FeatureAssembler::push` loop **and** is byte-identical across
  two runs. Structural: one `push` path drives the whole catalogue; `assemble_batch` is that loop.
- **Cache bridge** (`crates/ingest/tests/features.rs`):
  `assemble_cache_and_read_back_complete_vectors` (cache count == #complete vectors; last vector
  round-trips byte-for-byte via `get_indicator_state`), `cached_feature_is_stale_under_a_different_lineage`,
  `caching_is_reproducible_across_runs`.
- **Supporting:** schema↔catalogue match, byte codec round-trip (incl. `None` slots + width mismatch),
  `vectors_become_complete_after_max_lookback`.

### Design notes for the reviewer
- **Structural parity.** `FeatureAssembler::push` collects every catalogue indicator's `update` state
  (in schema order) + the bar time; `assemble_batch` is the push loop — batch == streaming inherited
  from QE-107.
- **Self-describing vectors.** `FeatureSchema` (ordered ids + lookbacks + `CATALOGUE_VERSION`) is the
  decode contract; `FeatureVector` has a compact deterministic byte codec (`i64` time + `u16` states,
  `0xFFFF` = `None`) — no serde, so `qe-signal` stays lean.
- **Closes the QE-107 flow caveat.** The assembler builds one `Sample` per bar with the scalar context
  carried alongside, so flow-factor lookback is in bar units (callers forward-fill sparse scalars onto
  the grid before assembly).
- **Caching.** Only **complete** vectors (every indicator warm) are cached — the rows WFO/DE consume —
  as one blob per bar under reserved `indicator_id "feature_vector"` (cannot collide; key also carries
  `max_lookback`), lineage-tagged for staleness. Topology: `qe-signal` stays `qe-domain`-only.
- **Out of scope:** strategy evaluation (QE-120).
