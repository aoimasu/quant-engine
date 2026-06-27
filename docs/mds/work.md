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

---

## QE-105 — Persist fused market data to LMDB — PR #18 — [Ready-for-review]

- **Branch:** `qe-105/persist-fused-market-lmdb`
- **PR:** https://github.com/aoimasu/quant-engine/pull/18
- **Latest commit:** `8296490`
- **Evidence/design:** `docs/architecture/qe-105-persist-fused-market-lmdb-design.md`
- **Changed surface:** `crates/storage` (lineage ledger on `MarketStore` + tests), `crates/ingest`
  (**new** `src/persist.rs` + `tests/persist.rs`, `src/lib.rs` wiring, `Cargo.toml` +`qe-storage`),
  `Cargo.lock`. No new third-party deps. Also bundles the QE-104 archive
  (`docs/mds/reviewed/qe-104.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks
  direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] A full ingest→fuse→persist run is reproducible and range-queryable.

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow` — clean)
- `cargo test --workspace --locked` — **230 passed, 1 ignored** (storage lineage 2, persist unit 2,
  persist integration 3)
- `cargo test -p qe-cli --test dependency_topology` — passes (new `qe-ingest→qe-storage` edge allowed)
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps; `qe-storage` is
  a workspace crate)

Key AC-proving tests (`crates/ingest/tests/persist.rs`):
- `full_fuse_persist_run_is_range_queryable` — after `persist_fused`, `scan_bars`/`scan_funding`/
  `scan_premium`/`scan_futures` return exactly the persisted rows (incl. a sub-range scan), and the
  store records the vintage lineage.
- `same_inputs_persist_to_identical_stores` — identical inputs → identical scans from two fresh
  stores (**reproducible**).
- `re_persisting_same_lineage_is_idempotent_noop` — re-running the same lineage writes nothing; the
  store is unchanged (**idempotent**).

### Design notes for the reviewer
- **Typed persistence, not the scalar corpus.** The store schema is typed (`Bar`/funding/premium/
  futures); `persist` writes a typed `FusedMarket` (coalesced + adjusted bars via QE-104 `fused_bars`
  + the typed scalar samples). The Arrow/JSON `FusedCorpus` stays the analytical artefact (its blob
  persistence / Parquet export is QE-135, out of scope).
- **Idempotency keyed by lineage.** Record-key `put_*` is already an upsert; the new `meta`-db
  lineage ledger makes the default `persist_fused` path a clean skip when the vintage is already
  recorded, and gives the store an auditable provenance (`lineages()`). The `lineage:` prefix cannot
  collide with the `schema_version` key.
- **Topology.** New `qe-ingest → qe-storage` edge is acyclic (storage depends only on `qe-domain`);
  the QE-001 guard (`runtime ⊥ wfo/ensemble`) is unaffected and its test re-runs green.
- **Out of scope:** Parquet/DuckDB export (QE-135); persisting the Arrow blob.
