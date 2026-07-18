# QE-453 — Admin SPA `evolve/` area (design + evidence note)

*The five-screen `web/src/app/evolve/` SPA area that consumes the MERGED QE-452 endpoints. Mirrors
`web/src/app/training/` verbatim. Frontend-only — no Rust/backend/golden change.*

- **Spec of record:** `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.4 (screens), §13.5
  (review-gate stat set), §13.3 (run-vs-pool lifecycles), §13.6 (sandbox↔production barriers).
- **Consumed server surface (merged):** QE-452 Phase A (evolve run-create + `validate_evolve`) + Phase B
  (`docs/mds/reviewed/qe-452-phaseB.md`): the pool read/governance routes + `/runs/{id}/archive` + `/halt`.
- **Branch:** `qe-453/spa-evolve-area`.

## 1. Current-state evidence (what I mirror + what I consume)

### Training area (the sibling I mirror verbatim)
- `web/src/app/training/TrainingArea.tsx` — a **router-less `useState<View>` machine** (`list|new|monitor`).
- `NewTraining.tsx` — a create form with **client-side `validate()`** → `createTrainRun` → `POST /api/runs`.
  Field/`serverError` state, `Callout` for both; `Card`/`Input`/`Select`/`Button` design primitives.
- `TrainingMonitor.tsx` — `usePollingRun(runId)` (bounded-retry, terminal-stop) → renders live progress
  (`.qe-cov__grid` archive-coverage cells, stat tiles, gate card). Narrows on `meta.type === 'train'`.
- `TrainingList.tsx` — `useRunListPolling({type})` → `DataTable` with `StatusBadge`, `.filter(r => r.type…)`.
- Tests: `NewTraining.test.tsx` (stubs `fetch`, asserts POST body + blocks-submit-when-invalid + surfaces
  server 400), `TrainingMonitor.test.tsx` (deterministic running→terminal flip).
- Shared plumbing: `web/src/api/runs.ts` (typed client + discriminated `RunMeta` union), `usePollingRun.ts`,
  `useRunListPolling.ts`, `web/src/design/*` (primitives), `web/src/design/injectCss.ts` (CSP-safe CSS),
  `web/src/app/App.tsx` (nav → area), `web/src/design/AppShell.tsx` `NAV` list.

### Endpoints consumed (exact wire shapes traced to Rust)
| Endpoint | Source | Shape |
|---|---|---|
| `POST /api/runs {type:"evolve",params}` | `run-protocol` `EvolveParams`; `manager::validate_evolve` | `params`: `seed`(REQUIRED u64), `mode`∈`{sandbox,production}`, `start`,`end`,`resolution`, optional `generations/offspring/states/depth/nodes/lookback/windows/k/config/profile`. → `201 {id}` / `400 {error}`. |
| `GET /api/runs/{id}` | `RunMeta` union | evolve variant: `{type:"evolve", params:EvolveParams, …RunMetaBase}`. |
| `GET /api/runs/{id}/archive` | `pools.rs::get_archive` → `EvolveArchive` | `{pool_id,mode,generations,offspring, cells:ArchiveCell[], trial_basis:ArchiveTrialBasis}`; `404` when absent. |
| `POST /api/runs/{id}/halt` | `pools.rs::halt` | `200 {id,status,halted}` / `404` / `409 {error,status}` already-terminal. |
| `GET /api/formula-pools` | `pools.rs::list_pools` → `PoolSummary[]` | `{id,mode,content_hash,pool_hash,formula_count,gp_aware,distinct_evaluations,lifecycle}`. |
| `GET /api/formula-pools/{id}` | `pools.rs::get_pool` → `PoolDetail` | `{content:FormulaPoolContent, content_hash, lifecycle, history:TransitionRecord[]}`; `404`. |
| `POST /api/formula-pools/{id}/{approve,reject,revoke,seal}` | `pools.rs::transition` | `200 {pool_id,lifecycle}`; **`409 {error,pool_id,mode:"production"}`** on a production Seal (gated on QE-454); `409 {error}` on an illegal edge; `404`; `403` role-less. |

Lifecycle wire tokens (snake_case): `draft`→`approved`→`sealed`, plus `rejected`/`revoked`. Legal edges
(the server state machine I mirror in the UI): `draft→approve`, `draft→reject`, `approved→seal`,
`approved→revoke`, `sealed→revoke`. `Decimal` bars in `DeflationSummary` arrive as **strings**
(`trial_variance`,`expected_max_sharpe`,`champion_dsr`,`uncensored_pbo|null`).

## 2. Screens (each: endpoint consumed + how it mirrors Training)

1. **NewCampaign** (`new`) — mirrors `NewTraining`. `createEvolveRun` → `POST /api/runs {type:"evolve"}`.
   Client-side `validate()` matching `validate_evolve`: **seed REQUIRED** (blocks submit without it),
   caps `depth≤4 / nodes≤16 / lookback≤200 / k≤16`, `windows ⊆ {5,10,20,50,100}`, `start<end`, `mode ∈
   {sandbox,production}`. Windows shown as fixed guardrail toggle-chips on the lattice. Production is
   selectable but carries an honest callout that a production launch may be **refused server-side** (the
   compiled prereq const, QE-454) — a `400` is surfaced inline, never hidden.
2. **CampaignMonitor** (`monitor`) — mirrors `TrainingMonitor`. `usePollingRun(runId)` for run status +
   `RunProgress`; `getRunArchive(runId)` (fetched on mount + while running) → **ArchiveHeatmap** (family ×
   timescale×complexity cells reusing `.qe-cov__grid`) + **TrialCountBar** (distinct-canonical `N` vs the
   analytic floor vs the `E[maxSharpe]` bar; amber when `N === floor` — the QE-439 "blind floor" tell). A
   persistent **mode banner** (sandbox = RESEARCH). A **Halt** control → `haltRun` (`POST /halt`).
3. **FormulaSexpr** (component, used by PoolReview) — renders one formula's canonical S-expression readably
   in an `overflow-x:auto` mono container + its `formula_hash`. The human-inspectable form the reviewer reads.
4. **PoolReview — THE GATE** (`review`) — `getFormulaPool(id)` → K formulas (via FormulaSexpr) + the
   **Deflation-basis card** (the four numbers together, never a lone green tile) + lineage + lifecycle +
   transition history. Approve/Reject/Revoke/**Seal** wired to the governance endpoints, each **disabled
   when illegal from the current state** (mirrors the server machine: Seal only from `approved`; terminal
   states offer nothing). **Fail-closed honesty:** a production Seal returns `409 "gated on QE-454"`; the UI
   surfaces the exact server message in a danger `Callout`, then **re-fetches the pool and reflects the
   unchanged lifecycle** (stays `approved`) — it never fakes success. Sandbox seal proceeds and the returned
   `sealed` state is reflected. A visible mode banner separates sandbox from production.
5. **PoolBrowser** (`pool`) — `listFormulaPools()` → `DataTable` of pools with mode + lifecycle badge +
   formula count + `gp_aware`; row-click opens PoolReview. Mirrors `TrainingList`.

`EvolveArea.tsx` is the router-less `useState<View>` machine `{list|new|monitor|pool|review}` (`list` =
CampaignList of evolve runs mirroring `TrainingList`; `pool` = PoolBrowser). Nav id `evolve` ("Indicator
evolution") is wired into `AppShell` `NAV` + `App.tsx`.

## 3. How PoolReview honours the fail-closed gate (safety-critical)
- The client **never** pre-empts the server verdict: Seal is *offered* whenever the state machine allows it
  (`approved`), but the server is the authority. On a production pool the click yields `409`; the screen
  shows the server's named-blocker message and **re-reads** `GET /api/formula-pools/{id}` so the rendered
  lifecycle equals the server's (still `approved`). No optimistic state flip.
- Actions illegal from the current state are **disabled** (mirrors §13.3), so the UI cannot even attempt a
  seal-before-approve or an approve-after-revoke.
- The Deflation-basis card renders `distinct_evaluations` vs `analytic_floor` (with an `N == floor`
  "blind-floor" warning), `gp_aware`, the finite `E[maxSharpe]`, uncensored PBO (+ `variance_trials`
  population), and DSR labelled "necessary — not sufficient". Displayed = what the QE-454 server seal will
  enforce; the SPA only makes the safe path legible.

## 4. Test plan (Vitest + Testing Library; mirrors the Training tests)
- `NewCampaign.test.tsx`: (a) **blocks submit without a seed** (no POST, inline message); (b) POSTs a
  `type:"evolve"` body with seed+caps+windows on a valid submit; (c) client-side cap violation blocks submit;
  (d) surfaces a server `400` inline.
- `CampaignMonitor.test.tsx`: renders live archive heatmap cells + trial-count bar from a stubbed
  `/archive`, and the Halt control POSTs `/halt`.
- `PoolReview.test.tsx`: **(the load-bearing one)** a production pool's Seal surfaces the `409` "gated on
  QE-454" message and the lifecycle stays `approved` (re-fetch reflects server truth); a sandbox pool's Seal
  transitions to `sealed`; Seal is disabled from a non-`approved` state.
- `PoolBrowser.test.tsx`: lists pools with lifecycle badges and opens PoolReview on row-click.

## 5. Web green-gate commands (the ONLY gate — CI disabled)
Run from `web/`, all must pass on the exact commit:
- **typecheck:** `npx tsc -b` (also part of `npm run build`)
- **lint:** `npm run lint` (ESLint 9 flat config)
- **test:** `npm run test` (`vitest run`)
- **build:** `npm run build` (`tsc -b && vite build`)

Plus a Rust-untouched sanity: `git status` shows only `web/` (+ `docs/`) changes — no `Cargo.*`, no
fixture/golden churn (this ticket adds **zero** Rust).

## 6. Risks
- **Faking the fail-closed gate** — the cardinal risk. Mitigated by re-fetching after every governance
  action and never optimistically flipping lifecycle; a dedicated test pins the 409 + `approved` behaviour.
- **Drift from the Rust wire contract** — the `runs.ts` DTOs are kept hand-in-lockstep with
  `run-protocol`/`pools.rs` (documented at the type), exactly as the existing `RunMeta` union is.
- **No prereq-ready signal exists yet** (that is `/api/me` capabilities, QE-454) — so production mode is
  selectable but honestly flagged as server-refusable, rather than hard-disabled on a signal that doesn't
  exist. Documented as a QE-454 carry-forward.
- **No new endpoint required** — every screen maps to an existing merged route; nothing is missing.
</content>
</invoke>
