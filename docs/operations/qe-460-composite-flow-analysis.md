# QE-460 — Composite `RunSpec::Flow` + frozen-holdout carve — evidence note

Design ref: `docs/architecture/qe-455-research-flow-design.md` §5 (composite lifecycle) + §4 (frozen OOS
holdout contract). Ticket: `docs/mds/tickets/QE-460.md`. Depends on QE-467 (lineage schema), QE-458 (steer
whitelist), QE-419 (config pin), QE-117 (WFO windows), QE-125 (regime labels).

## Current state (real anchors)

- **`RunSpec` enum** — `crates/server/src/runs/model.rs:49` (`Backtest | Train | Evolve`). Arms:
  `run_type()` `:62`, `params_value()` `:72`, `label()` `:82`, `writes_vintage()` `:93` (only `Train`).
- **`build_spec` dispatch** — `crates/server/src/runs/manager.rs:355` (`"backtest"|"train"|"evolve"`), with
  `validate_backtest` `:400`, `validate_train` `:421`, `validate_evolve` `:500`. Reusable helpers:
  `require` `:392`, `reject_if_present` `:472`, `floor_usize` `:483`. `validate_train` already enforces the
  QE-458 whitelist/blocklist + holdout/embargo/purge/windows/folds floors and rejects evolved-pool steering.
- **Supervisor** — `supervise()` `manager.rs:561` runs ONE child: acquire pool permit (+ evolve permit for
  evolve), spawn via `JobSpawner::spawn`, `drain_stdout`/`drain_stderr_tail`, terminal status from
  `done_seen && exit==0 && result.json exists`. `create()` `:156` writes `meta.json`+`index.json` and spawns
  a registered supervisor task.
- **Spawn seam** — `crates/server/src/runs/spawn.rs`. `JobSpawner` trait `:17` (ONE impl, `CliJobSpawner`);
  `spawn()` `:64` matches the spec and builds argv (`train_args` `:113`, `backtest_args` `:86`). QE-419
  config pin via `with_config_path` `:52`. Tests inject a fake `/bin/sh` script (`QE_SERVER_CLI_BIN`) — the
  server tests never build `qe-cli`.
- **Run-protocol** — `crates/run-protocol/src/lib.rs`. `PROTOCOL_VERSION = 2` `:31`; assertion
  `protocol_version_is_two` `:638`; `done_stamps_protocol_version_and_error_line` `:617` hard-codes
  `"protocol_version":2`. `TrainParams` `:357` (steer knobs + blocklist probes); `EvolveParams.seed`
  required `:495`.
- **Train CLI job** — `crates/cli/src/jobs/train.rs`. Splits `train|embargo|holdout` via
  `split_with_embargo(decision_bars.len(), holdout, embargo)` `:368` BEFORE search. Seals
  `VintageContent` `:715` incl. QE-467 `provenance` `:709` (currently only `data_provenance=Real` +
  `steer_delta`; holdout_split/regime/consultation left default). `TrainResultDoc` `:209` (has
  `instrument`, `window`). `TrainParams` (job) `:137`. CLI wiring: `TrainOptions` `crates/cli/src/lib.rs:60`,
  `Command::Train` `:269`, arg parse `:396`, `run_train` builds job params `:135`.
- **QE-467 lineage schema** — `crates/vintage/src/lib.rs`: `ResearchProvenance` `:238`
  (`holdout_split`, `regime_composition: Vec<RegimeShare>`, `consultation_count: u64`, `steer_delta`),
  `HoldoutSplit` `:147` (`holdout_range/train_range: Option<TimeRange>`, `embargo_bars: u64`),
  `TimeRange` `:137`, `RegimeShare` `:161` (`regime: String`, `bars: u64`). Part of hashed content →
  populating changes the content hash. `VINTAGE_FORMAT_VERSION = 8` `:50` — **not bumped here**.
- **QE-125 regime labels** — `crates/signal/src/regime.rs`: `label_regimes(&[Bar], &RegimeConfig) ->
  Vec<Option<Regime>>` `:101`, `Regime{vol,trend}` `:45`, deterministic. `qe-signal` is already a `qe-cli`
  dep (train job imports `qe_signal::CatalogueIdentity`).
- **QE-117 WFO windows** — `crates/wfo/src/walkforward.rs` `WalkForward::windows()` `:74`; the train job's
  fold geometry uses `selection_kfold`/`fold_test_ranges` (`qe_wfo::cv_fitness`).
- **Sealed cost calibration** — the train gate prices with `BacktestConfig::default().friction` (derived
  from `SlippageCalibration::default()`), sealed as `VintageContent.slippage`. The backtest CLI job's
  `backtest_config(2.0, None)` `crates/cli/src/jobs/backtest.rs:199` defaults its slippage to the **same
  selection cost model** (asserted by its own test `:396`). So a backtest sub-job built with default
  `taker_fee_bps=2.0` + default `slippage_model` re-costs the holdout under the identical friction the gate
  used → cost parity is structural.
- **Compiled floors** — `crates/validation/src/steer.rs`: `HOLDOUT_FLOOR=250`, `EMBARGO_FLOOR=1`,
  `PURGE_FLOOR=1`, `MIN_WFO_WINDOWS=4`, `MIN_WFO_FOLDS=2`, `MIN_OCCUPIED_NICHES=5`.
- **Firewall** — `crates/architecture/tests/firewall.rs`. `qe-server` already deps `qe-vintage`,
  `qe-run-protocol`, `qe-config`, `qe-validation`. No new crate edge is introduced.

## Implementation decisions

**Division of labour (server-owned sequencing over existing CLI sub-jobs).**
1. **Protocol (`qe-run-protocol`).** Bump `PROTOCOL_VERSION 2→3`; update the assertion + the two tests. Add
   `FlowParams` DTO: required `seed` (no serde default, mirrors `EvolveParams.seed`) + required window
   (`start`/`end`/`resolution`), every other field `#[serde(default)]`. It carries the QE-458 steer-whitelist
   knobs + blocklist probes + `holdout`/`embargo`/`purge` — i.e. it is a superset that yields a `TrainParams`
   via `to_train_params()`. This lets `validate_flow` **reuse** `validate_train` verbatim (no divergence).
2. **Server model (`RunSpec::Flow`).** Enum arm + `run_type()=="flow"`, `writes_vintage()==true`,
   `label()`=flow window, `params_value()`. A `FlowProgress` sub-record on `RunMeta`
   (`train_run`/`backtest_run` sub-run dir ids, `vintage`, `holdout_window`) — one run-store row, one status.
3. **`validate_flow`.** `require` window + serde-required seed; then `validate_train(&to_train_params())`
   reuses the whitelist/blocklist/floor logic (incl. holdout/embargo floors, §4). Uniform `400`.
4. **Server sequencing (`supervise` Flow arm).** Under one flow run dir: (a) spawn the **train** sub-job with
   the carved `--holdout/--embargo` + `--flow` into `<flow>/train`, tail it; require a sealed vintage (from
   the `done` line) — a train that fails G1 seals nothing → the flow fails and runs **NO** backtest; (b) read
   the train sub-run `result.json` (as `serde_json::Value`, no new crate edge) for `{vintage_id, instrument,
   holdout_window}`; (c) **cost-parity guard**: build the backtest `BacktestParams` with the pinned default
   cost model (taker_fee_bps=2.0, default slippage_model) = the sealed model, and assert equality before
   spawn; (d) spawn the **backtest** sub-job over the frozen holdout window with the sealed vintage id into
   `<flow>/backtest`, tail it. Flow `Succeeded` iff both sub-runs succeeded; sub-run ids recorded in
   `meta.flow`.
5. **Holdout carve + lineage recording (train CLI `--flow` mode only).** The split **policy** (holdout bars
   + embargo) is server-owned + floored (frozen once, before search; handed identically to both sub-runs —
   train applies it, the backtest rides the recorded range). In `--flow` mode the train seal **populates**
   QE-467's fields (byte-changing only the flow vintage; plain `train` stays byte-identical because the flag
   is off): `holdout_split{holdout_range, train_range, embargo_bars}` (dates from the actual split-boundary
   bar timestamps — server-derived from the pinned snapshot's right edge), `regime_composition` (QE-125
   `label_regimes` over the holdout bars → `Vec<RegimeShare>`), and `consultation_count` (overlap-keyed, see
   below). The resolved `holdout_window` is also written into `result.json` for the server handoff.

**Holdout geometry (b).** v1 keeps a **single trailing holdout** whose edge is the pinned snapshot's right
edge (server-derived, not operator-chosen). The **geometry assertion is keyed on named regimes + bars**:
the flow train asserts the holdout spans **≥ K distinct QE-125 regime labels** (`K = MIN_HOLDOUT_REGIMES =
2`, conservative — 4 regimes exist) **and** the holdout floor (`HOLDOUT_FLOOR = 250` bars, already enforced
by `validate_flow`). Falling below K is a hard `RunError` → the flow fails. This **closes QE-458's deferred
AC(d)** ("stress-regime + OOS-span-in-bars") *for the flow path* (keyed on named regimes + bar span); plain
`train`'s separate deferral is untouched (out of scope here).

**Consultation counter (c) — overlap-keyed.** At flow-seal the train job lists the existing vintage repo
(`VintageRepository::list`), reads each sealed vintage's `provenance.holdout_split.holdout_range`, and counts
those whose holdout **intersects** this run's holdout (bar-timestamp interval overlap, not exact equality) OR
whose `train_range` covers this holdout. `consultation_count = 1 + overlaps` (this run is the current
consultation). Deterministic given repo state → a fresh-repo reproduction yields a stable count (the QE-006
determinism reproduction runs in isolation). Recorded into `provenance.consultation_count`.

**Single-consultation contract (a).** The flow's backtest runs **on** the frozen holdout window (not a
disjoint OOS window). It re-surfaces the gate's holdout evaluation; it confers no independent deflation
credit (no gate/seal re-run — the backtest is a report over the already-sealed vintage). Documented in code
+ the design already dropped the "disjoint" language (§4/§11 risk 10).

**Cost parity (d).** The backtest sub-job is pinned to the sealed default cost model; the server asserts the
constructed `BacktestParams` cost fields equal the pinned model before spawn (maxdama #6). A CLI test already
proves `backtest_config` default impact == the selection cost model.

**Determinism.** The flow `seed` is the train seed (drives the search) and the backtest is deterministic. A
fresh-repo re-run from `seed` + pinned snapshot reproduces the flow vintage byte-identically (content hash).

## Test plan (per AC)

- **AC1 atomic sequencing / failed-G1-no-backtest** — server test (fake spawner): flow create → train
  sub-job emits `done`+vintage & writes `result.json` → backtest sub-job spawned over the holdout window →
  flow `Succeeded`, `meta.flow` carries both sub-run ids. Second test: train sub-job emits no vintage (G1
  fail) → flow `Failed`, backtest never spawned (asserted via the fake script's marker file absent).
- **AC2 single consultation** — code/doc assertion + CLI test that the flow backtest window equals the
  recorded holdout range (it evaluates ON the holdout).
- **AC3 regime-stratified holdout + floors** — CLI test: `--flow` seal records `holdout_split` +
  `regime_composition` (≥ K labels) into the vintage; a holdout below K regimes errors. `validate_flow`
  rejects sub-floor holdout/embargo (`400`).
- **AC4 overlap-keyed consultation** — CLI test: seal vintage A (holdout H1), then a flow whose holdout
  intersects H1 records `consultation_count == 2`; a disjoint holdout records `1` (intersection, not
  equality).
- **AC5 cost parity** — server unit test: the backtest `BacktestParams` the flow builds carry the pinned
  default cost model; a tampered cost model fails the parity guard.
- **AC6 determinism** — CLI test: two `--flow` seals from the same seed+fixture in fresh repos produce
  byte-identical content hashes.
- **AC7 PROTOCOL_VERSION==3 + no vintage bump + firewall** — protocol assertion test updated; firewall test
  stays green (no new edge); `VINTAGE_FORMAT_VERSION` unchanged at 8.
- **`validate_flow`** unit tests mirror the `validate_train` suite (window/seed required, whitelist accept,
  blocklist reject, floor reject).

## Risks

- **Content-hash drift on plain train.** Mitigated: holdout/regime/consultation fields are populated ONLY in
  `--flow` mode; plain `train` seals with the default (empty) provenance holdout fields → byte-identical, no
  golden move. Verified by the determinism harness staying green.
- **bar↔date handoff.** The train sub-job (which owns bar↔date) resolves the holdout window and records it;
  the server reads it from `result.json` — no server-side bar math, no new crate edge.
- **Consultation count vs determinism.** Count depends on repo state; the byte-identical reproduction
  guarantee is for a fresh-repo reproduction (QE-006 isolation), documented.
- **K/N/floor defaults undefined in repo (FLAGGED).** `K = MIN_HOLDOUT_REGIMES = 2` (new const; conservative
  — 4 regimes exist), `N` folds ← `MIN_WFO_FOLDS = 2`, holdout floor ← `HOLDOUT_FLOOR = 250`, embargo floor
  ← `EMBARGO_FLOOR = 1` (all reused compiled floors). Flagged for product confirmation.
</content>
</invoke>
