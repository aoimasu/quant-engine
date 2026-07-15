# QE-410 — Run-list read path: shared polling hook, live list refresh, server pagination/projection/filter

`Area: frontend + backend / read-path` · `Effort: M`

Spec of record: `### QE-410` in `docs/reviews/2026-07-15-team-improvement-review.md`.

## Current state

**Frontend.**
- `BacktestsList.tsx` / `TrainingList.tsx` fetch `listRuns()` **once on mount** (no interval), so a
  `RUNNING {pct}%` cell freezes until you navigate away.
- The bounded-retry polling `useEffect` (getRun → terminal-stop, streak-capped retry) is duplicated
  near-verbatim in `BacktestResult.tsx` and `TrainingMonitor.tsx`.
- `statusBadge()` is copied in both detail screens; `statusVariant()` + `fmtDate()` are copied in both
  list screens; the `.qe-run` progress-bar card markup+CSS is copied in both detail screens.
- `listRuns(type?)` returns a bare `RunMeta[]`; the union `RunMeta = BacktestRunMeta | TrainRunMeta`
  (QE-406) requires `params`.

**Backend.**
- `GET /api/runs` → `list_runs` returns the **entire** history newest-first, reading every run's full
  `meta.json` every call. `ListRunsQuery` has only `?type=` (QE-408, index-level skip). The whole batch
  runs in ONE `spawn_blocking` closure (QE-411). No limit, offset, cursor, or status filter.
- `RunMeta` (full) is the wire shape for both the list and `GET /runs/{id}`.
- `IndexEntry` already carries `{ id, type, created_ms, label }` — `label` is the vintage id (backtest)
  or `"train {start}→{end}"` (train). This is **already read** by `read_index()`, for free.

## Decisions

### Pagination: id-anchored cursor (not offset)

The AC requires "paginates **stably** under concurrent creates". The index is append-only; the
newest-first view is `index.iter().rev()`. New runs **prepend** to the newest-first view, so an
**offset** would skip/duplicate rows the moment a run is created between two page fetches.

An **id-anchored cursor** is stable: `?cursor=<id>` means "the page of runs strictly older than the run
with this id". We locate the cursor id's position in the newest-first view and start at `pos+1`. New
creates are always *newer* than any cursor id, so they only ever appear on page 1 (no cursor) and can
never shift, skip, or duplicate an older cursor-paginated page. `next_cursor` is the id of the last row
returned when more older entries remain, else `null`. Unknown cursor ⇒ empty continuation (never
restart-from-top, which would duplicate).

Default `limit = 50`, capped at `200`.

### Response envelope + slim projection

`list_runs` now returns `{ "runs": RunListItem[], "next_cursor": string | null }` (was a bare array).

`RunListItem` (slim) = `{ id, type, label, status, progress, created_ms, train? }`:
- `label` comes from the **index** (free; no meta read) and feeds the lists' identifying column
  (vintage / window) — so the lists stay meaningful without shipping heavy `params`.
- `status` / `progress` / `train` come from `meta.json` and are the **live** fields the lists must
  refresh (`RUNNING {pct}%`, Generation, G1). `train` is live progress (same category as `progress`),
  so it belongs in a live-refreshing list; it is small.
- **Deferred** to `GET /runs/{id}` (full `RunMeta`): the heavy immutable `params` (universe arrays,
  costs, strategy config). `GET /runs/{id}` is unchanged and still returns the full `RunMeta`, so the
  detail screens (and Re-run) get everything.

Trade-off: the lists drop the params-only columns (backtest Window/Res; train Res) — an intentional
slimming per the spec ("deferring heavy `params`"). Vintage (backtest) and window (train) survive via
`label`. Generation/G1 survive via `train`.

Consequence: the slice is taken at the **index** level inside the existing single `spawn_blocking`
closure (QE-411) — with only `?type=` (index-level) we read exactly the page's `meta.json` files, never
all of history. A `?status=` filter must read a meta to evaluate status (status is not in the index),
so it may scan past filtered rows to fill a page — inherent, documented.

### `ListRunsQuery` composition

Extend to `{ type?, status?, limit?, cursor? }`. Order inside the blocking loop, per candidate
(newest-first, from the cursor start):
1. `type` — index-level skip (no meta read), preserving QE-408.
2. read `meta.json`; skip if missing (QE-411 semantics).
3. `status` — skip if `meta.status != status`.
4. push slim item; stop at `limit`.

All four compose; each is independently optional.

### Frontend

- `usePollingRun(runId, { pollMs, failedFallback })` → `{ meta, error, retrying }`. Owns the shared
  bounded-retry (`MAX_POLL_FAILURES`) + terminal-stop (succeeded/failed) + non-overlap (chained
  `setTimeout`, next tick scheduled only after the previous resolves) logic. On `failed` it sets
  `error = meta.error ?? failedFallback`. Both detail screens consume it; the duplicated `useEffect`
  blocks are deleted. `BacktestResult` fetches the result once in a **separate** effect keyed on
  `meta.status === 'succeeded'` (the only backtest-specific bit).
- `useRunListPolling({ type?, status?, pollMs })` → `{ runs, error }`. Fetches `listRuns()` on mount
  and every `pollMs` **while any row is queued/running**, stops when all terminal, and guards
  overlapping requests with an in-flight ref. Shared by both list screens.
- Promote to the design layer (used by all four screens; copies deleted):
  - `StatusBadge` (`design/StatusBadge.tsx`) — `{ status, pct? }`; `RUNNING {pct}%` when `pct` given
    (lists), plain label otherwise (details). Replaces `statusBadge()` + `statusVariant()`.
  - `RunProgress` (`design/RunProgress.tsx`) — the `.qe-run` progress-bar card; `{ progress, status }`.
  - `fmtRunDate` (`design/formatRunDate.ts`) — the UTC `YYYY-MM-DD HH:MM` formatter.
- `listRuns(opts?)` signature → `{ type?, status?, limit?, cursor? }`, returns `RunPage`
  (`{ runs: RunListItem[]; nextCursor: string | null }`). `RunListItem` is a new slim TS type mirroring
  the Rust projection. `RunMeta` union + `isBacktestRun`/`isTrainRun` are **unchanged** (still the
  `GET /runs/{id}` shape the detail screens consume).

## Test plan

**Rust (`crates/server/tests/runs.rs`)** — update existing list assertions to the `{ runs, next_cursor }`
envelope, and add:
- default cap + pagination: create > default rows is capped; cursor walks pages with no overlap.
- **stability under an interleaved create**: page 1 → cursor; create more; page 2 via cursor returns
  exactly the original older rows, none of the new creates.
- `?status=` filters; `?status=`+`?type=` compose; `?type=` still works (QE-408 parity).
- slim projection shape: list rows carry `label`, no `params`; `GET /runs/{id}` still has `params`.

**Frontend (vitest)**
- `useRunListPolling` via a list screen: a running row's percent updates on the next poll, then polling
  stops once the row is terminal (fetch called no more).
- `usePollingRun`: retry note on transient error → recovery; terminal-stop.
- list consumes the slim envelope (`label` column, no crash on absent `params`).

## Risks / blast radius

- **Wire break**: `GET /api/runs` becomes an envelope + slim rows. Only the SPA consumes it; QE-264
  (metrics column) is not yet built. QE-408 tests updated in lockstep.
- **Design layer imports run types**: `StatusBadge`/`RunProgress` type-import `RunStatus`/`Progress`
  from `api/runs` — sanctioned by the ticket ("promote to the design layer").
- No golden/vintage bytes touched. `GET /runs/{id}`, create, shutdown/reconcile (QE-407), the batched
  `spawn_blocking` (QE-411), and the QE-406 union are all preserved.
</content>
</invoke>
