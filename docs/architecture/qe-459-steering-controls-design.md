# QE-459 — SPA steering controls on the New-training form (evidence note)

> **Status:** Implementation evidence note (written BEFORE coding, per the work-on-tickets skill).
> Ticket: [`docs/mds/tickets/QE-459.md`](../mds/tickets/QE-459.md). Design ref:
> [`qe-455-research-flow-design.md` §6](./qe-455-research-flow-design.md#6-the-steer-knob-whitelist--the-anti-overfitting-guardrail)
> (whitelist/blocklist), QE-450 §13.4 (guardrail chips — client ergonomics, server enforces).
> Depends on QE-458 (merged `cfbb8d9`), QE-261.

## 1. Goal

Surface QE-458's **whitelisted, gate-monotone steer knobs** on the New-training form (reused by the Flow
page QE-462): an **indicator picker**, **search-budget** (generations/population), **windows/folds** inputs;
render the **blocklisted** thresholds as **fixed disabled guardrail chips**; and show **deflation-scaling
feedback** (projected distinct-trial `N` + an honest archive-coverage note) so the operator learns that
steering more *raises* the deflation bar rather than buying a free pass.

## 2. Current state (real file paths)

- **Training form:** `web/src/app/training/NewTraining.tsx` (QE-261) — today exposes window/resolution/seed +
  optional generations/population/holdout/embargo, submits via `createTrainRun` → `POST /api/runs {type:"train"}`.
  Tests: `web/src/app/training/NewTraining.test.tsx`. Wired by `web/src/app/training/TrainingArea.tsx`.
- **NewCampaign guardrail chips (the mirror):** `web/src/app/evolve/NewCampaign.tsx` — disabled/fixed caps
  (`CAP_DEPTH=4`, `CAP_NODES=16`, `CAP_LOOKBACK=200`, `CAP_K=16`) as label hints + a **window-lattice
  toggle-chip group** (`.qe-nc__chips` / `.qe-nc__chip`, `aria-pressed`), full-lattice ⇒ omit ⇒ engine default.
  This is the pattern QE-459 mirrors (toggle chips for the indicator picker; disabled chips for the floors).
- **API client submit path:** `web/src/api/runs.ts` — `interface TrainParams` (source-of-truth mirror of the
  Rust wire DTO) + `createTrainRun(params) → postRun('train', params)`.
- **Design components:** `web/src/design` exports `Button, Callout, Card, Icon, Input, Select` (used verbatim).

### Whitelisted `TrainParams` fields the server actually accepts (verified in QE-458 source)

Confirmed by reading `crates/run-protocol/src/lib.rs` (`struct TrainParams`, lines 358–430),
`crates/server/src/runs/manager.rs` (`validate_train`, lines 421–468), and `crates/cli/src/jobs/train.rs`
(what `run_train_job` actually *applies* to the live search, lines 183–195 / 343–345):

| Field | Wire name | Applied live? | Client control |
|---|---|:---:|---|
| Window | `start`, `end`, `resolution` | yes | date/date/select (existing) |
| Seed | `seed` | yes | mono input (existing) |
| Search budget | `generations`, `population` | yes | mono inputs (existing) |
| Holdout / embargo | `holdout`, `embargo` | yes (floored ≥250 / ≥1) | mono inputs (existing) + floor validation |
| **Indicator subset** | `indicator_subset: string[]` | **yes** (`VariationDriver::with_allowed_features`) | **NEW** toggle-chip picker over the catalogue |
| **WFO windows** | `windows: usize` | **yes** (default 2; floor ≥4) | **NEW** mono input + floor validation |
| **CV folds** | `folds: usize` | **yes** (default `cv_folds`; floor ≥2) | **NEW** mono input + floor validation |

**Rejected / blocklisted fields the form MUST NOT submit** (each is a hard `400` in `validate_train`):
- `evolved_pool`, `evolved_formulas` — **`400` "not yet supported on the live train search"** (QE-402-safe
  feature-space extension is a follow-up). *No enabled control; a disabled affordance only.*
- `cost_stress_multiplier`, `max_turnover_frac`, `capacity_floor_usd`, `dsr_cutoff`, `pbo_cutoff`,
  `ic_fdr_threshold` — `reject_if_present` (`400` if so much as named). *No form control exists for these.*

Compiled floors (source of the chip magnitudes), from `crates/validation/src/steer.rs`:
`COST_STRESS_MULTIPLIER_FLOOR=1.0`, `MAX_TURNOVER_CAP_FLOOR=0.25`, `CAPACITY_FLOOR_USD=250_000`,
`DSR_CUTOFF_FLOOR=0.95`, `PBO_CUTOFF_FLOOR=0.5`, `IC_FDR_THRESHOLD_FLOOR=0.10`, `HOLDOUT_FLOOR=250`,
`EMBARGO_FLOOR=1`, `MIN_WFO_WINDOWS=4`, `MIN_WFO_FOLDS=2`, `DESCRIPTOR_SPACE_CELLS=45`.

### Catalogue indicator ids (the picker options)

There is **no `/api/indicators` endpoint** — the catalogue is compiled in `crates/signal/src/indicator/`
(`price.rs` + `flow.rs`, assembled by `catalogue()` in `mod.rs`). Mirroring NewCampaign's compiled
`WINDOW_LATTICE`/`CAP_*` mirrors, the picker hard-codes the catalogue id list as a **documented client mirror**
(22 ids: `return_1, sma_ratio_20, ema_ratio_20, roc_10, rsi_14, stoch_k_14, williams_r_14, cci_20, mfi_14,
cmf_20, aroon_osc_25, macd_hist_12_26_9, atr_pct_14, bb_percent_20, bb_bandwidth_20, std_returns_20,
volume_ratio_20, signed_volume_ratio_14, funding_avg_8, funding_state, oi_roc_10, premium_state`). Drift risk
is bounded: the server rejects any `indicator_subset` id not in the catalogue, so a stale mirror fails closed
(a stale add ⇒ server `400`, never a silent wrong search).

## 3. Implementation decisions

1. **Indicator picker** = toggle-chip group over the catalogue mirror (mirrors NewCampaign window lattice).
   Default: all selected (⇒ full catalogue). Submit `indicator_subset` **only when a strict subset** is
   selected (full set ⇒ omit ⇒ engine default full catalogue, exactly like the window-lattice logic). Client
   validation requires ≥1 indicator selected.
2. **Evolved-pool constraint (QE-458 integration #1).** The AC's "include/exclude already-sealed evolved-pool
   formulas" is reconciled with QE-458 rejecting `evolved_pool`/`evolved_formulas` server-side: rendered as a
   **disabled "not yet supported on the live train search" affordance** (a guardrail-style hint mirroring the
   disabled caps chips), **NOT** an enabled toggle. The form submits neither field — so it can never issue an
   always-`400` request. A component test asserts there is **no enabled control** that submits evolved-pool.
3. **Windows/folds** = mono inputs with client floor validation (`windows ≥ 4`, `folds ≥ 2`) mirroring
   `MIN_WFO_WINDOWS`/`MIN_WFO_FOLDS`; the server re-enforces (`400`). Holdout/embargo keep their existing
   inputs but gain floor validation (`≥ 250` / `≥ 1`) mirroring `HOLDOUT_FLOOR`/`EMBARGO_FLOOR`.
4. **Blocklisted guardrail chips** = a "Compiled floors (not steerable)" card of **disabled** chips for
   cost-stress `1×`, turnover cap `0.25`, capacity floor `$250k`, DSR `≥0.95`, PBO `≤0.5`, IC/FDR `≥0.10`,
   holdout floor `250 bars`, embargo floor `1 bar`. Non-interactive (`disabled`), with a hint that they are
   compiled floors the research path cannot relax. **No form control sets any of them.** A component test
   asserts no enabled control can set a blocklisted threshold.
5. **Deflation-scaling feedback (QE-458 integration #2).**
   - **Projected `N`** is a *pure client-side function* of subset cardinality + budget, mirroring QE-458's
     `effective_trials_with_features(cells, gens, windows, feature_space)`
     = `45 · generations · windows · max(1, subsetSize)` (`DESCRIPTOR_SPACE_CELLS=45`, `feature_space` =
     selected catalogue count, evolved always 0 since evolved-pool is disabled). Rendered as a **projected /
     indicative** number, explicitly labelled "the deflation bar rises with scope" — it grows monotonically as
     the operator widens the subset or raises generations/windows. Uses indicative defaults (gens=40,
     windows=4) when the budget fields are blank, clearly noted as indicative.
   - **Archive coverage pre/post is a runtime search OUTPUT** only known after a run — it **cannot** be
     previewed pre-submit. Handled honestly: the panel states coverage is **recorded after the run** (surfaced
     in the Vintage Inspector, QE-457) and shows **no fabricated pre-run number**. See §5 flagged decision.
6. **No server change.** Client-side hints only *remove* affordances; `validate_train` stays the single
   enforcement point. The only `runs.ts` change is extending the `TrainParams` mirror interface with
   `indicator_subset?`, `windows?`, `folds?` (already-present `generations/population/holdout/embargo` reused).

## 4. Test plan (component tests, vitest + testing-library)

Extend `web/src/app/training/NewTraining.test.tsx`:
- **Indicator picker** — deselect one catalogue chip, submit, assert `params.indicator_subset` is the strict
  subset (the deselected id absent); with all selected, assert `indicator_subset` is **omitted**.
- **Windows/folds** — enter valid `windows=6, folds=4`, assert they are POSTed; enter `windows=2` (below the
  `4` floor), assert submit is **blocked** with a client message and **no POST** fires.
- **Coverage / `N` feedback** — assert the projected-`N` figure is rendered and **increases** when generations
  is raised (and when a wider subset is selected); assert the honest "recorded after the run" coverage note is
  present and **no** fabricated pre-run coverage percentage is shown.
- **Disabled guardrail chips** — assert the compiled-floor chips render **disabled**, and assert there is **no
  enabled control** (input/checkbox/button) that can set a blocklisted threshold (cost-stress/turnover/
  capacity/DSR/PBO) and **no enabled control** that submits evolved-pool.
- Keep the existing three tests green (window/budget POST, missing-window block, server-`400` inline).

## 5. Flagged design decision (return to reviewer/orchestrator)

**Archive-coverage pre/post cannot be shown truthfully pre-submit.** Per QE-458 integration constraint #2,
archive coverage (occupied niches / descriptor space) is a *runtime search output*, only known after a run;
`available_feature_space`/`effective_trials_with_features` give a projectable `N`, but there is **no
server-provided pre-run coverage value** and no `GET` that would supply one for an unstarted run. This
implementation therefore: (a) shows a **projected/indicative `N`** that grows with subset+budget (labelled
projected), and (b) frames **archive coverage as recorded-after-run** (surfaced by the Vintage Inspector,
QE-457) with **no fabricated pre-run number**. This is the honest reading the ticket's constraint #2 asks for;
flagging it explicitly in case product wants a different treatment (e.g. seeding pre-run coverage from a prior
run of the same subset once QE-462's Flow page has a run history to read).

## 6. Risks

- **Catalogue-mirror drift** — the hard-coded id list can lag the compiled catalogue. Mitigated by fail-closed
  server validation (unknown id ⇒ `400`) and a code comment pinning the mirror to
  `crates/signal/src/indicator/`. A future `/api/indicators` endpoint (out of scope here) would remove it.
- **Projected-`N` is indicative, not the sealed basis** — the real `N` also folds in QE-439 distinct-canonical
  evaluations; the client figure is deliberately labelled *projected* so it teaches direction/scale, not an
  exact promise. The Vintage Inspector shows the sealed basis.
- **Scope creep** — no Flow-page (QE-462) layout, no server/validation change; ticket-only files.
