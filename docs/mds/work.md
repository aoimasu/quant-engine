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
- **Latest commit:** _(post-approval advisory follow-up — see below)_
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

### Review notes

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `8296490`) — read `persist.rs`, the `store.rs` ledger, both test files, the Cargo edge, and the
storage manifest. The AC is met and **correct**; the design choices (typed persistence, lineage-keyed
idempotency, ordering) are sound.

**AC — reproducible + range-queryable (PASS).** `persist_fused` writes the typed records and the
integration tests confirm: `scan_bars/funding/premium/futures` return **exactly** the persisted rows
(incl. a half-open `[5m,11m)` sub-range yielding just the in-window bars), `same_inputs_persist_to_
identical_stores` defines reproducibility as **identical query results** (the correct check — LMDB never
guarantees byte-identical files, so comparing scans, not the on-disk bytes, is right), and
`re_persisting_same_lineage_is_idempotent_noop` confirms a clean no-op.

**Idempotency keyed by lineage (verified collision-safe).** The ledger lives in the shared `meta` db
under a `lineage:` prefix; `lineages()` strips that prefix, so the `schema_version` key (which is not
prefix-matched) can never be returned as a vintage, and recording a lineage never disturbs the schema
record (`lineage_ledger_is_independent_of_schema_version_key` proves this directly). Ordering is **correct
and deliberate**: `persist_fused` gates on `has_lineage` first and writes `record_lineage` **last**, so a
crash mid-persist leaves the lineage *unrecorded* and a re-run re-writes everything (each `put_*` is an
upsert → identical bytes) before recording — self-healing, never a false skip.

**Typed persistence + topology (PASS).** `fused_bars` reuses QE-104 `coalesce_bars` + `adjust_bar`
(verified: last-partition-wins coalesce + ×2 adjust → close 200/202), and `FusedMarket` carries typed
`Bar`/funding/premium/futures — not the lossy scalar `FusedCorpus`. The new `qe-ingest → qe-storage` edge
is **acyclic** (storage's only internal dep is `qe-domain`), so the QE-001 `runtime ⊥ wfo/ensemble`
invariant is untouched; the existing MarketStore round-trip/range/prefix-isolation/schema tests are
retained and unaffected.

**Verification caveat (transparency).** The Rust toolchain is absent from this review environment, so I
did not execute the gates (incl. the `dependency_topology` test and `cargo deny`). The verdict rests on
full static review + hand-traced tests and manifest-level confirmation of acyclicity. I did not rely on
the PR's "all green" claim; treat the gate results as developer-reported. Nothing in the review
contradicts them.

**Advisory (non-blocking — do not gate merge):**
1. **The persist is not a single atomic transaction.** `persist_fused` issues five independent write
   txns (`put_bars` / `put_funding` / `put_premium` / `put_futures` / `record_lineage`, each its own
   commit via the QE-010 API). A crash mid-sequence leaves a *partially-written* vintage with no ledger
   entry — which the lineage-last + upsert design correctly **self-heals** on the next run, and the AC
   doesn't require atomicity, so this is fine for the intended single-writer offline batch persist. But a
   concurrent reader during a persist can observe a half-written vintage, and the partial state is
   transient on crash. If atomic vintage visibility is ever wanted, expose a MarketStore API that takes an
   external `RwTxn` (or a batched `put_all`) so all records + the ledger commit together. Noted for
   QE-135/runtime, not required here.

### Post-approval follow-up (coder) — advisory addressed (doc); status → [Ready-for-review]

The single non-blocking advisory (persist spans five write transactions, not one atomic txn → a
concurrent reader can observe a partial vintage; a crash leaves transient partial state) is now
**documented explicitly** on `persist_fused`, recording both the deliberate mitigation and the
future option:
- **Self-healing:** the lineage is recorded **last** + every `put_*` is an upsert, so a crash
  mid-persist leaves the vintage *unrecorded* and the next run re-writes idempotently (heals the
  partial state) rather than falsely skipping.
- **Concurrent-reader caveat:** a reader concurrent with an in-progress persist may see a
  partially-written vintage; sufficient for the QE-105 AC (a *completed* run is reproducible +
  range-queryable).
- **Future option (deferred):** for atomic vintage *visibility*, expose a `MarketStore` API taking
  an external `RwTxn` and write all kinds + the ledger in one transaction. Left out of QE-105 — it
  changes the storage public surface and is its own concern, as the reviewer framed it ("later").

Doc-only change; no behaviour/AC change. Gates re-run: fmt ok; clippy clean; `qe-ingest` 81 tests.
