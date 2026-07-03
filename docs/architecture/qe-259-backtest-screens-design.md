# QE-259 — Backtest screens wired to the API — design / evidence note

- **Ticket:** QE-259 (`Phase: PreP3` · `Area: frontend` · `Depends on: QE-255, QE-257, QE-258`)
- **Spec refs:** admin-ui design spec §7.2, §7.3, §8.1 (result contract); decisions D1 (genome params
  read-only), D4b/c (run store + subprocess), D4d (auth).
- **Branch:** `qe-259/backtest-screens-wired` off `main`.

## 1. Goal

The core user journey: from the SPA, trigger a backtest of a sealed vintage over a window, watch its
progress polled to completion, and review the full result contract (metrics strip, equity/drawdown,
monthly heatmap, trades table). Plus the read-only Market-data coverage screen. Genome params are
**read-only** (D1). Carried QE-258 follow-up: make the allowlist-rejection redirect actually fire.

## 2. Current-state evidence

### 2.1 The SPA surface (QE-258, `web/`)
- Vite + React + TS. Design tokens (`src/styles/tokens/*.css`), primitives ported as `.tsx` with
  co-located CSS injected via `injectCss(id, css)` (idempotent, jsdom-safe): `Button`, `Badge`,
  `Callout`, `Card`, `Icon`, `AppShell` (in `src/design/`, re-exported from `src/design/index.ts`).
- `src/app/App.tsx` — session gate: `fetchMe()` → Login (unauth) / AppShell (auth). Nav state
  `active` drives which Research screen renders; today all render `Placeholder` "Ships in QE-259".
- `src/api/session.ts` — `fetchMe`, `startLogin`, `logout`, `detectRejection(search)` reading
  `?error=forbidden|rejected|403|not_allowed|unauthorized` from the URL.
- Tests: Vitest + RTL, `fetch` stubbed via `vi.stubGlobal`. `css: false` in vitest config, so
  class-name assertions (not computed styles) are the idiom.

### 2.2 The API contracts (all session-gated, same-origin)
- `GET /api/runs` → `RunMeta[]` newest-first. `RunMeta = { id, type, status, params, progress{pct,
  stage,msg}, created_ms, started_ms, finished_ms, exit, error, artifacts }`. `status ∈ queued|
  running|succeeded|failed` (QE-255 `crates/server/src/runs/model.rs`).
- `POST /api/runs` body `{ type:"backtest", params: BacktestParams }` → `201 { id }` or `400 { error }`.
  `BacktestParams = { vintage, strategy?, start, end, resolution, universe[], taker_fee_bps,
  slippage_model }`. Lenient serde; required-ness enforced server-side as a uniform 400.
- `GET /api/runs/:id` → one `RunMeta` (status + progress) | 404.
- `GET /api/runs/:id/result` → the §8.1 `result.json` once `succeeded`; 409 while not-yet / 404 unknown.
- `GET /api/vintages` → `{ id, label, summary{chromosomes, content_hash, worst_case_loss,
  format_version} }[]` (QE-257 `crates/server/src/read.rs`). **No per-vintage symbol roster is
  exposed.**
- `GET /api/market-data/coverage` → `CoverageRow[] = { symbol, resolution, from(ms), to(ms), bars }`
  (QE-257 / `crates/storage/src/coverage.rs`).

### 2.3 The §8.1 result contract (drives the result screen)
`{ strategy{name,status,tags[],params{}}, window{start,end,resolution}, universe{symbols[],count},
costs{taker_fee_bps,slippage_model}, metrics{cagr,sharpe,sortino,max_dd,win_rate,profit_factor},
equity_curve[], drawdown[], monthly_returns[{year,months[12]}], trades[{id,symbol,side,entry,exit,
hold,return_pct,result}] }`.

### 2.4 DesignSync BacktestResearch kit
Pulled `ui_kits/strategy-research/BacktestResearch.jsx` (+ the primitives it uses) from the real
Claude Design project `2b443e2a-…` (the spec's id). The kit renders: a header (name + status badges +
mono param `Tag`s), a running progress `Card` (bar + `pct` + msg), a 6-cell metrics strip, `Tabs`
(Overview/Trades/Config), inline-SVG equity + drawdown area charts, a CSS-grid monthly-returns
heatmap, a `DataTable` trades table, and a right rail (Run config / Costs). **Charts are hand-rolled
inline SVG + CSS — no charting library, no runtime CDN** → CSP-safe by construction. The kit's data
is synthetic demo filler; we make it data-driven from the §8.1 contract and turn the editable config
inputs read-only (D1).

Primitives the kit needs that QE-258 had not yet ported: `Input`, `Select`, `DataTable`, `Tag`,
`Pnl`, `Tabs`. Ported faithfully as `.tsx` (same class names / CSS, typed props).

### 2.5 The QE-256 callback (carried change)
`crates/server/src/auth/mod.rs` `callback()` returns `reject(FORBIDDEN, …)` (JSON 403) on a
non-allowlisted login. The SPA's `detectRejection` reads `?error=forbidden` from the URL, so today the
styled Callout never fires. Test `crates/server/tests/auth.rs::valid_login_not_on_allowlist_is_403`
asserts the 403 + no cookie.

## 3. Decisions

- **D-routing.** No router library (keeps the CSP-safe, minimal-dep posture; avoids a new dep + its
  license review). Screen selection stays state-based in `App` via the existing `active` nav id. The
  Backtests area owns a small internal view state `{ view: 'list'|'new'|'result', runId? }`. This is
  sufficient for the four v1 screens and matches the existing App pattern.
- **D-polling.** The result screen polls `GET /api/runs/:id` on a `setInterval` (2 s) while
  `status ∈ queued|running`; on `succeeded` it fetches `GET /api/runs/:id/result` once and renders the
  full contract; on `failed` it shows the error. Interval cleared on unmount / terminal. (SSE
  `/stream` is optional per spec; polling is the specified default and is trivially testable.)
- **D-charts.** Reuse the kit's inline-SVG area charts + CSS-grid heatmap verbatim (data-driven). No
  charting lib, no CDN → CSP-safe, no new dependency, no license review.
- **D-universe source.** The vintages API exposes **no** per-vintage symbol roster, so the New-backtest
  form sources the universe options from `GET /api/market-data/coverage` (the symbols actually present
  in the store — which also bound the valid window). All coverage symbols are pre-selected by default;
  the user can deselect. Client-side validation requires ≥1 symbol (mirrors the server's non-empty
  universe rule). Documented deviation from "the vintage's roster" — the roster is not available at the
  API and coverage is the authoritative set of backtestable symbols. Follow-up: expose a roster on the
  vintage summary if per-vintage universes are needed.
- **D-genome-read-only (D1).** `strategy.params` from the result render as read-only mono `Tag`s in
  the header; the Config tab shows eval params (window/universe/costs) read-only. No editable genome
  inputs anywhere.
- **D-re-run.** "Re-run" POSTs `{ type:'backtest', params }` using the run's **meta.params** (the exact
  create params), then navigates to the new run id. (Meta.params is the faithful clone source; the
  result contract's derived shapes are not the create body.)

## 4. Implementation plan

New files under `web/src/`:
- `design/Input.tsx`, `design/Select.tsx`, `design/DataTable.tsx`, `design/Tag.tsx`, `design/Pnl.tsx`,
  `design/Tabs.tsx` (+ exports in `design/index.ts`).
- `api/runs.ts` — typed `RunMeta`, `BacktestParams`, `BacktestResult`, `VintageListItem`,
  `CoverageRow`, and `fetch` helpers (`listRuns`, `createRun`, `getRun`, `getRunResult`,
  `listVintages`, `getCoverage`).
- `app/backtest/BacktestsArea.tsx` — the Research "Backtests" area (list / new / result view state).
- `app/backtest/BacktestsList.tsx` — table from `GET /api/runs`; row click → result; "New backtest".
- `app/backtest/NewBacktest.tsx` — trigger form → `POST /api/runs`; client validation + server-400.
- `app/backtest/BacktestResult.tsx` — ported `BacktestResearch`, data-driven + polling + Re-run.
- `app/MarketData.tsx` — read-only coverage table from `GET /api/market-data/coverage`.
- Wire the three into `App.tsx` (`backtest` → BacktestsArea, `data` → MarketData; `strategies` stays a
  placeholder — not in QE-259 scope).

Server change (carried): in `crates/server/src/auth/mod.rs`, replace the allowlist-reject JSON 403
with `302 → /?error=forbidden` (a `redirect(&str)` helper); keep 401 (bad token / no session) and 400
(CSRF/missing code) as-is. Update `auth.rs::valid_login_not_on_allowlist_is_403` to assert the 302 +
`Location: /?error=forbidden` + no session cookie; rename to reflect the redirect.

## 5. Test plan

**Frontend (Vitest + RTL, `fetch` mocked):**
- New-backtest form POSTs to `/api/runs` with the entered params; a server 400 surfaces inline.
- Backtests list renders rows from a mocked `GET /api/runs` (id, vintage, window, status, metrics).
- Result screen renders the full §8.1 contract from a mocked `GET /api/runs/:id/result`: 6-metric
  strip, equity + drawdown charts (SVG present), monthly heatmap cells, trades table rows.
- Progress card polls `GET /api/runs/:id` while `running` and swaps to the result on `succeeded`
  (fake timers).
- Genome params render read-only (tags, no inputs); Config tab inputs are `readOnly`/`disabled`.
- "Re-run" issues a new `POST /api/runs` with the run's params.
- Market-data coverage renders symbol × range rows from a mocked `GET /api/market-data/coverage`.
- Ported primitives keep their design class names (render checks).

**Rust (server change):**
- `auth.rs` allowlist-reject now asserts 302 → `/?error=forbidden`, no session cookie. All other auth
  tests (401 no-session/bad-token, 200 allowlisted, 400 CSRF/missing-code) stay green unchanged.

**Gates:** full Rust green gate (`fmt`/`clippy -D`/`test --workspace`/firewall/`deny`) +
`cd web && npm ci && npm run build && npm run lint && npm test`.

## 6. Risks

- **Universe roster deviation** (D-universe) — sourced from coverage, not the vintage; documented.
- **No sealed vintage / empty store** in a fresh env — the list/coverage screens must render empty
  states gracefully (they do; `[]` handled).
- **Polling in tests** — use Vitest fake timers to avoid real 2 s waits; clear interval on unmount to
  avoid act() leaks.
- **CSP** — charts are inline SVG/CSS, no new runtime dep; the CSP-safe posture from QE-258 is
  preserved (verified: `npm run build` produces no external-origin refs).
