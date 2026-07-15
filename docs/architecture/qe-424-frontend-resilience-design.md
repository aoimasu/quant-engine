# QE-424 — Frontend resilience: error boundary + tests for the list/401/deep-link seams

`Phase: PreP3` · `Area: frontend / testing + resilience` · `Depends on: QE-259, QE-261`

Spec of record: `### QE-424` in `docs/reviews/2026-07-15-team-improvement-review.md` (lines 626-639).

## Problem / current gaps

1. **No React error boundary anywhere.** `web/src/app/App.tsx` renders the authed `AppShell` (and each
   Research screen) with no boundary. A render throw in any screen (a bad payload shape, a null deref)
   propagates to the React root and **blanks the whole SPA** — the user sees a white page, no nav, no
   recovery. `grep -rin "getDerivedStateFromError|componentDidCatch|ErrorBoundary" web/src` → nothing.

2. **The highest-risk seams' App-level / end-to-end behaviour is unevenly tested.** Inventory of what
   exists today (component/hook level):
   - **Seam 1 — mixed-type list filtering (QE-408).** `BacktestsList.test.tsx` has a strong
     component-level test ("renders only backtest rows from a mixed payload…") asserting the train row's
     id/label are absent and exactly one `tbody tr` renders. **FAILS on regression** (drop the
     `.filter((r) => r.type === 'backtest')` in `BacktestsList.tsx:34` → the train row appears). Solid at
     the component level; there is **no App-level** version exercising App → AppShell → BacktestsArea →
     BacktestsList.
   - **Seam 2 — 401 → Login (QE-409).** `useRunListPolling.test.tsx` has a hook-level 401 test; and
     `App.test.tsx` **already** has an App-level flip ("flips back to Login when a mid-session API call
     returns 401"). **FAILS on regression** (remove the `onUnauthorized` subscription in `App.tsx:64-69`
     → the shell never flips). Covered at the level the AC wants — reference, do not duplicate.
   - **Seam 3 — list auto-refresh of a running run (QE-410).** `BacktestsList.test.tsx` has a strong
     test ("live-refreshes a running row and stops polling once it is terminal") advancing 42%→73%→
     SUCCEEDED and asserting polling stops. **FAILS on regression** (break `useRunListPolling`'s
     re-schedule → the percent never advances). Covered — reference/extend, do not duplicate. Note:
     `BacktestsArea`/`TrainingArea` do **not** forward `pollMs` to their lists/monitors, so an App-level
     variant would poll at the 2 s default (slow/flaky); the component-level test is the correct level.
   - **Seam 4 — the router-less Training→Backtest deep-link (`App.tsx:36-40`).** `TrainingMonitor.test.tsx`
     asserts the monitor **calls** `onBacktestVintage(vintage)`; `BacktestsArea.test.tsx` asserts the area
     **preselects** `initialVintage`. **But nothing tests the two halves wired together through `App.tsx`'s
     bespoke `openBacktestForVintage` / `backtestVintage` state.** This end-to-end path is the explicitly
     called-out gap: a regression in `App.tsx`'s wiring (e.g. not passing `backtestVintage` into
     `BacktestsArea`) is caught by **no** current test.

## Design

### Error boundary (`web/src/app/ErrorBoundary.tsx`) — genuinely new

- **Dep-free class component** (no new dependency): `static getDerivedStateFromError(error)` records the
  error into state; `componentDidCatch(error, info)` logs to `console.error` for diagnostics (no telemetry
  dep). React requires a class for boundaries; this stays local and small.
- **Recoverable fallback** (not a dead end): when `state.error` is set, render a `role="alert"` panel —
  "Something went wrong" + a short explanation + a **"Try again"** `<button>` whose `onClick` calls
  `reset()` (`setState({ error: null })`), clearing the error so the wrapped subtree re-renders. If the
  underlying cause is deterministic it re-throws and shows the fallback again — expected; a transient
  cause recovers. Styled with inline CSS custom properties so it renders even if a design primitive is
  the thing that threw, and stays theme-aware/accessible.
- **Reset on auth change:** a `resetKeys` prop; `componentDidUpdate` clears a shown error when any
  `resetKeys` entry changes (shallow compare). `App.tsx` passes `resetKeys={[me?.email]}` so an
  auth/session change auto-clears a stale error. (An auth flip to `unauth` also unmounts the boundary
  entirely, since `App` returns `<Login>` from an earlier branch — so the unauth/Login shell is never
  wrapped and cannot be blanked by a screen throw.)
- **Placement:** wraps the **whole authed `AppShell`** return in `App.tsx` (the "top-level boundary around
  the authed shell" the spec asks for). The `loading` and `unauth` early returns are outside it.

### Tests (Vitest + Testing Library)

New file `web/src/app/App.seams.test.tsx` (App-level integration + boundary) plus references to the
existing component/hook tests. Every test **fails if its seam regresses**:

- **Error boundary — fallback on throw + recovery (new).** Render `<ErrorBoundary>` around a child that
  throws while a flag is set; assert the `role="alert"` fallback shows (not a blank render), click
  "Try again" with the flag cleared, assert the child content is back. Fails if the boundary doesn't
  catch (no `getDerivedStateFromError`) or the reset control doesn't clear the error.
- **Error boundary — reset on `resetKeys` change (new).** Assert a `resetKeys` change auto-clears the
  fallback (proves the auth-change recovery wired in `App.tsx`).
- **Seam 4 — deep-link end-to-end through `App.tsx` (new, the key gap).** Render `<App />` authed, click
  the **Training** nav, open a succeeded training run (its `train.vintage` sealed), click "Backtest this
  vintage", and assert we land on the **New backtest** form with the **Vintage** select preselected to
  that sealed vintage (not the first vintage). Exercises `App.openBacktestForVintage` → `backtestVintage`
  → `BacktestsArea initialVintage` → `NewBacktest`. Fails if `App.tsx`'s router-less wiring regresses
  (the select would fall back to the first vintage). No timers: every fetch returns terminal data.
- **Seam 1 — mixed-type filtering at App level (new App-level; complements the component test).** Render
  `<App />` authed on Backtests with a mixed `GET /api/runs` payload; assert only the backtest row is
  present and the train row's id/label are absent. Fails if the client-side `type` filter regresses.
- **Seam 2 — 401 → Login:** reference `App.test.tsx`'s existing App-level flip (already at the AC's level).
- **Seam 3 — running-row live refresh:** reference `BacktestsList.test.tsx`'s existing QE-410 test.

## Risks / blast radius

- Frontend-only; no Rust, no golden, no backend touched. New `ErrorBoundary.tsx`, a one-line wrap in
  `App.tsx`, and one new test file; existing tests unchanged.
- The boundary wraps the whole shell, so a screen throw hides the nav until "Try again"; acceptable per
  spec (recoverable fallback). Alternative (wrap only the content area, keep nav live) was rejected as it
  contradicts the spec's "top-level boundary around the authed shell".
- Boundary tests emit React's expected `console.error` for caught errors; suppressed within those tests
  to keep output clean.
