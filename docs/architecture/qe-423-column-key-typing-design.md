# QE-423 — `DataTable` generic typing: drop the `Record<string, unknown>` casts

`Phase: PreP3` · `Area: frontend / type-safety` · `Depends on: QE-258` · `Effort: M`

Spec of record: `### QE-423` in `docs/reviews/2026-07-15-team-improvement-review.md`.

## Problem / current casts (evidence)

`web/src/design/DataTable.tsx` declares the generics with a `Record` bound and a stringly-typed key:

```ts
export interface Column<Row> {
  key: string;                                             // any string — typos not caught
  render?: (value: unknown, row: Row, index: number) => ReactNode;  // value is unknown
}
export interface DataTableProps<Row extends Record<string, unknown>> { ... }
export function DataTable<Row extends Record<string, unknown>>(...) { ... }
```

Because `Row extends Record<string, unknown>`, every caller widens its real row type with an
intersection cast so it satisfies the bound, and casts the column array the same way. That defeats the
generic: `key: string` accepts any string, so a key typo compiles and silently renders `undefined`.

Four production call sites cast (verified at current HEAD `2561509`):

| Site | Column decl cast | `rows=` cast |
| --- | --- | --- |
| `web/src/app/backtest/BacktestsList.tsx` | `Column<RunListItem & Record<string, unknown>>[]` (L36) | `runs as (RunListItem & Record<string, unknown>)[]` (L94) |
| `web/src/app/training/TrainingList.tsx` | `Column<RunListItem & Record<string, unknown>>[]` (L35) | `runs as (RunListItem & Record<string, unknown>)[]` (L118) |
| `web/src/app/MarketData.tsx` | `Column<CoverageRow & Record<string, unknown>>[]` (L43) | `rows as (CoverageRow & Record<string, unknown>)[]` (L80) |
| `web/src/app/backtest/BacktestResult.tsx` | `Column<Trade & Record<string, unknown>>[]` (L197) | `result.trades as (Trade & Record<string, unknown>)[]` (L371) |

### Keyed vs derived columns per site (surveyed)

Every column at every site either (a) names a **real key** of its row type, or (b) is a **derived**
cell that ignores the cell value and computes from the whole row. To remove the casts without breaking
compilation I must type both.

- **BacktestsList** (`RunListItem`): `id`, `label`, `status`, `created_ms` — all real keys.
  `label`/`status` render from `row`, but their `key` is still a real key. No keyless columns.
- **TrainingList** (`RunListItem`): `id`, `label`, `status`, `created_ms` are real keys; **`gen` and
  `g1` are NOT keys of `RunListItem`** — they are derived from `row.train?.generation` / `row.train?.gate`.
  These are the derived-column case the ticket flags.
- **MarketData** (`CoverageRow`): `symbol`, `resolution`, `from`, `to`, `bars` — all real keys.
- **BacktestResult** (`Trade`): `id`, `symbol`, `side`, `entry`, `exit`, `hold`, `return_pct`,
  `result` — all real keys.

So only TrainingList needs the derived variant (two columns).

Also dead: `.qe-table th.is-sortable`, `.qe-table th.is-sortable:hover`, `.qe-table__sort` CSS in the
`CSS` string (`DataTable.tsx:19-21`). No `is-sortable`/`qe-table__sort` class is ever applied — there is
no sort feature. Ticket scope is **remove** (not implement).

## Chosen typing

Public `Column<Row>` becomes a **distributive union** over the row's keys plus a derived variant. `Row`
loses the `Record` bound entirely (unconstrained).

```ts
interface ColumnBase<Row> { header: ReactNode; align?: ColumnAlign; width?: string | number; }

// One member per real key K; render's value is exactly Row[K].
type KeyedColumn<Row, K extends keyof Row & string> = ColumnBase<Row> & {
  key: K;
  id?: never;
  render?: (value: Row[K], row: Row, index: number) => ReactNode;
};

// Derived/computed cell: no backing key, identified by `id`, renders from the row.
type DerivedColumn<Row> = ColumnBase<Row> & {
  key?: never;
  id: string;
  render: (value: undefined, row: Row, index: number) => ReactNode;
};

export type Column<Row> =
  | { [K in keyof Row & string]: KeyedColumn<Row, K> }[keyof Row & string]
  | DerivedColumn<Row>;

export interface DataTableProps<Row> { columns?: Column<Row>[]; rows?: Row[]; ... }
export function DataTable<Row>(...) { ... }
```

Why this shape:

- `{ [K in keyof Row & string]: KeyedColumn<Row, K> }[keyof Row & string]` produces a union with one
  member per key, each carrying `key: <literal>` and `render(value: Row[K], ...)`. A column literal is
  checked against the union; TS uses the literal `key` as a **discriminant** to (a) reject a non-existent
  key (assignable to no member) and (b) contextually type `render`'s `value` as exactly `Row[K]`.
- The **render signature stays `(value, row, index)`** for both variants — derived columns just get
  `value: undefined`. This makes the derived call-site change minimal: `key: 'gen'` → `id: 'gen'`, and
  the existing `render: (_v, row) => …` body is unchanged.

### Derived columns

TrainingList's two computed columns switch from `key: 'gen'`/`key: 'g1'` (fake keys) to
`id: 'gen'`/`id: 'g1'` (a stable column identity used only for the React `key` on `<th>`/`<td>`), keeping
their `render: (_v, row) => …` bodies verbatim. No other site uses a derived column.

### Component internals

Inside `DataTable`, the cell value and React key derive from the union:

```ts
const value = c.key !== undefined ? row[c.key] : undefined;   // Row[K] when keyed, else undefined
// React key: c.key ?? c.id  (both readable on the union)
// The union-of-functions render is invoked through one localized, commented cast:
const render = c.render as ((v: unknown, row: Row, i: number) => ReactNode) | undefined;
```

The single internal cast is unavoidable and standard: a heterogeneous column array is a union of
differently-typed render signatures, which TS cannot call with a single `value`. The cast is confined to
the generic component; it does **not** affect call-site safety, which is what the AC measures. `keyField`
stays `keyof Row & string` too (a real key) for the row React key.

QE-422 a11y (`role="button"` / `tabIndex` / `onKeyDown` gated on `onRowClick`) is untouched — only the
column/value plumbing changes.

## Dead-CSS removal

Delete the three dead rules (`is-sortable`, `is-sortable:hover`, `qe-table__sort`) from the `CSS`
template. No class references exist to update. No sort feature is added (out of scope).

## Test plan

- **Unit (unchanged, must stay green):** `DataTable.test.tsx` (QE-422 keyboard rows) and
  `primitives.test.tsx` (`DataTable renders rows and fires onRowClick`). Their local `Row` types still
  satisfy the now-unconstrained generic, so no change needed.
- **AC compile-error assertion (new):** a type-level test using `// @ts-expect-error` that a column with
  a non-existent key fails to compile, plus a positive assertion that a real key + wrong render-value
  usage fails. Placed in `src` (which `tsconfig.app.json` `include: ["src"]` type-checks under
  `npm run build`), so `tsc -b` genuinely exercises the guards. Verified in a spike: removing the guard
  makes `tsc` emit `TS2322: Type '"nope"' is not assignable to …` (exit 2); with the guard, exit 0.
- **Gates:** `npm run lint`, `npm run build` (`tsc -b && vite build` — the core gate), `npm test`
  (`vitest run`), all from `web/`.

## Risks / blast radius

- `DataTable` is a hot shared component (QE-422/QE-410/QE-408 just touched it). Change is purely
  type-level plus the dead-CSS delete and the value/key plumbing; runtime render output is identical
  (derived columns already rendered from `row`; keyed cells already read `row[key]`).
- The distributive-union pattern's contextual typing was de-risked by spike before implementation
  (typo → compile error; `render` value correctly typed as `Row[K]`; derived variant compiles).
- Frontend-only; no Rust, no golden/vintage, no API/wire change.

## Out of scope

A real sort feature; QE-422 a11y (merged); QE-424 error boundary.
