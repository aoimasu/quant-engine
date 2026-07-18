# QE-457 — SPA Vintage Inspector screen (evidence + design)

> Ticket: `docs/mds/tickets/QE-457.md` · Design ref: `docs/architecture/qe-455-research-flow-design.md` §7.2
> Backend consumed: `GET /api/vintages/{id}` (QE-456, merged `20d972b`), evidence persisted by QE-467 (`ce39bc8`).

## 1. Current-state evidence (real paths)

- **The placeholder to replace** — `web/src/app/App.tsx:121`: the `active === 'strategies'` branch renders
  `<Placeholder … "The strategies browser is on the way." />`. The `strategies` nav item already exists
  (`web/src/design/AppShell.tsx:66`, `{ id: 'strategies', label: 'Strategies', icon: 'git-branch' }`) and
  `SCREEN_TITLES.strategies = 'Strategies'` (`App.tsx:17`).
- **The screen to mirror — evolve PoolReview**:
  - `web/src/app/evolve/PoolReview.tsx` — the governance/inspection screen: injected CSS via
    `injectCss('qe-pr-css', CSS)`, `useState`/`useCallback`/`useEffect` load, `getFormulaPool(poolId)` fetch,
    `ApiError` handling, a back button, a header with badges, the non-collapsible **Deflation-basis card**
    (the honest number grid `.qe-pr__defl`), a lineage grid, and a governance-actions card.
  - `web/src/app/evolve/PoolReview.test.tsx` — the test harness: `vitest` + `@testing-library/react` +
    `userEvent`; a `json()` helper, a `mockApi()` that routes GET/POST by URL regex, `vi.stubGlobal('fetch', …)`.
  - `web/src/app/evolve/PoolBrowser.tsx` — the read-only list table (`DataTable`, `listFormulaPools()`,
    row-click → open) and the exported `LifecycleBadge`. This is the "list → open detail" pattern.
  - `web/src/app/evolve/EvolveArea.tsx` — the **router-less view-state machine** (`list | new | monitor | pool
    | review`) an area uses to move between its list and its detail screen. I mirror this exactly.
- **The API client layer** — `web/src/api/runs.ts` ("Runs / vintages / market-data API client"): `getJson<T>()`
  (`credentials: 'same-origin'`, `throwForResponse` → `ApiError`/`UnauthorizedError`), the existing
  `listVintages()` (`GET /api/vintages` → `VintageListItem[]`) and `VintageListItem`/`VintageSummary` types,
  and `getFormulaPool()`/`PoolDetail` (the detail-fetch pattern I copy for `getVintage()`).
- **Design components** — `web/src/design/index.ts`: `Badge`, `Callout`, `Card`, `DataTable`, `Button`, `Icon`,
  `Column`, `BadgeVariant`. `injectCss` at `web/src/design/injectCss.ts`. Icons registered in
  `web/src/design/Icon.tsx` (I reuse `shield`, `flask-conical`, `layers`, `arrow-left`, `git-branch`, `hash`,
  `activity` — all already in `REGISTRY`; no new glyph needed).
- **Backend response shape** — `crates/server/src/read.rs` `VintageDetail` (lines ~96-135) + the vintage types
  in `crates/vintage/src/lib.rs`: `SealEvidence` (l.62, `dsr/pbo/spa_pvalue/n_trials/realised_turnover/
  capacity_usd` + optional `cost_stress_net_min/uncensored_pbo/ic/fdr`), `DataProvenance`
  (`#[serde(rename_all="lowercase")]` → `real|synthetic|mixed`, l.124), `HoldoutSplit`
  (`holdout_range?/train_range?/embargo_bars`, l.147), `RegimeShare` (`regime/bars`, l.161), `SteerDelta`
  (l.171), `TimeRange` (`start/end`, l.137). `composition[]` = `{index, weight, indicators[{feature, id?,
  source}]}`, `source ∈ {"catalogue","evolved"}`.

## 2. Implementation decisions

- **New area `web/src/app/strategies/`** mirroring `evolve/`:
  - `StrategiesArea.tsx` — a view-state machine (`{ view:'list' } | { view:'inspect', vintageId }`), mirroring
    `EvolveArea.tsx`. Wired into `App.tsx` in place of the `strategies` placeholder.
  - `VintageBrowser.tsx` — the read-only list over `listVintages()` (`DataTable`, row-click → inspect),
    mirroring `PoolBrowser.tsx`. Shows id / chromosomes / worst-case loss / format version (the list summary
    has no `data_provenance`; provenance is a detail-only field, surfaced in the inspector).
  - `VintageInspector.tsx` — the inspector screen mirroring `PoolReview.tsx`. Exports `ProvenanceBanner` (like
    `PoolBrowser` exports `LifecycleBadge`).
- **API**: add `getVintage(id)` + the `VintageDetail` DTO family to `web/src/api/runs.ts` (same file as
  `listVintages`/`getFormulaPool`), typed one-to-one with the Rust `VintageDetail`.
- **Provenance banner (first-class)**: a full-width banner keyed off `data_provenance`. `synthetic` and `mixed`
  are loud (warning styling, explicit "NOT REAL DATA" / "PARTIALLY SYNTHETIC" wording); `real` is a calm
  neutral note. `mixed` is never rendered as `real`. Implemented as `ProvenanceBanner` with a `role` so tests
  can assert the exact copy per variant.
- **Gate-evidence card leads net-of-cost / tradability**: the first grid block is the tradability set —
  cost-stress `min{1×,2×}` net (`cost_stress_net_min`), realised turnover, `capacity_usd`. The deflation basis
  (DSR "necessary — not sufficient", uncensored PBO + its trial population, distinct-trial `N` vs `E[maxSharpe]`
  proxy, IC/FDR, SPA p) follows, demoted. **No standalone green health tile.** All numbers rendered verbatim
  from the payload — no client recomputation (optional fields show `—` when absent).
- **Frozen-holdout panel**: the `{train_range, embargo_bars, holdout_range}` split plus the
  `regime_composition[]` table (regime label → bars → share%), so the operator sees the verdict rode diverse
  regimes. Share% is a pure display ratio of the returned bar counts, not a gate recomputation.
- **"Not paper-confirmed" label**: a standing `Callout`/note stating the verdict is a backtest-holdout
  evaluation that still owes the G2/G3 live/shadow gates before promotion.
- **Inspection only**: the screen renders **no** Approve/Seal/Promote/Select/Revoke control. Enforced by a test
  that queries for every such control name and asserts absence.
- **Numbers**: a small `fmt` helper formats floats for display (fixed dp) and `capacity_usd` as USD; it never
  alters or derives gate values — it only formats the server value.

## 3. Test plan (`VintageInspector.test.tsx`, mirroring `PoolReview.test.tsx`)

- `vi.stubGlobal('fetch', …)` routing `GET /api/vintages/{id}` to a fixture builder `detail(provenance)`.
- Provenance banner: three cases — `real` (calm, not flagged), `synthetic` (unmistakable "synthetic" copy),
  `mixed` (called out distinctly, **asserts the banner text does not read as `real`**).
- Composition table: renders chromosome index → indicator refs (catalogue vs evolved labelled) → weight.
- Gate card: leads with the net-of-cost/tradability numbers (cost-stress, turnover, capacity), shows the
  deflation basis honestly, and carries the "not paper-confirmed" label; asserts no lone health badge.
- Inspection-only: asserts **no** button/control matching `/approve|seal|promote|select|revoke|reject/i`.
- Load-error path: a `500` surfaces the error `Callout`.
- `ProvenanceBanner` unit assertions folded into the same file (exported component).

## 4. Risks

- **List summary lacks `data_provenance`** — the browser row can't show provenance; acceptable, the inspector
  is the first-class provenance surface (ticket scope). Noted so a future ticket can enrich the list.
- **Optional gate fields** (`cost_stress_net_min`, `uncensored_pbo`, `ic`, `fdr`) are absent on the normal train
  path — the card renders `—` rather than hiding the row, so the honest-basis hierarchy is always visible.
- **`holdout_split`/`regime_composition` empty until QE-460 populates them** — the panel renders an explicit
  "not yet recorded" empty state rather than a blank, so an operator isn't misled into thinking coverage is
  known.
- **No Rust change** — the Rust gate is run as insurance only; a red Rust gate means an unrelated breakage.
