# QE-260 — Runnable `qe-cli train` search job + rich progress (design / evidence note)

`Phase: PreP3` · `Area: runnable jobs / wfo` · fast-follow to QE-251.
Branch `qe-260/cli-train-job`. Written **before** implementation (prove-it / TDD where practical).

## 1. Goal (from backlog)

Replace the `train` stub (today it only writes a `manifest.json`) with the **real** pipeline:
WFO/MAP-Elites **search** → **ensemble** construction → **validation** → **G1 gate** → **seal a
vintage** (via `qe-vintage`), streaming rich JSON-line progress (generation, MAP-Elites archive
coverage, CV folds, best-so-far fitness, G1 pass/fail). Deterministic for a fixed seed; single-threaded;
no wall-clock/RNG in the sealed output. Small, configurable budget so a test run is fast + deterministic.

## 2. Current-state evidence (verified by reading)

### 2.1 The stub being replaced
`crates/cli/src/lib.rs::run_train(cfg, code_commit) -> Result<Vintage, CliError>` today: resolves the
point-in-time universe (`cfg.universe()`), ensures the three storage dirs exist, builds a
`qe_determinism::Lineage::from_config(cfg, "", code_commit, vec![cfg.determinism.seed])`, and writes a
bespoke `VintageManifest` JSON to `<artifacts_dir>/vintages/<lineage-id>/manifest.json`. It does **not**
run evolution. Its return type `qe_cli::Vintage { id: VintageHash, manifest_path }` and the
`VintageManifest`/`InstrumentRecord` structs are QE-013-only and are removed here. The reusable parts
(universe resolution, dir creation, lineage from config+seed+commit) are **kept** and moved into the new
entry point.

### 2.2 The QE-251 job pattern being mirrored (`crates/cli/src/jobs/`)
- `mod.rs`: `ProgressLine` enum (`#[serde(tag="t", rename_all="snake_case")]`) with `Progress{pct,stage,
  msg}` / `Done{result}` / `Error{msg}`; `emit_progress/emit_done/emit_error`; `RunError` (runtime
  failures surfaced as the terminal `{"t":"error"}` line + non-zero exit).
- `backtest.rs`: `BacktestParams` + `run_backtest(&params, &mut progress) -> Result<Doc, RunError>`,
  coarse `progress(pct,stage,msg)` at load/scan/features/simulate/report; `main.rs` streams the lines,
  writes `result.json` into `--run-dir`, sets the exit code.
- `features.rs`: `catalogue_config() = CatalogueConfig::default()`, `catalogue_schema() =
  FeatureSchema::from_catalogue(&catalogue_config())`, `to_decision_bars(bars,funding,premium)`,
  `check_schema(chromosomes,&schema)`. **Reused verbatim** by the train job.

### 2.3 Engine APIs verified (signatures)
- **Search substrate** `qe_wfo`:
  - `mapelites::MapElitesArchive::new(FeatureSchema)`; `.insert(Genome,f64) -> Insertion`;
    `.direction(Direction) -> &DirectionArchive` (→ `occupied_cells()`, `cell(&Cell)->Option<&SubPopulation>`,
    `len()`); `.occupied_cells()->usize`; `.total_elites()->usize`. `SubPopulation::elites()->&[Elite]`,
    `Elite{genome,fitness}`.
  - `variation::VariationDriver::new(OperatorSelector, Direction)`; `.step(&mut archive,&schema,&mut
    DetRng, eval: Fn(&Genome)->f64) -> StepReport` — sequential, single-threaded, deterministic for a
    fixed rng. (The parallel `evaluate_batch`/`evaluate_and_insert` use rayon and are **not** used — the
    driver path is single-threaded, honouring the determinism constraint.)
  - `regularise::coverage(&archive, Direction) -> usize` (occupied cells in a direction) — the archive
    coverage metric the progress stream must carry.
  - `backtest::backtest(&Genome,&[Bar],&BacktestConfig) -> BacktestResult` (pure, no RNG);
    `BacktestResult{ returns:Vec<f64> (net-of-cost, bar0 excluded), accepted, fitness, .. }`;
    `.elite_fitness()` = `fitness.mean`. `BacktestConfig{friction,min_trades,windows}`.
- **Ensemble** `qe_ensemble::search::search_portfolio(pool:&[Vec<f64>], &SearchConfig, seed) ->
  SearchResult{ best:EnsembleMask, score, generations_run, history }`; `SearchConfig{pop_size,generations,
  cr,folds,init_density,objective}`; `EnsembleMask::members()->Vec<usize>`. Deterministic in `seed`
  (single seeded `DetRng`).
- **Validation** `qe_validation`: `assess(&VintageStats, &SpaConfig, seed) -> Result<RobustnessReport,
  ValidationError>`; `VintageStats{candidate_returns, trial_returns:&[Vec<f64>], excess_over_benchmark,
  n_trials, cscv_blocks}`; `effective_trials(cells,generations,windows)`; `sharpe_ratio(&[f64])`;
  `RobustnessReport{observed_sharpe,dsr,pbo,spa_pvalue,n_trials}`. `pbo_cscv` needs `blocks` even ≥2 and
  `trial_returns.len() ≥ blocks`.
- **G1 gate** `qe_gate` (QE-134): `split_with_embargo(n, holdout_len, embargo) -> Holdout{train,embargo,
  holdout}`; `evaluate_g1(in_sample_sharpe, holdout_returns, &RobustnessReport, &G1Criteria) ->
  G1Decision{promoted, criteria}`; `G1Criteria::with_defaults()` (min_holdout_sharpe 0, dsr>0.95,
  spa<0.05, oos_tol 0.5, min_holdout_samples 30). `G1Decision::failed_criteria() -> Vec<&str>`.
- **Seal** `qe_vintage`: `VintageContent{format_version,vintage_id,chromosomes,weights,calibration,
  worst_case_loss:Option<f64>,lineage}`; `Vintage::seal(content) -> Result<Vintage,_>` (validates weight
  alignment + finiteness, pins content hash); `VintageRepository::new(root).write(&vintage) -> path`
  (`<root>/<vintage_id>.json`); `.verify()`.
- **Calibration** `qe_risk::CalibrationProfile::new(Fraction)`, `Fraction::new(Decimal)->Result`.
- **Determinism** `qe_determinism`: `Lineage::from_config`, `.id()->Result<String>`; `seed_rng(seed)`,
  `task_rng`, `DetRng`.

## 3. Catalogue-schema alignment with QE-251 (critical)

The backtest job builds decision bars against `catalogue_schema() =
FeatureSchema::from_catalogue(&CatalogueConfig::default())` and `CatalogueConfig::default() == { states: 5 }`
(verified `crates/signal/src/indicator/mod.rs:143`). **The train job evolves genomes against the same
`catalogue_schema()`** (same `features.rs` helper) and builds its decision bars with the same
`to_decision_bars`. Therefore a vintage sealed by `train` addresses exactly the features/states the
backtest job assembles, so QE-251's `check_schema` passes and the vintage is backtestable. This is proved
directly in the integration test: after sealing, the test runs `run_backtest` over the sealed vintage on
the same store window and asserts `Ok`. (Persisting `CATALOGUE_VERSION`/`states` inside the vintage
remains the separately-tracked follow-up from QE-251 — **not** expanded here.)

## 4. Decisions

- **D1 — Job shape.** New `crates/cli/src/jobs/train.rs`: `TrainParams` (plain data: store path + map
  size, artifacts/vintage root, instrument, window start/end/resolution, budget, a pre-built `Lineage`,
  profile label) + `run_train_job(&TrainParams, &mut dyn FnMut(ProgressLine)) -> Result<TrainOutcome,
  RunError>`. `lib.rs::run_train(cfg, &TrainOptions, code_commit, emit)` keeps the config/universe/lineage/
  dir responsibilities and delegates the pipeline to the job (mirrors how `run_backtest_command` builds
  `BacktestParams`).
- **D2 — Progress schema (train-specific, additive; QE-261 consumes it).** Extend `ProgressLine` with
  new serde-tagged variants (backtest's `Progress/Done/Error` untouched; `Done` gains an optional
  `vintage` field that is **omitted when `None`**, so the existing `{"t":"done","result":"result.json"}`
  byte-shape is preserved):
  - `{"t":"gen","pct":u8,"stage":"search","generation":usize,"generations":usize,"coverage":usize,
    "coverage_long":usize,"coverage_short":usize,"best_fitness":f64}` — one per generation.
  - `{"t":"ensemble","pct":u8,"stage":"ensemble","folds":usize,"members":usize,"score":f64}` — after the
    portfolio search (carries the **CV folds**).
  - `{"t":"gate","pct":u8,"stage":"gate","promoted":bool,"failed":[String],"in_sample_sharpe":f64,
    "holdout_sharpe":f64,"dsr":f64,"spa_pvalue":f64,"n_trials":usize}` — the **G1 pass/fail** with
    evidence.
  - terminal `{"t":"done","result":"result.json","vintage":"<vintage_id>"}` names the sealed vintage;
    `{"t":"error","msg":..}` on failure. Helpers `emit_gen/emit_ensemble/emit_gate/emit_train_done`.
- **D3 — Budget + small defaults / flags.** `train` flags: `--run-dir`, `--json`, `--start/--end/
  --resolution`, and budget `--seed` (default `cfg.determinism.seed`), `--generations` (default 8),
  `--population` (variation steps/generation, default 24), `--holdout` (bars, default 30), `--embargo`
  (bars, default 2). Small defaults keep a fixture run < 1s. Instrument = first configured universe
  symbol (no extra flag). Train `BacktestConfig` uses `min_trades = 1`, `windows = 2` so short fixture
  series produce finite fitness (documented small-budget config; the *sealed* genomes are still evolved
  net-of-cost).
- **D4 — Search loop (single-threaded, deterministic).** Split decision bars once via
  `split_with_embargo(n_bars, holdout, embargo)` → `train_bars` / `holdout_bars`; the search sees only
  `train_bars` (leakage-free — the holdout is the G1 slice). One `MapElitesArchive` over
  `catalogue_schema()`; two `VariationDriver`s (Long, Short) sharing it; a single `DetRng =
  seed_rng(seed)`. Per generation: `population` driver steps per direction with `eval = |g|
  backtest(g,&train_bars,&train_cfg).elite_fitness()`; then emit a `gen` line with per-direction
  `coverage` and running `best_fitness`.
- **D5 — Ensemble + validation.** Pool = every archive elite's net-of-cost `returns` over `train_bars`
  (deduped by genome; finite-length aligned). `search_portfolio(&pool, &SearchConfig{small}, seed)` →
  selected members; **equal weights** `1/k` (capacity-capping is out of scope — documented). Candidate
  in-sample returns = equal-weight combine of selected members over `train_bars`; candidate holdout
  returns = the same genomes combined over `holdout_bars`. `n_trials = effective_trials(occupied_cells,
  generations, windows)`. Benchmark for SPA excess = per-bar buy-&-hold price return of the instrument;
  `excess[k] = pool[k] − benchmark`. `cscv_blocks = 2` (min; small budget). If the pool has fewer than
  `cscv_blocks` trials (degenerate tiny run), skip `assess` and use a conservative
  `RobustnessReport{dsr:0,pbo:1,spa_pvalue:1}` so the gate still runs — the pipeline never aborts on a
  thin archive.
- **D6 — G1 + seal.** `evaluate_g1(sharpe(in_sample), &holdout_returns, &robustness,
  &G1Criteria::with_defaults())`. The vintage is **sealed regardless of the G1 verdict** (the verdict is
  recorded in the progress `gate` line and the `result.json` sidecar, not inside the artefact — the
  fixed `VintageContent` schema has no G1 field; persisting it is out of scope). `vintage_id =
  lineage.id()` (64-hex) → deterministic and independent of the stochastic search; `content = {selected
  chromosomes, equal weights, default CalibrationProfile(0.1), worst_case_loss: None (QE-130 stress feeds
  G3, out of scope here), lineage}`; `Vintage::seal` + `VintageRepository::new(<artifacts>/vintages)
  .write`. `main.rs` writes a `TrainResultDoc` to `<run-dir>/result.json` (vintage id, budget, final
  coverage, best fitness, selected members + weights, the full `G1Decision`, the `RobustnessReport`) and
  emits the terminal `done`.
- **D7 — Determinism.** Every stochastic step is seeded off the config seed and single-threaded
  (`VariationDriver::step`, `search_portfolio`, `assess`'s bootstrap). `backtest` is pure. So the sealed
  content (chromosomes + weights + lineage) is byte-reproducible → identical `content_hash`, and the
  `vintage_id` (lineage id) is identical. Same-seed determinism is asserted on **both** the vintage id and
  the content hash.

## 5. Firewall

`train` lives in `qe-cli` (the composition root already depending on both `qe-runtime` and the training
crates). New deps added to `qe-cli` only: `qe-gate`, `qe-validation`, `qe-risk` (promoted from dev-dep).
`crates/cli/tests/dependency_topology.rs` guards only `qe-runtime`/`qe-wfo`/`qe-ensemble`/`qe-server`
edges — none touched. `qe-runtime` gains no training deps. `cargo test -p qe-architecture --test
firewall` stays green.

## 6. Test plan (TDD)

New `crates/cli/tests/train_job.rs` over a **copy** of the committed QE-251 sample store fixture
(`tests/fixtures/sample_store/`, BTCUSDT 1h, 120 bars), fixed seed, tiny budget:
1. `train_over_fixture_store_seals_verifiable_vintage`: run the job; assert a vintage exists at
   `<artifacts>/vintages/<id>.json`, `VintageRepository::load(id)` + `verify()` pass, chromosomes are
   `check_schema`-valid, and the collected progress stream contains a `gen` line (with coverage), an
   `ensemble` line (folds), and a `gate` line (G1 result). The job returns a `TrainOutcome` with the
   vintage id.
2. `sealed_vintage_is_backtestable_by_qe251`: load the sealed vintage and run `run_backtest` over the
   same store window → `Ok` (direct catalogue-schema alignment proof).
3. `train_is_deterministic_for_fixed_seed`: two runs, same seed ⇒ identical vintage id **and** identical
   `content_hash` (byte-identical lineage + content).
Existing `crates/cli/tests/train.rs` (QE-013) is updated: the manifest-specific assertions are replaced
with the new job behaviour; `example_config_loads_and_validates` and `dockerfile_runs_the_same_binary`
are kept. QE-251 golden (`backtest_job.rs`) and QE-253 (`ingest_job.rs`) tests are untouched → no
regression.

## 7. Risks

- **Thin archive on a tiny budget** → too few ensemble trials for CSCV. Mitigated by D5 (`cscv_blocks=2`
  + conservative-report fallback) and `min_trades=1` train config so genomes actually trade over 120 bars.
- **G1 will typically NOT promote** on a 120-bar fixture (strict defaults; DSR/SPA). This is expected and
  acceptable: the AC requires the gate *result* in the stream, not a pass. The vintage is sealed
  regardless (D6).
- **Non-determinism traps**: any accidental use of the rayon `evaluate_batch` path or an unseeded RNG.
  Avoided by using the sequential `VariationDriver` and seeded `search_portfolio`/`assess`; asserted by
  the determinism test.
</content>
</invoke>
