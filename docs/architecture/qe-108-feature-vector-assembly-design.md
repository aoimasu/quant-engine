# QE-108 — Feature vector assembly → synthetic store — design note

`Phase: P1` · `Area: ④ Signal generation` · `Depends on: QE-107`
`Branch: qe-108/feature-vector-assembly`

## Goal (from backlog)

WFO/DE consume **feature vectors**, not raw indicators.

- Assemble per-bar feature vectors from quantised indicator states; cache to the synthetic LMDB store.

**Acceptance criteria.**
- [ ] Feature vectors are reproducible and parity-safe (batch == streaming).

**Out of scope.** Strategy evaluation (QE-120).

## Current-state evidence

- **QE-107** gives `qe-signal::indicator`: the `catalogue(cfg)` of 22 quantised `Indicator`s, each
  `update(&Sample) -> Option<QState>` with one path shared by batch + streaming, `CATALOGUE_VERSION`,
  and `IndicatorSpec { id, lookback, num_states }`.
- **QE-107 flow caveat (carried here):** flow-factor `lookback` counts *present scalars*. This ticket
  is the declared place to **feed dense, bar-aligned scalars** so every indicator's lookback is in
  bar units — the assembler builds one `Sample` per bar and forward-fills the scalar context.
- **`qe-storage::SyntheticStore`** has a lineage-tagged indicator-state cache
  (`put_indicator_state(IndicatorKey{instrument,resolution,indicator_id,lookback,time}, lineage,
  bytes)` / `get_indicator_state`), with stale-by-lineage detection. A feature vector caches cleanly
  as one opaque blob per bar under a reserved `indicator_id`.

## Decisions

### D1 — Pure assembly in `qe-signal`; caching bridge in `qe-ingest`

`qe-signal` gets a storage-free `feature` module (the `FeatureVector`, `FeatureSchema`, and
`FeatureAssembler` over the catalogue). The synthetic-store caching is a thin `qe-ingest` bridge
(`qe-ingest` already depends on both `qe-signal` and `qe-storage`, QE-105/106).

### D2 — One `push` path ⇒ batch == streaming is structural (AC)

`FeatureAssembler::push(&Sample) -> FeatureVector` calls `update` on every catalogue indicator and
collects their states (in catalogue order) plus the bar time. `assemble_batch` is the `push` loop, so
batch and streaming are identical — the AC is structural, inherited from QE-107's per-indicator
parity.

### D3 — `FeatureVector` self-describing against a versioned `FeatureSchema`

`FeatureSchema::from_catalogue(cfg)` captures the ordered indicator ids + lookbacks and the
`CATALOGUE_VERSION` — the contract a stored vector is interpreted against. A `FeatureVector` is
`{ time_ms, states: Vec<Option<QState>> }` (one slot per indicator, in schema order; `None` until
that indicator is warm). `is_complete()` = every slot `Some`. A compact, deterministic byte codec
(`to_bytes`/`from_bytes`) encodes `i64` time + `u16` states (`0xFFFF` = `None`) — no serde needed, so
`qe-signal` stays lean.

### D4 — Cache complete vectors as one blob per bar

The bridge caches only **complete** feature vectors (every indicator warm) — the rows WFO/DE
actually consume — as `to_bytes()` under
`IndicatorKey{indicator_id: "feature_vector", lookback: schema.max_lookback(), time}`, tagged with the
source lineage. Stale-by-lineage detection is inherited from the store; a read decodes back to the
exact vector.

## Module / API plan

**`crates/signal/src/feature.rs`** (new):
- `FeatureSchema { ids, lookbacks, version }` — `from_catalogue`, `len`/`is_empty`, `ids`,
  `max_lookback`, `version`.
- `FeatureVector { time_ms, states: Vec<Option<QState>> }` — `is_complete`, `to_bytes`,
  `from_bytes(bytes, width)`.
- `FeatureAssembler` — `new(cfg)`, `schema()`, `push(&Sample) -> FeatureVector`, `reset()`.
- `assemble_batch(cfg, samples) -> Vec<FeatureVector>`.
- `lib.rs` wiring + re-exports.

**`crates/ingest/src/features.rs`** (new):
- `FEATURE_VECTOR_ID: &str = "feature_vector"`.
- `assemble_and_cache_features(store, instrument, resolution, lineage, cfg, samples) ->
  Result<usize, FeatureCacheError>` — assemble, cache each **complete** vector's bytes via
  `put_indicator_state`, return the count cached.
- `FeatureCacheError` wrapping `StorageError`.

## Test plan (TDD)

- **AC parity** (`qe-signal`): `assemble_batch` over a fixture equals streaming
  `FeatureAssembler::push` one-at-a-time; reproducible across two runs (identical bytes).
- **codec**: `FeatureVector::to_bytes`/`from_bytes` round-trips (incl. `None` slots and a complete
  vector); width mismatch → `None`.
- **schema**: `FeatureSchema::from_catalogue` len == catalogue size, ids match, version ==
  `CATALOGUE_VERSION`.
- **cache bridge** (`qe-ingest` integration): assemble→cache→`get_indicator_state`→decode equals the
  source vector; a different lineage reads stale (`None`); only complete vectors are cached (count ==
  number of fully-warmed bars).

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
`cargo test --workspace --locked`; `cargo deny check` (no new third-party deps); topology guard
(`qe-signal` stays `qe-domain`-only).

## Risks

- **Reserved `indicator_id`** (`"feature_vector"`) shares the indicator-state cache namespace with
  real indicators; it cannot collide because no catalogue indicator uses that id, and the key also
  carries the (distinct) `max_lookback`.
- **Scope:** strategy evaluation (QE-120) is out; this ticket assembles + caches the vectors only.
  Dense scalar alignment is performed here (D2) precisely to close the QE-107 flow-lookback caveat.
