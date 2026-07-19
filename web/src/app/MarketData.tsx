import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon } from '../design';
import type { BadgeVariant, Column } from '../design';
import { injectCss } from '../design/injectCss';
import { getCoverage, type CoverageProvenance, type CoverageRow } from '../api/runs';

const CSS = `
.qe-md { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-md__hd { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; }
.qe-md__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-md__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-md__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
.qe-md__prov { display: inline-flex; align-items: center; gap: 6px; }
.qe-md__cal { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); }
`;

injectCss('qe-md-css', CSS);

function fmtDay(ms: number): string {
  const d = new Date(ms);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/**
 * Provenance → badge appearance (QE-465, design §8.2 — nobody trains on synthetic as real). `synthetic`
 * is the loud amber `warn`; `real` is the positive `up`; `unknown` (legacy untagged) is a neutral flag —
 * **never** softened to `real`.
 */
const PROVENANCE_BADGE: Record<CoverageProvenance, { variant: BadgeVariant; label: string }> = {
  real: { variant: 'up', label: 'REAL' },
  synthetic: { variant: 'warn', label: 'SYNTHETIC' },
  unknown: { variant: 'neutral', label: 'UNKNOWN' },
};

/** Render one row's provenance badge (+ a muted calibration tag on real rows). */
function ProvenanceCell({ row }: { row: CoverageRow }) {
  const badge = PROVENANCE_BADGE[row.provenance] ?? PROVENANCE_BADGE.unknown;
  return (
    <span className="qe-md__prov">
      <Badge variant={badge.variant}>{badge.label}</Badge>
      {row.provenance === 'real' && (
        <span className="qe-md__cal" title="whether this run's tradability inputs were measured">
          {row.calibrated ? 'calibrated' : 'uncalibrated'}
        </span>
      )}
    </span>
  );
}

export interface MarketDataProps {
  /** Open the ingest-trigger screen (QE-465). Omit to hide the "Ingest data" affordance. */
  onNewIngest?: () => void;
}

/**
 * Market data (coverage) — read-only table of symbols × ranges from `GET /api/market-data/coverage`,
 * QE-465 adds the **provenance column** (`real`/`synthetic`/`unknown` per row) and the ingest-trigger
 * entry point. The server already splits a mixed store into **one coverage row per provenance run**
 * (`qe_storage::coverage`), so the table marks each row and does **no** client-side merging — an
 * instrument with interleaved provenance shows as several explicitly-badged rows, never one blended
 * unmarked range (design §8.2).
 */
export function MarketData({ onNewIngest }: MarketDataProps = {}) {
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

  const columns: Column<CoverageRow>[] = [
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
    {
      key: 'provenance',
      header: 'Provenance',
      render: (_v, row) => <ProvenanceCell row={row} />,
    },
    { key: 'from', header: 'From', align: 'num', render: (v) => fmtDay(Number(v)) },
    { key: 'to', header: 'To', align: 'num', render: (v) => fmtDay(Number(v)) },
    { key: 'bars', header: 'Bars', align: 'num', render: (v) => Number(v).toLocaleString('en-US') },
  ];

  return (
    <div className="qe-md">
      <div className="qe-md__hd">
        <div>
          <h2>Market data</h2>
          <div className="qe-md__sub">
            Read-only coverage of the local market-data store — the symbols, date ranges, and data{' '}
            <strong>provenance</strong> present. An instrument with mixed provenance appears as one marked
            row per run (real vs synthetic), never a single unmarked range.
          </div>
        </div>
        {onNewIngest && (
          <Button variant="primary" onClick={onNewIngest} iconLeft={<Icon name="arrow-right" size={15} />}>
            Ingest data
          </Button>
        )}
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
        {rows != null && rows.length > 0 && <DataTable columns={columns} rows={rows} />}
      </Card>
    </div>
  );
}
