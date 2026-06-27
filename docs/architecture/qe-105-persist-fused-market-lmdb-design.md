# QE-105 — Persist fused market data to LMDB — design note

`Phase: P1` · `Area: ②→③` · `Depends on: QE-104, QE-010`
`Branch: qe-105/persist-fused-market-lmdb`

## Goal (from backlog)

Downstream signal/WFO/DE stages read the **market store**, not raw files. Persist the fused market
data into the QE-010 `MarketStore`.

- Write fused market data into the QE-010 schema; **idempotent upserts keyed by lineage**.

**Acceptance criteria.**
- [ ] A full ingest→fuse→persist run is reproducible and range-queryable.

**Out of scope.** Parquet/DuckDB export (QE-135).

## Current-state evidence

- **QE-010 `MarketStore`** (`crates/storage`) already stores the typed fused corpus: `put_bars` /
  `scan_bars` (keyed by instrument+resolution+time), `put_funding`/`scan_funding`,
  `put_premium`/`scan_premium`, `put_futures`/`scan_futures`. LMDB `put` overwrites by key, so each
  `put_*` is **already an idempotent upsert at the record-key level**; keys are order-preserving so
  scans are chronological and range-queryable. A `meta` (`Str`→`Str`) sub-db holds the schema
  version under `schema_version`.
- **QE-104 fusion** (`crates/ingest`): `coalesce_bars` (daily→monthly merge/dedup/sort) and
  `adjust_bar` (split/contract adjustment) produce the normalised, deterministic bar series;
  funding/premium/futures arrive as typed samples from ingest. `FusedCorpus` is the *analytical*
  Arrow/JSON view (scalar, aligned-to-grid) — the **typed records** are what the store range-queries.
- **Lineage** (`qe-determinism::Lineage`, used by QE-013 `run_train`): a content-addressed vintage
  id (`Lineage::id()`), already the key under which artefacts are written.
- **Topology (QE-001):** the guard only forbids `runtime ↔ wfo/ensemble`. A new
  `qe-ingest → qe-storage` edge is acyclic (storage depends only on `qe-domain`) and allowed.

## Decisions

### D1 — The persistence bridge lives in `qe-ingest`, depending on `qe-storage`

`qe-ingest` already owns "② import & fusion"; "②→③ persist" is its natural extension. A new
`persist` module depends on `qe-storage`. No topology violation (verified against the QE-001 guard,
which only constrains runtime↔training).

### D2 — Persist the **typed** fused records, not the scalar `FusedCorpus`

The store's schema is typed (`Bar`, `FundingRateSample`, `PremiumSample`, `FuturesMetrics`). The
scalar `FusedCorpus` is lossy for bars (close-only). So QE-105 persists a typed `FusedMarket`
bundle: the **coalesced + adjusted** perp bars (QE-104 `coalesce_bars`/`adjust_bar`) plus the typed
funding/premium/futures samples. The Arrow `FusedCorpus` remains the analytical interchange artefact
(its persistence as a derived blob is out of scope / QE-135).

### D3 — Idempotency keyed by lineage: a lineage ledger in `meta`

Record-key upserts are idempotent but say nothing about *which vintage* a store holds. QE-105 adds a
**lineage ledger** to `MarketStore` (in the existing `meta` db, under a `lineage:` prefix):
`record_lineage(id) -> bool` (true iff newly recorded), `has_lineage(id)`, `lineages()`. The persist
bridge **skips** (no writes) when the lineage is already recorded — so re-running the same
ingest→fuse→persist is a true idempotent no-op, and the store carries an auditable provenance of the
vintages it contains. (Record-level upsert still makes a *forced* re-write safe; the ledger makes
the *default* path a clean skip.)

## Module / API plan

**`crates/storage/src/store.rs`** — extend `MarketStore`:
- `record_lineage(&self, lineage_id: &str) -> Result<bool, StorageError>`
- `has_lineage(&self, lineage_id: &str) -> Result<bool, StorageError>`
- `lineages(&self) -> Result<Vec<String>, StorageError>`
(stored as `lineage:<id>` → `"1"` in `meta`; `lineages()` prefix-scans and strips the prefix.)

**`crates/ingest/src/persist.rs`** (new; +`qe-storage` dep):
- `FusedMarket { instrument, bars: Vec<Bar>, funding: Vec<FundingRateSample>, premium:
  Vec<PremiumSample>, futures: Vec<FuturesMetrics> }`.
- `fused_bars(perp_partitions: &[Vec<Bar>], adjustment: Adjustment) -> Result<Vec<Bar>, DomainError>`
  — coalesce + adjust (the "fuse" step for bars), reused from QE-104.
- `PersistStatus { Persisted, AlreadyPersisted }`, `PersistReport { lineage_id, status, bars,
  funding, premium, futures }` (counts).
- `persist_fused(store: &MarketStore, lineage_id: &str, market: &FusedMarket) ->
  Result<PersistReport, PersistError>` — skip-if-recorded, else write all four record kinds then
  record the lineage.
- `PersistError` (thiserror) wrapping `StorageError` (+ surfaced as needed).

**`lib.rs`** — wire `persist` + re-export `FusedMarket`, `PersistReport`, `PersistStatus`,
`PersistError`, `persist_fused`, `fused_bars`.

## Test plan (TDD)

- **storage lineage ledger** (`crates/storage`): `record_lineage` returns true then false on repeat;
  `has_lineage`/`lineages` reflect recorded ids; independent of schema-version key.
- **persist unit** (`crates/ingest`): `fused_bars` coalesces+adjusts; `persist_fused` writes counts
  and records lineage; a second call with the same lineage returns `AlreadyPersisted` with zero
  writes.
- **AC — full pipeline integration** (`crates/ingest/tests`): build a `FusedMarket`, open a tempdir
  `MarketStore`, `persist_fused`, then `scan_bars`/`scan_funding`/`scan_premium`/`scan_futures`
  return exactly the persisted rows in range (**range-queryable**); persisting the same lineage into
  a second fresh store yields **identical scans** (**reproducible**); re-persisting the same lineage
  is a no-op (**idempotent**).

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
`cargo test --workspace --locked`; `cargo deny check` (no new third-party deps — `qe-storage` is a
workspace crate, so deny is unaffected). QE-001 topology guard re-runs (the new
`qe-ingest→qe-storage` edge is allowed).

## Risks

- **Lineage ledger in `meta`** shares the db with `schema_version`; the `lineage:` prefix cannot
  collide with the fixed `schema_version` key, and `lineages()` filters by prefix.
- **Scope discipline:** persisting the Arrow `FusedCorpus` blob and Parquet/DuckDB export are **not**
  here (QE-135). QE-105 persists the typed, range-queryable market records only.
