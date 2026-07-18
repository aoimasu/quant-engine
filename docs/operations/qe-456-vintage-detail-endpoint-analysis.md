# QE-456 — `GET /api/vintages/{id}` vintage-detail endpoint — evidence note

> Analysis produced **before** coding, per the work-on-tickets skill. Ticket:
> `docs/mds/tickets/QE-456.md`. Design ref: `docs/architecture/qe-455-research-flow-design.md` §7.1
> (and §4.1 for the QE-467 persisted evidence). Additive, **read-only, no recompute**.

## 1. Goal (restated)

Add `GET /api/vintages/{id}` beside the existing thin list, returning **exactly** what QE-467 sealed
into `VintageContent`: ensemble composition (chromosomes→indicators + aligned weights), the persisted
G1 gate/deflation `SealEvidence`, `data_provenance`, a **handle** to the persisted net-of-cost holdout
series (not the inline series, not a re-run), the frozen holdout split + regime composition, the
existing provenance sidecars (`slippage`/`sizer`/`worst_case_loss`/`calibration`/`catalogue`), plus a
**vintage→run reverse-join** listing the producing run(s) with a deterministic tie-break. It must
**never** recompute gate evidence or re-run scoring — it reslices the sealed artefact.

## 2. Current-state evidence (file:line citations)

### Vintage store read path
- `crates/vintage/src/lib.rs:467` — `VintageRepository::load(vintage_id)` opens `<root>/<id>.json`,
  runs `Vintage::load` (hash-verified, `lib.rs:419`) then `schema::assert_schema` (QE-402 exact
  catalogue/genome-rep match). Missing file ⇒ `VintageError::Io(NotFound)`; tamper ⇒
  `VintageError::HashMismatch`; catalogue drift ⇒ `SchemaMismatch`/`GenomeRepMismatch`.
- `crates/vintage/src/lib.rs:50` — `VINTAGE_FORMAT_VERSION = 8` (QE-467 bump).
- QE-467 persisted fields on `VintageContent` (`lib.rs:259/263/268`): `seal_evidence: SealEvidence`
  (`lib.rs:62`), `holdout_series: HoldoutReturnSeries` (`lib.rs:102`, `handle()` at `lib.rs:113`),
  `provenance: ResearchProvenance` (`lib.rs:194`) carrying `data_provenance` (`DataProvenance`,
  `lib.rs:124`), `holdout_split: HoldoutSplit` (`lib.rs:147`), `regime_composition: Vec<RegimeShare>`
  (`lib.rs:161`), `consultation_count`, and `steer_delta: Option<SteerDelta>` (`lib.rs:171`).
- Existing sidecars already on `VintageContent`: `slippage` (`lib.rs:225`), `sizer` (`lib.rs:232`),
  `shocks` (`lib.rs:241`), `worst_case_loss` (`lib.rs:246`), `catalogue: CatalogueIdentity`
  (`lib.rs:252`), `calibration` (`lib.rs:219`).

### Existing route registration + conventions
- `crates/server/src/read.rs:22` — `read::routes() -> Router<crate::AppState>` registers
  `GET /vintages` (`list_vintages`, `read.rs:70`) and `GET /market-data/coverage`. Mounted under the
  session-gated `/api` subtree (`lib.rs:452` `.nest("/api", api)`, protected routes at `lib.rs:416`).
- `list_vintages` (`read.rs:70`) runs `repo.list()` inside `tokio::task::spawn_blocking`, maps errors
  to a `500` JSON `{ "error": … }` via `internal(..)` (`read.rs:96`). This is the exact pattern to
  mirror for the detail handler.
- Path-param + 404 precedent: `get_pool` (`crates/server/src/pools.rs:317`) —
  `Path(id): Path<String>`, `spawn_blocking`, `Ok(None) ⇒ not_found_pool(&id)` (404 JSON, `pools.rs:945`).
- `ReadState` (`lib.rs:154`) holds `vintages: VintageRepository` + `market_store`. `AppState`
  (`lib.rs:58`) exposes `FromRef` projections for both `Arc<ReadState>` (`lib.rs:124`) and
  `Arc<RunManager>` (`lib.rs:112`), so a handler in `read::routes()` can extract **both** States.

### Run→vintage linkage
- A `train` run records its sealed vintage id at `meta.train.vintage: Option<String>`
  (`crates/server/src/runs/model.rs:194`, `TrainProgress`). `RunSpec::writes_vintage()` is
  `true` only for `Train` (`model.rs:93`).
- The reverse-join has an exact precedent: `RunStore::find_run_id_by_pool` (`store.rs:100`) scans
  `read_index()` newest-first and matches `meta.train.pool == pool_id`. QE-456 mirrors it on
  `meta.train.vintage == {id}` but must **list all** matches (more than one run can seal a
  content-identical vintage) with a **deterministic tie-break**.
- `RunStore` primitives: `read_index()` (`store.rs:137`), `read_meta(id)` (`store.rs:81`),
  accessed via `RunManager::store()` (`manager.rs:146`).

### Feature-index → indicator-id resolution
- `Genome::referenced_features() -> BTreeSet<u16>` (`crates/signal/src/genome.rs:369`) — enabled
  clause feature indices.
- `FeatureSchema::from_catalogue(&CatalogueConfig::default()).ids()` (`crates/signal/src/feature.rs:49/67`)
  maps schema-ordered index → indicator id string. Sound because `VintageRepository::load` already
  asserted the sealed `CatalogueIdentity` matches this build **exactly** (QE-402), so the current
  build's schema is the authoritative addressing basis for the sealed genomes. `qe-signal` is already
  a `qe-server` dependency (`crates/server/Cargo.toml:95`) — no new crate edge.
- Catalogue-vs-evolved distinction: a feature index `< schema.len()` is a **catalogue** indicator
  (resolved to its id); an index `>= schema.len()` is an **evolved** formula reference (surfaced with
  the sealed `catalogue.formula_pool` hashes, `feature.rs:131`). Read-only over sealed data.

## 3. Implementation decisions

- **Placement.** Add the handler in `crates/server/src/read.rs` and register
  `GET /vintages/{id}` in `read::routes()` (beside the list). The handler extracts
  `State<Arc<ReadState>>` (for `vintages`) **and** `State<Arc<RunManager>>` (for the run-store
  reverse-join) — both via the existing `FromRef<AppState>` impls; no `ReadState` change, no new
  crate dependency.
- **Load + blocking.** `spawn_blocking(move || repo.load(&id))`, exactly like the list. Map
  `Err(VintageError::Io)` with `kind()==NotFound` ⇒ **404**; all other errors (hash mismatch, schema
  mismatch, deserialize, other IO) ⇒ **500** JSON `{ "error": … }`. Never a panic.
- **Reverse-join.** New `RunStore::find_runs_by_vintage(&self, vintage_id) -> io::Result<Vec<ProducingRun>>`
  mirroring `find_run_id_by_pool`: scan `read_index()`, read each meta, match
  `meta.train.vintage == vintage_id`, collect `{run_id, run_type, status, created_ms}`, sort by
  **(created_ms asc, run_id lexicographic asc)** — the deterministic tie-break. Runs inside the same
  `spawn_blocking` closure as the vintage load. `primary_run` = first of the sorted list (or `None`).
- **Holdout series as a handle.** Return `holdout_series_handle = content.holdout_series.handle()?`
  (64-hex SHA-256) and `holdout_series_len` (a count over sealed data), **never** the `returns`
  vector — the endpoint returns a ref, not a re-run.
- **Reslice, don't reshape.** `seal_evidence`, `holdout_split`, `regime_composition`,
  `consultation_count`, `steer_delta`, `data_provenance`, and the sidecars
  (`slippage`/`sizer`/`worst_case_loss`/`calibration`/`catalogue`) are the sealed types serialised
  directly (all already `Serialize`). Composition is the only computed projection (feature→id lookup +
  aligned weight) and computes nothing beyond a catalogue id lookup.
- **No recompute.** No call to `evaluate_g1`, no scoring, no backtest — the handler only reads the
  sealed artefact + the run index. No `qe-wfo`/`qe-ensemble` edge (verified against the firewall
  allowlist, `crates/architecture/tests/firewall.rs`).

## 4. Response shape (top-level fields)

```
VintageDetail {
  id, label, content_hash, format_version,
  data_provenance,                    // "real" | "synthetic" | "mixed"
  composition: [ { index, weight, indicators: [ { feature, id, source } ] } ],
  seal_evidence: SealEvidence,        // dsr, pbo, spa_pvalue, n_trials, realised_turnover,
                                      // capacity_usd, cost_stress_net_min?, uncensored_pbo?, ic?, fdr?
  holdout_series_handle,              // 64-hex ref (NOT the inline series)
  holdout_series_len,
  holdout_split: HoldoutSplit,        // holdout_range?, train_range?, embargo_bars
  regime_composition: [ RegimeShare ],// { regime, bars }
  consultation_count,
  steer_delta: SteerDelta?,           // indicator_subset_hash, generations, population, windows, folds
  sidecars: { slippage, sizer, worst_case_loss, calibration, catalogue },
  producing_runs: [ { run_id, run_type, status, created_ms } ],  // deterministic order
  primary_run: run_id?                // deterministic tie-break winner
}
```

`indicators[].source ∈ {"catalogue","evolved"}`; `id` is the catalogue indicator id (or an evolved
formula-pool reference).

## 5. Test plan

Route tests (in `read.rs` `#[cfg(test)]`, or a `crates/server/tests/` integration test mirroring the
existing read-endpoint tests):
1. **Exact-fields.** Seal a vintage with populated `seal_evidence`/`provenance`/`holdout_series`, write
   it, `GET /api/vintages/{id}` ⇒ 200 with composition (chromosomes→indicators + weights),
   `seal_evidence`, `data_provenance`, holdout split + regime composition. Assert each field equals the
   sealed value.
2. **Handle not inline.** Assert the JSON contains `holdout_series_handle` == `series.handle()` and does
   **not** contain the raw `returns` array.
3. **Reverse-join lists producers.** Write two `train` runs whose `meta.train.vintage == id` with
   different `created_ms`; assert `producing_runs` lists both in (created_ms, run_id) order and
   `primary_run` is the earliest.
4. **404.** `GET /api/vintages/does-not-exist` ⇒ 404 with the read-module error body.
5. **No recompute.** Assert via code structure (no `evaluate_g1`/backtest import in the handler) — the
   handler reads only `repo.load` + run index. Covered by the firewall test staying green + the
   handle-not-inline assertion (a re-run would produce the series, not a ref).
6. **`RunStore::find_runs_by_vintage`** unit test in `store.rs`: matches only train runs with the
   vintage, ignores unrelated runs, returns deterministic order.

## 6. Risks & mitigations

- **Feature-index out of catalogue range (evolved).** Resolve via schema len; index `>= len` ⇒
  `source:"evolved"` with the sealed formula-pool reference rather than a panic/`unwrap`.
- **Corrupt/failing-verify artefact ⇒ 500 not panic.** `VintageError` variants other than
  `Io(NotFound)` map to `500`; the handler never `unwrap`s the load.
- **Firewall regression.** No new crate dep (both `qe-signal` and `qe-vintage` already server deps);
  `firewall`/`dependency_topology` must stay green — verified in the green gate.
- **Rollback.** Purely additive (one new route + one new `RunStore` method + response DTOs); reverting
  the commit removes the endpoint with zero effect on existing routes or the sealed format.

## 7. Out of scope (per ticket)

The SPA Vintage Inspector (QE-457), any gate/holdout recomputation, any promote/seal/select affordance,
ingest/flow work, and persisting the evidence itself (QE-467, already merged).
</content>
</invoke>
