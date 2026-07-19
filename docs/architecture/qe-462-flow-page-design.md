# QE-462 — SPA single stepped Flow page (evidence note)

> Ticket: `docs/mds/tickets/QE-462.md`. Design ref: `docs/architecture/qe-455-research-flow-design.md`
> §5 (composite flow), §14 Q6 (stepped page vs dedicated area). Depends on QE-460 (`RunSpec::Flow`
> + `FlowParams`), QE-459 (steering controls), QE-457 (Vintage Inspector), QE-467/456 (`GET
> /api/vintages/{id}`). Branch: `qe-462/spa-flow-page`.

## 1. Current state (real paths + contracts)

### Server contract (source of truth the SPA must match exactly)
- **`FlowParams`** — `crates/run-protocol/src/lib.rs:559`. The `params` object of a `type:"flow"`
  create-run request. `seed: u64` is **required** (no serde default, mirrors `EvolveParams`);
  `start`/`end`/`resolution` are `#[serde(default)]` but enforced-present as a uniform `400` by
  `validate_flow`. Every other field is `#[serde(default, skip_serializing_if="Option::is_none")]`:
  the QE-458 whitelisted steer block (`generations`, `population`, `holdout`, `embargo`,
  `indicator_subset`, `windows`, `folds`, `config`, `profile`) **plus** the blocklist probes
  (`cost_stress_multiplier`, `max_turnover_frac`, `capacity_floor_usd`, `dsr_cutoff`, `pbo_cutoff`,
  `ic_fdr_threshold`, `purge`, `evolved_pool`, `evolved_formulas`) that `validate_flow` **rejects**
  if present. The backtest window is **not** operator-chosen — it is the server-frozen holdout the
  train phase carves; there is no separate backtest-window field on the wire.
- **`validate_flow`** — `crates/server/src/runs/manager.rs`: `validate_train(&p.to_train_params())`.
  The flow reuses the train whitelist/blocklist/floors verbatim (holdout ≥ 250, embargo ≥ 1,
  windows ≥ 4, folds ≥ 2; blocklist knobs are hard `400`). So the SPA must submit **only** the
  whitelisted fields + `seed` + window, never a blocklisted threshold.
- **`RunMeta.flow: Option<FlowProgress>`** — `crates/server/src/runs/model.rs:215,258`. The single
  flow `meta.json`'s supervision record: `train_run`, `backtest_run` (sub-run ids), `vintage`
  (content-hash handoff / Inspector deep-link target), `holdout_start`, `holdout_end`. Per-phase
  progress is derived from which of these are set + `meta.status` + coarse `meta.progress`.
- **`type:"flow"` create** posts exactly like train/evolve: `POST /api/runs {type, params}` — see
  `postRun` in `web/src/api/runs.ts:536`. `run_type()` maps `RunSpec::Flow` → `"flow"`
  (`model.rs:72`).

### Frontend (reused, not rebuilt)
- **QE-459 steering controls** — `web/src/app/training/NewTraining.tsx`: the catalogue indicator
  picker, search budget (generations/population), WFO windows/CV folds, holdout/embargo inputs, the
  projected distinct-trial `N` feedback, the disabled evolved-pool affordance, and the disabled
  **compiled-floor guardrail chips** (`GUARDRAIL_FLOORS`). Client floors mirror
  `crates/validation/src/steer.rs` (only *remove* affordances; server is the enforcement point).
- **QE-457 Inspector** — `web/src/app/strategies/VintageInspector.tsx`: the "backtest-holdout only —
  not paper-confirmed" `Callout` and the frozen-holdout **regime composition** list
  (`.qe-vi__regimes`, `regime + bars + %`). Consumes `GET /api/vintages/{id}` (`getVintage`).
- **Run monitor pattern** — `web/src/app/training/TrainingMonitor.tsx` + `usePollingRun`
  (`web/src/api/usePollingRun.ts`): polls `GET /api/runs/:id` until terminal, bounded retry.
- **Area/view machines** — `TrainingArea.tsx` (list/new/monitor), `StrategiesArea.tsx`
  (list/inspect), `BacktestsArea` (`initialVintage` deep-link). App wiring + nav: `App.tsx`,
  `web/src/design/AppShell.tsx`.
- **API client** — `web/src/api/runs.ts`: `RunMeta` discriminated union (`backtest|train|evolve`),
  `TrainParams`, `createTrainRun`, `getVintage`, `VintageDetail` (with `regime_composition`,
  `holdout_split`).

## 2. Implementation decisions

- **No new `web/src/app/flow/` area** (design §14 Q6 lean choice; ticket out-of-scope). The stepped
  Flow page + monitor live **inside the Training area** and reuse its view machine. Entry: a "New
  flow run" button on the Training list. No flow *list/browser* (out of scope).
- **Reuse via extraction, single source of truth.** Extract the QE-459 steering controls into
  `web/src/app/training/steer.tsx` — constants (`CATALOGUE_INDICATORS`, floors, `GUARDRAIL_FLOORS`),
  `optInt`, a `useSteerControls()` hook (state + `validate()` + `applyTo(params)`), and the
  presentational cards (`IndicatorSubsetCard`, `SearchBudgetCard`, `DeflationFeedbackCard`,
  `CompiledFloorsCard`). `NewTraining` is refactored to consume them (DOM/accessible-names
  preserved so its tests stay green); `NewFlow` reuses the identical controls — train and flow can
  never diverge on what is steerable.
- **Stepped `NewFlow`** (configure → review → launch): step 0 Configure (window + **required** seed
  + the steer cards + compiled-floor chips), step 1 Review (a read-only preview of the exact
  `FlowParams` body + the "one supervised train→backtest flow / server-frozen holdout" framing),
  then the **Launch flow** action → `createFlowRun` → `POST /api/runs {type:"flow"}`.
- **POST body shape** = `{ type:"flow", params: FlowParams }` where params carries `seed` (required)
  + `start`/`end`/`resolution` (required) + only the whitelisted steer fields that were set
  (`generations`/`population`/`holdout`/`embargo`/`windows`/`folds`; `indicator_subset` only when a
  strict subset). **No** blocklisted threshold and **no** backtest-window field is ever sent (no
  control exists for them). Client floors block a sub-floor submit before the POST.
- **`FlowMonitor`** — polls `GET /api/runs/:id` via `usePollingRun`. Renders the **per-phase
  progression** (Train phase → Backtest phase) derived from `meta.flow` (`train_run`/`vintage`/
  `backtest_run`) + `meta.status`, the sub-run ids, and the frozen-holdout window
  (`holdout_start→holdout_end`). On success (`status==succeeded` ∧ `flow.vintage`) it fetches the
  sealed vintage (`getVintage`) and renders the **holdout/regime chips** + the **"backtest-holdout
  only — not paper-confirmed"** label (both reused from `VintageInspector` via extracted
  `RegimeComposition` + `NotPaperConfirmedCallout`), plus an **"Open in Vintage Inspector"** button.
- **Inspector deep-link.** `StrategiesArea` gains an optional `initialVintage` (mirrors
  `BacktestsArea`); `App.tsx` gains `openInspectorForVintage` and threads `onInspectVintage` to the
  Training area so the flow-success button opens the sealed vintage in the Inspector.
- **New TS wire types** in `web/src/api/runs.ts`: `FlowParams`, `FlowProgress`, `FlowRunMeta`
  (extends the `RunMeta` union + `RunType`), `isFlowRun`, `createFlowRun` — kept hand-in-lockstep
  with the Rust DTOs.

## 3. Test plan (component tests)
- **`NewFlow.test.tsx`**: (a) stepped form advances Configure→Review and the Launch button issues a
  single `POST /api/runs`; (b) **assert the body matches `FlowParams`**: `type==="flow"`, `seed`
  present, `start`/`end`/`resolution` present, a strict `indicator_subset` when an indicator is
  deselected (omitted when full), whitelisted `windows`/`folds`, and **no** blocklisted key
  (`cost_stress_multiplier`/`dsr_cutoff`/`pbo_cutoff`/`evolved_pool`/`max_turnover_frac`) and **no**
  backtest-window field; (c) required-seed + sub-floor windows client validation blocks the POST;
  (d) compiled-floor guardrail chips render disabled with no enabled control that can set them; (e)
  a server `400` surfaces inline.
- **`FlowMonitor.test.tsx`**: (a) per-phase progression (train active → backtest active) from
  `meta.flow`; (b) on success it renders the regime chips + the "not paper-confirmed" label and the
  "Open in Vintage Inspector" button fires `onInspectVintage(vintage)`.
- **`NewTraining.test.tsx`** (existing) must stay green after the steer extraction (DOM/accessible
  names preserved) — the regression guard that reuse did not change train behaviour.

## 4. Risks
- **NewTraining regression** from the steer extraction — mitigated: its tests query by role/label/
  text (not CSS class), and the extracted cards preserve every accessible name (`Start`, `End`,
  `WFO windows`, `CV folds`, `Projected distinct-trial N`, the `compiled gate floors` /
  `evolved-pool formulas` groups) and the "recorded after the run" copy.
- **Contract drift** — the client `FlowParams`/`FlowProgress` are hand-mirrors of the Rust DTOs;
  drift is fail-closed (the server `validate_flow` rejects anything off-whitelist as a `400`).
- **No flow list** — a launched flow is reachable only via its live monitor this ticket; a flow
  browser is deliberately out of scope (design §14 Q6) until the flow list grows its own lifecycle.
</content>
</invoke>
