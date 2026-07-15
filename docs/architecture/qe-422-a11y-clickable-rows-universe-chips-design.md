# QE-422 — Keyboard / screen-reader access for clickable table rows and universe chips

Status: design note (pre-implementation)
Ticket: `### QE-422` in `docs/reviews/2026-07-15-team-improvement-review.md` (lines 588-603)
Area: frontend / accessibility (`web/`)

## Problem / current a11y gaps (evidence)

### 1. Clickable table rows are mouse-only
`web/src/design/DataTable.tsx` renders each body row as a bare `<tr>` that gets an
`onClick` and a `cursor: pointer` style when `onRowClick` is set (current code, ~lines
96-101):

```tsx
<tr
  key={keyField ? String(row[keyField]) : i}
  onClick={onRowClick ? () => onRowClick(row, i) : undefined}
  style={onRowClick ? { cursor: 'pointer' } : undefined}
>
```

There is no `role`, no `tabIndex`, and no `onKeyDown`. The row cannot receive keyboard
focus and cannot be activated by keyboard. Row-click is the *primary* navigation into a
run: `BacktestsList` (`onRowClick={(row) => onOpen(row.id)}`) and `TrainingList`
(`onRowClick={(row) => onOpen(row.id)}`) both open the result screen this way. So a
keyboard/SR user cannot open a run at all. `MarketData` uses `DataTable` *without*
`onRowClick`, so those rows are (correctly) non-interactive and must stay that way.

### 2. Universe chips advertise checkbox semantics an element can't deliver
`web/src/app/backtest/NewBacktest.tsx` (current, ~lines 206-223) renders each universe
symbol as a design `Tag` with `role="checkbox"` + `aria-checked` + `onClick`:

```tsx
<Tag mono role="checkbox" aria-checked={on} aria-label={s} onClick={() => toggleSymbol(s)}>
  {s}
</Tag>
```

But `web/src/design/Tag.tsx` renders a plain `<span>` (~line 38) with no `tabIndex` and no
key handler. The ARIA says "checkbox" but the element is not focusable and not
keyboard-togglable — the interactivity is fake. A SR announces a checkbox the user can
never reach or toggle by keyboard.

`Tag` is also used purely as a non-interactive label elsewhere
(`web/src/app/backtest/BacktestResult.tsx:250,403,409` — no `onClick`/role). Those usages
must NOT be made interactive.

## Chosen accessible patterns

### Rows — `role="button"` + `tabIndex=0` + Enter/Space keydown (only when `onRowClick` set)
- **Role = `button`, not `link`.** `DataTable` is a generic component; `onRowClick` is an
  arbitrary activation callback, not necessarily URL navigation, and there is no `href`.
  `role="link"` would promise href/location semantics (and by ARIA only Enter activates a
  link). `role="button"` honestly says "activates an action" and is activated by BOTH
  Enter and Space — which is exactly the ticket requirement. So `button` is the correct
  generic semantic here.
- Applied ONLY when `onRowClick` is provided. Rows without a click handler stay plain
  `<tr>` with no role/tabIndex/keydown (preserves `MarketData`'s non-interactive rows).
- `onKeyDown`: activate on `Enter` and `Space` (`' '`/`'Spacebar'`), calling the SAME
  `onRowClick(row, i)` as `onClick`. `preventDefault()` on the key event (stops Space from
  scrolling the page).
- Visible keyboard focus ring via a `qe-table__row--clickable` class + a `:focus-visible`
  outline using the design accent token, so focus is perceivable.
- Accessible name: the row's concatenated cell text (run id / vintage / status / date) is
  descriptive enough to serve as the button's name; no extra label prop added (keeps the
  generic component and the diff minimal).

### Universe chips — real native `<input type="checkbox">` (kept out of `Tag`)
- Use a native checkbox for correctness/SR support (preferred option in the ticket). The
  chip becomes a `<label class="qe-new__chip">` wrapping a visually-hidden-but-focusable
  native `<input type="checkbox">` plus the existing `<Tag mono>` for the visual token.
  - Native checkbox = real `role=checkbox`, focusable, Space toggles natively, announces
    checked state + label. `aria-label={symbol}` gives the accessible name.
  - An `onKeyDown` also toggles on Enter (native checkboxes ignore Enter) so BOTH Enter and
    Space toggle, matching the ticket's "Space/Enter".
  - Selection behavior is unchanged: `onChange`/keydown call the existing `toggleSymbol(s)`
    (add/remove in the `Set`); submit still builds `universe` from `selected`.
- **`Tag` is left untouched** — no forced interactivity — so `BacktestResult`'s label-only
  `Tag`s are unaffected. The fake `role="checkbox"`/`aria-checked` on the old `Tag` chip is
  removed (the native input now carries the real semantics).
- The input is hidden with an sr-only-but-focusable style (position:absolute; opacity:0 —
  NOT `display:none`, which would kill focus). Focus ring shows on the visible chip via
  `:focus-within` using the design `--ring` token.

## Test plan (Vitest + Testing Library, jsdom)
- `DataTable.test.tsx` (new): (a) a clickable row exposes `role="button"` + `tabIndex=0`;
  focusing it and pressing **Enter** fires `onRowClick` with the right row; pressing
  **Space** fires it again — both proven separately, non-vacuous. (b) When no `onRowClick`
  is passed, rows have NO role/tabIndex and keydown does nothing.
- `BacktestsList.test.tsx` (extend): a run opens from the list via keyboard alone — focus
  the row, press Enter → `onOpen('run-xyz')`; press Space → same id. Proves AC end-to-end
  through the real list component.
- `NewBacktest.test.tsx` (extend): a universe chip is queryable as `getByRole('checkbox',
  { name: 'BTCUSDT' })` with the correct `checked` state; focusing it and pressing Space
  toggles selection off (reflected in the `N/total` count and/or submitted `universe`), and
  pressing Enter toggles it back — proving keyboard toggle for both keys. Assert the chip is
  a real checkbox and checked state announces correctly.

## Risks / blast radius
- `DataTable` is shared by `BacktestsList`, `TrainingList`, `MarketData`, `BacktestResult`.
  The row change is gated on `onRowClick`, so only the two navigating lists gain
  interactivity; `MarketData`/`BacktestResult` (no `onRowClick`) render byte-identical rows.
- Row `role="button"` on a `<tr>` is valid ARIA; the button's implicit-table-semantics
  concern is minimal here since the tables are presentational grids of runs, not data grids
  needing full grid keyboard nav (out of scope).
- Existing `BacktestsList` "opens on row click" and `NewBacktest` universe tests keep
  passing (mouse path unchanged); new tests cover the keyboard path.
- Scope guard: NO QE-423 work (generic typing / dead sort CSS untouched); QE-408 client
  filter untouched.
