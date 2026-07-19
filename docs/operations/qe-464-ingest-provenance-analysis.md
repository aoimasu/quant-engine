# QE-464 — ingest run-kind + POST /api/ingest + real/synthetic provenance + liquidity screen

Evidence note (design ref: `docs/architecture/qe-455-research-flow-design.md` §8.2 / §8.4 / §10).
Scope: an `ingest` run-kind + `POST /api/ingest` trigger; real-vs-synthetic-vs-mixed provenance into
the store/coverage **and** `VintageContent.lineage`; fetch-all via the as-of universe machinery
(survivorship kill); a liquidity screen. **No `PROTOCOL_VERSION` bump; no vintage schema bump.**

## Current state (file:line)

- **RunSpec enum + run_type/label/writes_vintage**: `crates/server/src/runs/model.rs:50` (`RunSpec`
  variants Backtest/Train/Evolve/Flow), `run_type()` :67, `params_value()` :78, `label()` :89,
  `writes_vintage()` :101. `CreateRunRequest` :117.
- **validate_\* pattern + build_spec dispatch**: `crates/server/src/runs/manager.rs:449` `build_spec`
  (`match req.run_type.as_str()` → per-type `validate_*` → `Ok(RunSpec::…)`; unknown type → uniform
  `CreateError::Validation` → 400). `require()` :494, `validate_backtest` :502, `validate_train` :523,
  `validate_flow` :576, `validate_evolve` :614. `manager.create` :176.
- **Subprocess spawn argv**: `crates/server/src/runs/spawn.rs` — `JobSpawner::spawn` :24 matches on
  `RunSpec`, `backtest_args` :119, `train_args` :146, `evolve_args` :200. `Flow` is sequenced, not
  spawned.
- **Routes**: `crates/server/src/runs/api.rs:49` `routes()` (`/runs` post/get, `/runs/{id}` …).
  `create_run` :59 maps `CreateError::Validation` → 400.
- **ingest done-line writer (already exists)**: `crates/run-protocol/src/lib.rs:220`
  `emit_ingest_done(w, result, synthetic)` — `ProgressLine::Done` carries the absent-by-default
  `synthetic: bool` (:164). `PROTOCOL_VERSION = 3` (:31). No bump needed (the ingest done-line
  predates this ticket).
- **CoverageRow + coverage query (key-only, QE-412)**: `crates/storage/src/coverage.rs:22`
  `CoverageRow { symbol, resolution, from, to, bars }`; `coverage()` :43 loops instruments × resolutions
  and calls `store.coverage_bounds()`; `coverage_all()` :75. `coverage_bounds`
  (`crates/storage/src/store.rs:186`) derives first/last/count from **keys only**
  (`remap_data_type::<DecodeIgnore>` — never decodes `Bar`). Key layout: `crates/storage/src/key.rs:47`
  `bar_key = instrument ‖ 0x00 ‖ [res_ordinal] ‖ order(time)`; `bar_prefix` :54.
- **as-of universe machinery**: `crates/config/src/universe.rs` — `InstrumentListing{listed,delisted}`
  (:25), `open_ended()` :56 (listed == `OPEN_LISTING = i64::MIN`, :18), `is_tradable_at` :84,
  `Universe::members_at` :109 (excludes not-yet-listed / already-delisted — the survivorship kill),
  `all_known()` :129. `Config::universe()` (`crates/config/src/lib.rs:198`) prefers the `[[universe]]`
  table; **falls back to `open_ended` per flat `instruments` entry when the section is empty**.
  `crates/ingest/src/plan.rs:58` `overlaps(period, p_end, listed, delisted)` + `enumerate_targets` :75
  intersect each instrument's `[listed,delisted)` with the fetch window.
- **QE-467 DataProvenance / lineage**: `crates/vintage/src/lib.rs:124` `enum DataProvenance {Real,
  Synthetic, Mixed}` (default Real); `ResearchProvenance.data_provenance` :240; `VintageContent.provenance`
  :312 (hashed into the vintage id). The **train path hardcodes `Real`** at
  `crates/cli/src/jobs/train.rs:956` — this is what must reflect the store's actual provenance.
  Read path already surfaces it: `crates/server/src/read.rs:316`.
- **QE-440 ADV / QE-447 %ADV guard**: `crates/risk/src/limit.rs:26` `LimitKind::MaxParticipation`
  ("order_qty / rolling hourly ADV", QE-447); slippage/impact keyed on ADV participation
  `crates/risk/src/slippage.rs:35`. Capacity-eligibility needs per-instrument rolling ADV/impact.
- **QE-463 calibration_source drop point**: `crates/cli/src/jobs/ingest.rs:346` — the real
  `BinanceHistoricalSource::fetch` maps `IngestedWindow`→`HistoricalWindow` and, after a
  `debug_assert_eq!(window.calibration_source, CalibrationSource::Uncalibrated)`, **drops** the marker
  (`HistoricalWindow` has no calibration field). `CalibrationSource {Measured, Uncalibrated}` at
  `crates/ingest/src/binance.rs:42`.
- **input_snapshot_id**: `crates/determinism/src/lineage.rs:30` — a lineage field set from params
  (`Lineage::new`/`from_config`), a SHA over `{config_hash, input_snapshot_id, code_commit, seeds}`. It is
  **not** derived from bar bytes, and my provenance records live in a separate sub-DB that never touches
  bar keys/values — so re-tagging cannot drift it. `run_ingest` writes bars via
  `MarketStore::put_bars` (`store.rs:94`).

## Implementation decisions

- **Provenance storage key-scheme (coverage stays key-only, QE-412).** New `provenance` sub-DB in
  `MarketStore`, keyed **identically to a bar key** `instrument ‖ 0x00 ‖ [res_ordinal] ‖ order(seg_start)`
  → JSON `ProvenanceSegment { end_ms, provenance, calibration }`. Each ingest run writes **one** segment
  spanning `[first_open_time, last_open_time]` of the bars it wrote. This is a small, key-scannable index
  **separate** from the bars DB, so the bars coverage scan stays byte-for-byte key-only (QE-412). Per-bar
  provenance is recoverable by prefix-scanning the segment keys (the segment whose `[start,end]` contains
  the bar) — key-scannable.
- **`Provenance {Real, Synthetic, Unknown}` + `Calibration {Calibrated, Uncalibrated}` in qe-storage**
  (deps: qe-domain only — firewall-clean). `Unknown` = legacy/untagged bars (documented default; no
  migration/guess). QE-463's real klines-only slice is `(Real, Uncalibrated)`; `--synthetic` is
  `(Synthetic, Uncalibrated)`.
- **Mixed = multiple contiguous rows.** `coverage()` emits one `CoverageRow` per provenance segment
  (each with its own `from/to/bars/provenance/calibrated`, `bars` counted key-only within the segment
  range). A store mixing real + synthetic therefore reports **multiple contiguous per-provenance rows**,
  never a single blended row. Legacy (no segment) → one `unknown`/uncalibrated row. `CoverageRow` gains
  `provenance: String` + `calibrated: bool`, both `#[serde(default)]` (additive; QE-257 wire stays
  backward-parseable; QE-465 renders the new column).
- **Vintage provenance reflects the store.** `train` derives `DataProvenance` from
  `MarketStore::store_provenance_summary(...)` (all-real→Real, all-synthetic→Synthetic, mix→Mixed,
  legacy/empty→Real documented default) instead of the hardcoded `Real`. Synthetic/mixed store ⇒ a
  vintage **marked** synthetic-/mixed-derived (hashed into the id per QE-467).
- **Fetch-all via as-of machinery + survivorship-unsafe flag.** `qe_ingest::resolve_fetch_all(universe)`
  returns the resolved roster (`all_known()` ids, incl. delisted, for max point-in-time history) plus a
  `survivorship_unsafe` flag = **every** listing is `open_ended` (listed == `OPEN_LISTING`) i.e. the config
  has no `[[universe]]` dates. The survivorship **kill** at backtest time is the existing
  `Universe::members_at(as_of)` (excludes not-yet-listed / already-delisted). Listing dates **are**
  available in config today via `[[universe]]` (see `config.example.toml` — BTCUSDT listed 2019-09-08,
  ETHUSDT 2019-11-27); a flat `instruments`-only config triggers the `survivorship-unsafe` flag path.
- **Liquidity screen.** `qe_ingest::screen_liquidity(candidates, participation_cap, min_adv_usd)`:
  capacity-eligibility **requires** per-instrument rolling ADV/impact (QE-440) — an instrument with no
  calibration (`adv_usd == None`) is **excluded** (`Uncalibrated`), and one whose ADV is below the
  conservative floor (so the `$250k` major floor would breach the QE-447 `%ADV` participation cap) is
  **flagged Thin**. Conservative default floor `MIN_ADV_USD = $2,000,000` (flagged; see risks). Names that
  pass are `Tradable`.
- **QE-463 handoff threaded.** `run_ingest` gains explicit `provenance` + `calibration` params; the real
  path passes `(Real, Uncalibrated)` (the klines-only slice's asserted marker), so an uncalibrated real
  ingest is **visibly uncalibrated** in coverage/provenance rather than reading as default-calibrated.
- **No PROTOCOL_VERSION bump / no snapshot drift.** The ingest done-line already exists; the provenance
  sub-DB never touches bar keys/values, so `coverage_bounds` (and any snapshot id over bars) is unchanged
  after (re-)tagging.

## Test plan per AC

1. **POST launches supervised run + uniform 400.** server test: `POST /api/ingest` with valid body →
   201 + run created + spawner invoked (`ingest_args`); missing start/end/resolution and missing
   instruments-without-fetch-all → 400 uniformly. `validate_ingest` unit tests.
2. **Fetch-all as-of + survivorship.** qe-ingest: `resolve_fetch_all` on a dated universe → all_known;
   `members_at(as_of)` excludes not-yet-listed/delisted; a flat/open-ended universe → `survivorship_unsafe`.
3. **Every bar tagged, key-scannable, coverage key-only.** storage: `put_bars_with_provenance` writes a
   segment; `coverage` row carries provenance; a bars-only (legacy) store → `unknown`; assert coverage
   still derives from keys (no `Bar` decode needed).
4. **data_provenance threaded; synthetic/mixed store → marked vintage.** storage
   `store_provenance_summary` (real/synthetic/mixed/unknown); train maps to `DataProvenance`
   (unit test on the mapping + a synthetic store → `Synthetic`).
5. **Coverage mixed = multiple contiguous rows.** storage: a store written real-then-synthetic over two
   ranges → two contiguous `CoverageRow`s, one per provenance, never blended.
6. **Liquidity screen.** qe-ingest: uncalibrated name excluded; thin name (ADV<floor) flagged; liquid
   name tradable.
   Plus: `--synthetic` tags `synthetic`, real tags `real`, no untagged bar going forward; re-tag causes
   no `coverage_bounds` (bar-key) drift.

## Risks

- **Thin-name %ADV threshold undefined in spec.** Picked a conservative `MIN_ADV_USD = $2M` floor +
  the existing `MaxParticipation` (QE-447) cap; **flagged** for product confirmation.
- **Real http fetch loop needs network** — the fetch-all execution loop under `--fetch-all` (http
  feature) is offline-tested via QE-463's `FakeRest` for a single instrument; the multi-instrument live
  loop is exercised structurally, not against the live venue.
- **Legacy untagged bars** default to `unknown`/`Real`-derived per the documented default (migration out
  of scope) — a pre-existing store reads `unknown` until re-ingested.
