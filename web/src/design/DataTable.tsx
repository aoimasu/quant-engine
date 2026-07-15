import type { HTMLAttributes, ReactNode } from 'react';
import { injectCss } from './injectCss';

/* CSS ported verbatim from the Claude Design "Quant Engine Design System"
   (components/data/DataTable.jsx). */
const CSS = `
.qe-table-wrap { width: 100%; overflow-x: auto; }
.qe-table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.qe-table thead th {
  position: sticky; top: 0; z-index: 1;
  text-align: left; padding: 8px var(--pad-cell);
  font-family: var(--font-mono); font-size: 10px; font-weight: var(--fw-medium);
  letter-spacing: var(--ls-caps); text-transform: uppercase; color: var(--text-muted);
  background: var(--surface-base); border-bottom: var(--border-w) solid var(--border-default);
  white-space: nowrap; user-select: none;
}
.qe-table th.is-num, .qe-table td.is-num { text-align: right; font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
.qe-table th.is-center, .qe-table td.is-center { text-align: center; }
.qe-table tbody td {
  padding: 10px var(--pad-cell); border-bottom: var(--border-w) solid var(--border-subtle);
  color: var(--text-primary); white-space: nowrap;
}
.qe-table tbody tr { transition: background var(--dur-fast) var(--ease-out); }
.qe-table tbody tr.qe-table__row--clickable { cursor: pointer; }
.qe-table tbody tr.qe-table__row--clickable:focus-visible {
  outline: 2px solid var(--accent); outline-offset: -2px; background: var(--surface-hover);
}
.qe-table--hover tbody tr:hover { background: var(--surface-hover); }
.qe-table--striped tbody tr:nth-child(even) { background: rgba(255,255,255,0.015); }
.qe-table tbody tr:last-child td { border-bottom: none; }
.qe-table--compact tbody td { padding: 6px var(--pad-cell); }
.qe-table--compact thead th { padding: 6px var(--pad-cell); }
`;

injectCss('qe-table-css', CSS);

export type ColumnAlign = 'left' | 'right' | 'num' | 'center';

interface ColumnBase {
  header: ReactNode;
  align?: ColumnAlign;
  width?: string | number;
}

/**
 * A column bound to a real key `K` of `Row`. `key` is constrained to `keyof Row & string`, so a typo is
 * a compile error (QE-423 AC), and `render` receives the correctly-typed cell value `Row[K]` (not
 * `unknown`). With no `render`, the cell renders `row[key]` directly.
 */
type KeyedColumn<Row, K extends keyof Row & string> = ColumnBase & {
  key: K;
  id?: never;
  render?: (value: Row[K], row: Row, index: number) => ReactNode;
};

/**
 * A derived/computed column with no backing key — identified by a stable `id` (used only for the React
 * key on `<th>`/`<td>`) and rendered from the whole `row` (`value` is always `undefined`). Use this for
 * cells that don't map to a single `Row` field (e.g. a value read from a nested/optional sub-object).
 */
type DerivedColumn<Row> = ColumnBase & {
  key?: never;
  id: string;
  render: (value: undefined, row: Row, index: number) => ReactNode;
};

/**
 * A `DataTable` column: either a {@link KeyedColumn} for each real key of `Row` (distributive union, so
 * `render`'s value is exactly `Row[key]` and a non-existent key fails to compile) or a
 * {@link DerivedColumn} for computed cells.
 */
export type Column<Row> =
  | { [K in keyof Row & string]: KeyedColumn<Row, K> }[keyof Row & string]
  | DerivedColumn<Row>;

export interface DataTableProps<Row>
  extends Omit<HTMLAttributes<HTMLDivElement>, 'children'> {
  columns?: Column<Row>[];
  rows?: Row[];
  keyField?: keyof Row & string;
  hover?: boolean;
  striped?: boolean;
  compact?: boolean;
  onRowClick?: (row: Row, index: number) => void;
}

/** DataTable — configurable dense table for positions, orders, results. */
export function DataTable<Row>({
  columns = [],
  rows = [],
  keyField,
  hover = true,
  striped = false,
  compact = false,
  onRowClick,
  className = '',
  ...rest
}: DataTableProps<Row>) {
  const cls = [
    'qe-table',
    hover ? 'qe-table--hover' : '',
    striped ? 'qe-table--striped' : '',
    compact ? 'qe-table--compact' : '',
  ]
    .filter(Boolean)
    .join(' ');
  const alignCls = (a?: ColumnAlign) =>
    a === 'right' || a === 'num' ? 'is-num' : a === 'center' ? 'is-center' : '';
  return (
    <div className={`qe-table-wrap ${className}`.trim()} {...rest}>
      <table className={cls}>
        <thead>
          <tr>
            {columns.map((c) => (
              <th
                key={String(c.key ?? c.id)}
                className={alignCls(c.align)}
                style={c.width ? { width: c.width } : undefined}
              >
                {c.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            // When `onRowClick` is set the row is an activation control: expose it as a
            // keyboard-operable button (Enter/Space) so navigation isn't mouse-only (QE-422).
            // Rows without a handler stay plain, non-interactive `<tr>`s.
            <tr
              key={keyField ? String(row[keyField]) : i}
              className={onRowClick ? 'qe-table__row--clickable' : undefined}
              role={onRowClick ? 'button' : undefined}
              tabIndex={onRowClick ? 0 : undefined}
              onClick={onRowClick ? () => onRowClick(row, i) : undefined}
              onKeyDown={
                onRowClick
                  ? (e) => {
                      if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
                        e.preventDefault(); // Space would otherwise scroll the page.
                        onRowClick(row, i);
                      }
                    }
                  : undefined
              }
            >
              {columns.map((c) => {
                // Keyed column → the cell value is `row[key]` (typed `Row[key]`); a derived column has
                // no backing key, so its value is `undefined` and it renders from `row`.
                const value = c.key !== undefined ? row[c.key] : undefined;
                // `Column<Row>` is a union of per-key render signatures; a single `value` cannot satisfy
                // all of them at once, so invoke through one localized unifying cast. Call-site typing
                // stays precise — this cast is confined to the generic component (QE-423).
                const render = c.render as
                  | ((value: unknown, row: Row, index: number) => ReactNode)
                  | undefined;
                return (
                  <td key={String(c.key ?? c.id)} className={alignCls(c.align)}>
                    {render ? render(value, row, i) : (value as ReactNode)}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
