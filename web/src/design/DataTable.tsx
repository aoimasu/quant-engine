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
.qe-table th.is-sortable { cursor: pointer; }
.qe-table th.is-sortable:hover { color: var(--text-secondary); }
.qe-table__sort { opacity: 0.5; margin-left: 4px; font-size: 9px; }
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

export interface Column<Row> {
  key: string;
  header: ReactNode;
  align?: ColumnAlign;
  width?: string | number;
  render?: (value: unknown, row: Row, index: number) => ReactNode;
}

export interface DataTableProps<Row extends Record<string, unknown>>
  extends Omit<HTMLAttributes<HTMLDivElement>, 'children'> {
  columns?: Column<Row>[];
  rows?: Row[];
  keyField?: string;
  hover?: boolean;
  striped?: boolean;
  compact?: boolean;
  onRowClick?: (row: Row, index: number) => void;
}

/** DataTable — configurable dense table for positions, orders, results. */
export function DataTable<Row extends Record<string, unknown>>({
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
                key={c.key}
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
              {columns.map((c) => (
                <td key={c.key} className={alignCls(c.align)}>
                  {c.render ? c.render(row[c.key], row, i) : (row[c.key] as ReactNode)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
