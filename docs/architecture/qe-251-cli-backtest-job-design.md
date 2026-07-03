# QE-251 — `qe-cli backtest` job (run a vintage over a window) — design & evidence

Implements plan `docs/superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md` **Tasks 1, 2, 3, 5**.
Task 4 (trade recording in `qe-wfo`) landed with QE-252 and is reused, not reimplemented.
Result-contract field names follow `docs/superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md` §8.1.

## Current-state evidence (signatures verified in-tree on 2026-07-03)

- **qe-vintage** (`crates/vintage/src/lib.rs`):
  - `VintageRepository::new(root: impl Into<PathBuf>) -> VintageRepository`; `repo.load(&str) -> Result<Vintage, VintageError>` (path `<root>/<id>.json`); `Vintage::load` already verifies the hash on read.
  - `Vintage { content: VintageContent, content_hash: String }`; `Vintage::verify() -> Result<(), VintageError>`.
  - `VintageContent { format_version, vintage_id, chromosomes: Vec<qe_signal::Genome>, weights: Vec<f64>, calibration: CalibrationProfile, worst_case_loss: Option<f64>, lineage }`.
- **qe-storage** (`crates/storage/src/store.rs`): `MarketStore::open(path, map_size) -> Result<_, StorageError>`; `scan_bars(&InstrumentId, Resolution, from: Timestamp, to: Timestamp) -> Result<Vec<qe_domain::Bar>>` over `[from, to)`; `scan_funding`/`scan_premium`同 shape; `put_bars`/`put_funding` for fixture building.
- **qe-wfo** (`crates/wfo/src/backtest.rs`, re-exported at `qe_wfo::backtest`):
  - `backtest_with_trades(&Genome, &[wfo::Bar], &BacktestConfig) -> (BacktestResult, Vec<TradeFill>)` (QE-252, present & merged).
  - `wfo::Bar { features: FeatureVector, price: Decimal, funding_rate: Option<Decimal> }`.
  - `BacktestResult { returns: Vec<f64> /* len = bars-1, bar0 excluded */, trades: usize, net_pnl: Decimal, accepted, fitness }`.
  - `TradeFill { entry_idx, exit_idx, side: Direction, entry_px: Decimal, exit_px: Decimal, return_frac: f64 }` — `return_frac` is a **gross, price-only** round-trip return (no qty/fees).
  - `BacktestConfig { friction: FrictionConfig, min_trades, windows }`; `FrictionConfig { fees: FeeSchedule{taker,maker}, slippage: SlippageModel{half_spread,impact}, cost_multiplier }`.
- **qe-signal** (`crates/signal/src/{feature,genome,indicator}.rs`):
  - `assemble_batch(&CatalogueConfig, &[Sample]) -> Vec<FeatureVector>`; `FeatureSchema::from_catalogue(&CatalogueConfig)`; `Genome::is_valid(&FeatureSchema) -> bool`.
  - `Sample { bar: qe_domain::Bar, funding: Option<Decimal>, open_interest: Option<Decimal>, premium: Option<Decimal> }`.
  - `CatalogueConfig { states: u16 }`, `Default = { states: 5 }`; `CATALOGUE_VERSION = 1`.
  - `FeatureVector { time_ms: i64, states: Vec<Option<QState>> }`.
- **qe-domain**: `Timestamp::from_millis(i64)`/`millis()`; `Resolution` `FromStr`/`as_str` (`"1h"`,`"5m"`,…); `InstrumentId::new` canonicalises to **UPPERCASE ASCII-alphanumeric** (rejects `-`, empty); `Bar::new(open_time,resolution,o,h,l,c,volume,trades)`; `Price::new(Decimal)`/`get`, `Qty::new(Decimal)/get`; `Direction::{Long,Short}`.

## Key decisions

1. **Catalogue schema sourcing (deviation from plan wording).** The plan assumed the vintage carries the
   catalogue schema. It does **not** — neither `Vintage`/`VintageContent`, `CalibrationProfile`, nor `Genome`
   persists `CatalogueConfig.states` or the feature count; the catalogue is defined entirely by
   `qe_signal::indicator::catalogue(CatalogueConfig)` with `Default{states:5}` and `CATALOGUE_VERSION=1` —
   the canonical schema training evolves against. So the bridge builds the schema from
   `CatalogueConfig::default()` and enforces compatibility by asserting **every chromosome `is_valid(&schema)`**
   (feature indices `< schema.len()`, state bounds `< num_states`). Any mismatch ⇒ `RunError::SchemaMismatch`.
   This is the strongest compatibility check the persisted artefact allows.
2. **Cost mapping.** `taker_fee_bps` → `FeeSchedule.taker = Decimal(taker_fee_bps)/10_000` (maker left at the
   VIP0 default; the backtester fills as taker). `slippage_model` is a **nominal label**: the string is
   **recorded verbatim** in `costs.slippage_model` (it captures the *requested* model), but the engine
   applies its default *linear* `SlippageModel` (spread + size-impact) regardless — so e.g.
   `"square-root-impact"` is a contract tag, **not** necessarily the friction the v1 engine actually applied.
   Documented so the label↔engine relationship is explicit, not silently dropped; wiring the label through
   to a real square-root-impact friction model is a future enhancement.
3. **Single-instrument ensemble (v1).** `backtest()` runs one genome over one bar series. v1 backtests the
   ensemble over the **first symbol in `--universe`** (errors if empty): every chromosome runs over that
   instrument's decision bars and per-bar returns are weight-aggregated by the vintage `weights`
   (`ens[t] = Σ_c w_c · ret_c[t]`). Multi-instrument portfolio aggregation is out of scope for v1 (noted).
   `TradeRow.symbol` = that instrument. Trades from all chromosomes are pooled, ordered by `(entry_idx, chromosome)`.
4. **Date parsing.** No `YYYY-MM-DD` parser exists in domain/config and no date crate is a dependency. A pure,
   dependency-free `days_from_civil` parser converts `YYYY-MM-DD` → epoch-ms (UTC midnight); bad dates ⇒
   `CliError::Usage` (parse time) / `RunError::BadDate` (job time). Deterministic, no wall-clock.
5. **Gross win-rate / profit-factor.** `win_rate`/`profit_factor` are computed from `TradeRow.return_pct`,
   which derives from `TradeFill.return_frac` — a **gross, price-only** figure (no qty/fees). The functions'
   docs state this cost-blind approximation explicitly (carried QE-252 review note). Net-of-cost accounting
   lives in the aggregate equity curve / CAGR / Sharpe, which use the backtester's net returns.
6. **`profit_factor` no-loss convention.** `Σgains / |Σlosses|`; `f64::INFINITY` when there are no losing
   trades (documented). Zero trades ⇒ `0.0`.
7. **`Ingest` enum variant scoped out.** Adding a parse-only `Command::Ingest` with no dispatch arm here would
   be a dead variant; the ingest job is QE-253. To keep clippy green and scope tight, `Ingest` is deferred to
   QE-253. Only `Command::Backtest` is added now.
8. **Fixture store portability.** The committed `MarketStore` is an LMDB dir built with a **small map_size
   (1 MiB)** so `data.mdb` is a few pages, not a sparse multi-GB file (git would materialise the holes). Both
   dev (arm64) and CI (x86_64) are 64-bit little-endian ⇒ the LMDB file is binary-compatible. The golden test
   **copies** the fixture store into a tempdir before opening (open() takes a write txn for schema init; never
   mutate the committed fixture). Reused by QE-253 per ticket.

## Determinism

Single-threaded; no wall-clock/RNG in any output. f64 ops are sequential and IEEE-754 (SSE2 on x86_64,
IEEE double on arm64 — no x87 extended precision), serialised via serde_json/ryū ⇒ byte-identical
`result.json` across platforms for identical inputs. Decimal→f64 via `ToPrimitive` is deterministic.

## Test plan (TDD — failing test first per plan Step 1s)

- `lib.rs`: `backtest_parses_required_and_optional_flags`, `backtest_requires_vintage`.
- `jobs/result.rs`: serde round-trip asserting exact §8.1 JSON keys.
- `jobs/metrics.rs`: hand-checked `equity_curve`, `drawdown`, `sharpe` zero-variance, `sortino`,
  `cagr`, `monthly_returns`, `win_rate`, `profit_factor` (incl. `INFINITY` no-loss).
- `jobs/features.rs`: schema-compat happy path + `SchemaMismatch` on an out-of-range genome.
- `tests/backtest_job.rs`: fixture store + vintage ⇒ golden `result.json` byte-exact; progress ends `done`.

## Risks

- **LMDB fixture portability** — mitigated by small map_size + 64-bit LE on both targets + tempdir copy.
- **Golden brittleness** — any metric formula change re-bakes the golden; acceptable (it *is* the contract lock).
- **Schema drift (partial guard — not fully caught).** `check_schema` uses `Genome::is_valid`, which only
  *bounds-checks* clause feature indices (`< schema.len()`) and state values (`< num_states`). So
  `SchemaMismatch` fires only on **out-of-range** drift (a genome addressing a feature/state the current
  catalogue no longer has). It does **not** detect *identity* drift that preserves width and `num_states` —
  a catalogue **reorder** (a clause index silently rebinds to a different indicator) or a `CATALOGUE_VERSION`
  bump of the same shape. These are undetectable today because the vintage persists neither
  `CATALOGUE_VERSION` nor `states`. **Recommended follow-up (tracked separately):** pin `CATALOGUE_VERSION` +
  `states` (or the full `FeatureSchema` fingerprint) in the vintage artefact so an exact schema-identity
  match can be verified at load, upgrading the guard from bounds-only to full drift detection.
