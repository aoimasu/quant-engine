# QE-261 — Training-monitor UI screen — design / evidence note

`Phase: PreP3` · `Area: frontend (+ server wiring)` · `Depends on: QE-259, QE-260` · Spec §10 (spec 4)

## Goal

Trigger a **training** run and monitor it live to a **G1 verdict** from the admin UI, then deep-link to
the produced vintage's backtest. Two parts:

- **Part A (server):** extend the QE-255 run lifecycle so a `type:"train"` run spawns `qe-cli train`
  (not `backtest`) and the supervisor captures the QE-260 **rich progress** (`gen`/`ensemble`/`gate` +
  the terminal `done`'s sealed `vintage` id) into `meta.json` for polling. Backtest runs unchanged.
- **Part B (frontend):** a net-new Training screen (the design kit has none) composed from the ported
  design system — a trigger form → `POST /api/runs {type:"train"}`, live progress via polling
  `GET /api/runs/:id`, the G1 verdict on completion, and a link into the QE-259 New-backtest flow for
  the sealed vintage.

## Current-state evidence

### QE-255 run lifecycle + supervisor (`crates/server/src/runs/*`)
- `POST /api/runs` body is `CreateRunRequest { type, params }` (`model.rs`). Today `params` is a typed
  `BacktestParams` (every field `#[serde(default)]` → lenient parse; required-ness enforced uniformly in
  `manager::validate` as a 400). `validate` **rejects any `type != "backtest"`**.
- `RunMeta` (authoritative `meta.json`) carries `params: BacktestParams`, `progress: Progress
  {pct,stage,msg}`, status, timestamps, `exit`, `error`, `artifacts`.
- `CliJobSpawner::spawn(run_dir, &BacktestParams)` builds `qe backtest … --run-dir <dir> --json` and pipes
  stdout/stderr; the injectable `JobSpawner` seam lets tests swap the binary for a `/bin/sh` fake.
- `manager::supervise` acquires a pool permit, spawns, and `drain_stdout` tails JSON lines: it parses a
  **local** `ProgressLine` enum (mirrored, NOT depending on `qe-cli`, to keep the firewall clean) that
  only understands `progress`/`done`/`error`. It stores `progress` into `meta.progress` and treats `done`
  + exit 0 as success (`artifacts=["result.json"]` when present).
- Tests (`crates/server/tests/runs.rs`) drive the **real** store/pool/supervision with a `/bin/sh` fake
  job through the real `JobSpawner` seam; status polled with a bounded timeout.

### QE-260 train job + progress schema (`crates/cli/src/jobs/{train.rs,mod.rs}`, `lib.rs`)
The train CLI streams a serde-`tag="t"` `ProgressLine` stream the UI consumes:
- `gen`: `pct, stage="search", generation, generations, coverage, coverage_long, coverage_short,
  best_fitness` (best_fitness can be `-inf` early → serde_json emits `null`).
- `ensemble`: `pct, stage="ensemble", folds, members, score`.
- `gate`: `pct, stage="gate", promoted, failed[], in_sample_sharpe, holdout_sharpe, dsr, spa_pvalue,
  n_trials` (the real G1Decision).
- terminal `done`: `result="result.json"` **+ `vintage`** (the sealed vintage id; `Option`,
  `skip_serializing_if` so backtest's `done` byte-shape is unchanged), or `error`.
The `qe train` command flags: `--config --profile --run-dir --json --start --end --resolution --seed
--generations --population --holdout --embargo`. Store path, artifacts (vintage) root, and the
**instrument/universe** come from the **config file** (`cfg.instruments.first()`), not flags — mirroring
how the backtest CLI resolves store/vintage from config.

### QE-259 SPA (`web/src/…`)
- `api/runs.ts`: `RunMeta`, `Progress`, `createRun` (`POST {type:'backtest', params}`), `getRun`,
  `listRuns`, `getRunResult`, `listVintages`, `getCoverage`. Resilient polling in `BacktestResult`
  (`POLL_MS`, bounded `MAX_POLL_FAILURES`, streak reset).
- `BacktestsArea` = a router-less view-state machine (list/new/result). `App` switches screens on the
  `active` nav id; `AppShell` `NAV` has the Research group (Strategies/Backtests/Market data) + disabled
  Trade/Risk placeholders. CSP-safe: hand-rolled SVG, `injectCss`, no runtime CDN, bundled `lucide-react`.

## Decisions

### Part A — dispatch train vs backtest + capture rich progress
1. **Typed params without breaking backtest wire shape.** `CreateRunRequest.params` and
   `RunMeta.params` become `serde_json::Value` (exactly preserves the backtest `meta.params` byte-shape;
   no ambiguous untagged-enum round-trip). An internal, non-serialized `RunSpec { Backtest(BacktestParams)
   | Train(TrainParams) }` drives spawning. `manager::create` dispatches on `type`: parse the Value into
   the typed params, `validate`, build a `RunSpec`, store `meta.params = to_value(params)`, and pass the
   `RunSpec` to the supervisor/spawner. Any parse/validation failure is a uniform **400** (never 500/422).
2. **`TrainParams`** (server): `start,end,resolution` (required, validated non-empty like backtest's
   window) + optional `seed,generations,population,holdout,embargo` (CLI has defaults) + optional
   `config,profile`. No `--universe` flag exists on `qe train` (universe is config-derived) — not touching
   the QE-260 CLI, so the server does not synthesize one.
3. **`JobSpawner::spawn(run_dir, &RunSpec)`.** `CliJobSpawner` matches: `Backtest` → today's argv
   verbatim; `Train` → `qe train --run-dir <dir> --json --start --end --resolution [--seed]
   [--generations] [--population] [--holdout] [--embargo] [--config] [--profile]`.
4. **Rich progress capture.** The supervisor's local `ProgressLine` gains `gen`/`ensemble`/`gate` variants
   and `done { vintage }`. Non-finite floats are `Option<f64>` (serde_json emits `null` for `-inf`/NaN, so
   a required `f64` would drop the whole line). `RunMeta` gains `train: Option<TrainProgress>`
   (`skip_serializing_if=None` → backtest `meta.json` unchanged) holding the **latest** `generation`
   /`ensemble`/`gate` snapshot + the sealed `vintage`. Each rich line also advances the generic
   `meta.progress` (pct + synthesized msg) so the shared progress bar/list still move.
5. **Vintage id exposure.** On the terminal `done`, `meta.train.vintage` is set from the line — so
   `GET /api/runs/:id` exposes the sealed id for the deep-link without the UI parsing `result.json`.

### Part B — the Training screen (composed from the design system)
- `api/runs.ts`: add `TrainParams`, `TrainProgress` (+ `GenSnapshot`/`EnsembleSnapshot`/`GateSnapshot`),
  optional `RunMeta.train`, and `createTrainRun(params)` (`POST {type:'train', params}`).
- `web/src/app/training/`: `TrainingArea` (list/new/monitor view-state, mirroring `BacktestsArea`),
  `NewTraining` (window/resolution/seed/budget form → `createTrainRun`, client validation + inline 400),
  `TrainingMonitor` (resilient polling of `getRun`, same pattern as `BacktestResult`): generations +
  progress bar, the **MAP-Elites archive-coverage grid** (long/short occupied cells), **CV folds**
  (ensemble), **best-so-far** fitness, and the **G1 verdict** (pass/fail badge + failed criteria +
  sharpes/DSR/SPA). `TrainingList` filters `listRuns()` to `type==='train'`.
- **Deep-link:** on completion the monitor shows the G1 verdict + a "Backtest this vintage" button. Cross-
  area nav is lifted into `App`: a callback sets `active='backtest'` and passes the vintage into
  `BacktestsArea → NewBacktest` (new optional `initialVintage` prop, backward-compatible) which preselects
  it. `AppShell.NAV` gains a Research **Training** item (new `activity` glyph in the `Icon` registry);
  Trade/Risk stay disabled.
- CSP-safe like QE-258/259: `injectCss`, bundled icons, no runtime CDN.

## Firewall / deny
- No new crate deps. `qe train` is **spawned** as a subprocess (not linked), same as backtest → no
  `qe-runtime`/`qe-venue` edge. Async stays confined to `qe-server`. `cargo deny` unaffected.

## Test plan
- **Rust:** add `crates/server/tests/runs.rs` cases — a `type:"train"` run through the real spawner with a
  `/bin/sh` fake emitting `gen`/`ensemble`/`gate`/`done{vintage}` → poll to `succeeded`, assert
  `meta.train.{generation,ensemble,gate,vintage}` + `artifacts=["result.json"]`; a train run missing the
  window → 400; backtest cases still pass. Unit: `store.rs` `sample_meta` updated to `to_value` params.
- **Frontend (Vitest/RTL):** `NewTraining` POSTs `{type:'train', params}`; `TrainingMonitor` renders live
  generations/coverage/folds/best-so-far from a mocked polling sequence and, on completion, shows the G1
  verdict + the "Backtest this vintage" link carrying the vintage id.
- Gates: `fmt`/`clippy -Dwarnings`/`test --workspace`/`firewall`/`deny`; `npm ci && build && lint && test`.

## Risks
- **Non-finite floats** in the progress stream → mitigated by `Option<f64>` on every float snapshot field.
- **Untagged params ambiguity** (both param structs are all-default) → avoided by storing `meta.params` as
  a `Value` + an internal typed `RunSpec`, never an untagged enum.
- **Config-derived training universe:** the spawned `qe train` resolves universe/store from config.toml at
  the server CWD (same implicit deploy contract as the QE-255 backtest spawner) — documented, not a
  regression.
</content>
</invoke>
