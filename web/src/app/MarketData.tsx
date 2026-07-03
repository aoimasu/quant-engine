import { useEffect, useState } from 'react';
import { Badge, Callout, Card, DataTable } from '../design';
import type { Column } from '../design';
import { injectCss } from '../design/injectCss';
import { getCoverage, type CoverageRow } from '../api/runs';

const CSS = `
.qe-md { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-md__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-md__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-md__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-md-css', CSS);

function fmtDay(ms: number): string {
  const d = new Date(ms);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/** Market data (coverage) — read-only table of symbols × ranges from `GET /api/market-data/coverage`. */
export function MarketData() {
  const [rows, setRows] = useState<CoverageRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getCoverage()
      .then((r) => {
        if (!cancelled) setRows(r);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Failed to load coverage.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const columns: Column<CoverageRow & Record<string, unknown>>[] = [
    {
      key: 'symbol',
      header: 'Symbol',
      render: (v) => <span style={{ fontFamily: 'var(--font-mono)', fontWeight: 600 }}>{String(v)}</span>,
    },
    {
      key: 'resolution',
      header: 'Resolution',
      render: (v) => <Badge variant="neutral">{String(v)}</Badge>,
    },
    { key: 'from', header: 'From', align: 'num', render: (v) => fmtDay(Number(v)) },
    { key: 'to', header: 'To', align: 'num', render: (v) => fmtDay(Number(v)) },
    { key: 'bars', header: 'Bars', align: 'num', render: (v) => Number(v).toLocaleString('en-US') },
  ];

  return (
    <div className="qe-md">
      <div className="qe-md__hd">
        <h2>Market data</h2>
        <div className="qe-md__sub">
          Read-only coverage of the local market-data store — the symbols and date ranges present.
        </div>
      </div>

      {error && (
        <Callout variant="danger" title="Could not load coverage">
          {error}
        </Callout>
      )}

      <Card>
        {rows == null && !error && <div className="qe-md__empty">Loading coverage…</div>}
        {rows != null && rows.length === 0 && (
          <div className="qe-md__empty">The market-data store is empty. Ingest data to populate it.</div>
        )}
        {rows != null && rows.length > 0 && (
          <DataTable columns={columns} rows={rows as (CoverageRow & Record<string, unknown>)[]} />
        )}
      </Card>
    </div>
  );
}
