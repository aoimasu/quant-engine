# QE-408 — Backtests list must filter to backtest runs (client filter + backend `?type=`)

`Phase: PreP3` · `Area: frontend / data-fetching + backend-contract` · `Effort: S`

Spec of record: `### QE-408` in `docs/reviews/2026-07-15-team-improvement-review.md` (lines 265–280).

## Problem / current-state evidence

`GET /api/runs` returns **all** runs — both `type:"backtest"` and `type:"train"` — newest-first
(`crates/server/src/runs/api.rs:62` `list_runs` → `list_runs_blocking`). There is no server-side
type filter.

On the frontend:

- `web/src/app/training/TrainingList.tsx:56` correctly narrows the shared payload with the
  `isTrainRun` type-predicate: `setRuns(r.filter(isTrainRun))`.
- `web/src/app/backtest/BacktestsList.tsx:52-54` renders `listRuns()` **unfiltered**:
  `.then((r) => setRuns(r))`. Training rows therefore leak into the Backtests table. The columns
  paper over the mismatch — the `Vintage` column already guards with
  `row.type === 'backtest' ? row.params.vintage || '—' : '—'` (a train run has no vintage) and the
  `Res` column falls back to `|| '—'`. A leaked train row is clickable: `onRowClick` calls
  `onOpen(row.id)`, which routes to `BacktestResult` → `getRunResult(id)`. For a train run that has
  no backtest `result.json` shape (or is not `succeeded`), that request 409s/404s → a permanently
  erroring detail screen.

Discriminator confirmed against QE-406:
- Frontend: `web/src/api/runs.ts` — `RunMeta = BacktestRunMeta | TrainRunMeta`, discriminated on
  `type`; `BacktestRunMeta.type === 'backtest'`, `TrainRunMeta.type === 'train'`. Only `isTrainRun`
  exists today; no `isBacktestRun`.
- Backend: `crates/server/src/runs/model.rs` — `RunMeta.run_type` (`#[serde(rename = "type")]`),
  values `"backtest"` / `"train"` (`RunSpec::run_type`). `IndexEntry.run_type` carries the same
  value, written in lockstep with the meta.

**Confirmed backtest discriminator value: `'backtest'`** (the complement of `TrainingList`'s
`type === 'train'`).

## Decisions

1. **Frontend — add `isBacktestRun` + client-side filter (mirror `TrainingList`).**
   Add an `isBacktestRun` type-predicate to `runs.ts` (complement of `isTrainRun`). In
   `BacktestsList`, narrow the state to `BacktestRunMeta[]` and set
   `setRuns(r.filter(isBacktestRun))`. Because the array now narrows to `BacktestRunMeta[]`, the
   `Vintage` column simplifies from the `type`-ternary to `row.params.vintage || '—'` (the leak-masking
   branch is deleted — train rows can no longer reach the table). This is the exact model
   `TrainingList` already uses, so the two lists stay symmetric.

2. **Frontend — pass `?type=backtest` to stop over-fetching, keep the client filter as
   defense-in-depth.**
   Give the shared `listRuns` an optional `type` argument that appends `?type=<t>` when present;
   `BacktestsList` calls `listRuns('backtest')`. The client-side `isBacktestRun` filter is retained
   (idempotent when the server honours the query — no double-filtering bug, just a narrowing no-op),
   so the table is correct even against an older/unfiltered server. `TrainingList` is **left
   unchanged** (out of QE-408 scope): it keeps `listRuns()` + `isTrainRun`. The `listRuns` signature
   change is backward-compatible (arg optional; no-arg preserves the bare `/api/runs`).

3. **Backend — add a `?type=` query filter to `list_runs`.**
   Parse an optional `type` via `axum::extract::Query`. When present, filter at the **index** level
   inside the existing single `spawn_blocking` closure (skip `IndexEntry`s whose `run_type` differs
   *before* reading their `meta.json`) — this also stops the server over-reading meta files and keeps
   the QE-411 batched-blocking invariant (one closure for the whole list, no per-run `spawn_blocking`).
   When absent, behaviour is **byte-identical** to today (iterate all, newest-first, skip-on-missing-
   meta). An unknown/empty `type` value yields an empty list (not an error), which is the correct,
   simplest contract. Coordinates with QE-410 (which will consume the same `?type=`).

## Test plan

- **Frontend regression (Vitest), `BacktestsList.test.tsx`:** stub `fetch` to return a **mixed**
  payload (a backtest run + a train run) for `/api/runs` (query-agnostic path match). Assert:
  - only the backtest row renders (the backtest vintage present; the train run's distinctive
    window/id absent);
  - exactly one data row is rendered;
  - clicking the sole row calls `onOpen` with the **backtest** id, and the train id is never reachable
    (no element bearing it).
  Existing `mockRuns` helper matches `url.endsWith('/api/runs')`; updated to match the `/api/runs`
  **pathname** so the new `?type=backtest` query still resolves.
- **Backend (Rust integration), `crates/server/tests/runs.rs`:** create one backtest + one train run
  (quick fake job), then assert:
  - `GET /api/runs` → both runs (no-filter parity);
  - `GET /api/runs?type=backtest` → only the backtest;
  - `GET /api/runs?type=train` → only the train;
  - `GET /api/runs?type=bogus` → empty list, `200`.

## Risks / blast radius

- `listRuns` signature gains an optional arg — every existing caller (`listRuns()`) is unaffected.
- Only `BacktestsList` and `runs.ts` change on the FE; `TrainingList` untouched.
- Server change is additive: the no-filter path is byte-identical, so `list_runs` parity for existing
  consumers (and the QE-411 batched-blocking test) is preserved.
- No golden/vintage artefacts touched — no determinism-boundary or golden-file impact.

## AC mapping

- *A mixed `listRuns` response renders only backtest rows* → FE regression test (decision 1/2).
- *No training run is reachable via the Backtests table* → train rows never render (nothing to click);
  asserted in the FE test. Server `?type=backtest` additionally prevents them being fetched.
