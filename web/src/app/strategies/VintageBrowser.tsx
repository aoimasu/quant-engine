import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, listVintages, type VintageListItem } from '../../api/runs';

const CSS = `
.qe-vb { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-vb__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-vb__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-vb__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-vb__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-vb-css', CSS);

export interface VintageBrowserProps {
  onOpen: (vintageId: string) => void;
  /** Open the read-only QE-466 vintage leaderboard/comparison. Absent ⇒ no leaderboard entry point. */
  onLeaderboard?: () => void;
}

/**
 * VintageBrowser (QE-457) — the read-only browse table of sealed vintages from `GET /api/vintages`, mirroring
 * {@link import('../evolve/PoolBrowser').PoolBrowser}. Each row shows the vintage id, chromosome count,
 * worst-case loss and format version; a row-click opens the {@link import('./VintageInspector').VintageInspector}.
 * Vintages are human-paced (not live), so this fetches once on mount rather than polling. Provenance is a
 * detail-only field (the list summary does not carry it), so it is surfaced in the inspector, not here.
 */
export function VintageBrowser({ onOpen, onLeaderboard }: VintageBrowserProps) {
  const [vintages, setVintages] = useState<VintageListItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listVintages()
      .then((v) => {
        if (!cancelled) setVintages(v);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : 'Failed to load vintages.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const columns: Column<VintageListItem>[] = [
    {
      key: 'id',
      header: 'Vintage',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-secondary)' }}>{String(v)}</span>
      ),
    },
    {
      id: 'chromosomes',
      header: 'Chromosomes',
      align: 'num',
      render: (_v, row) => <span style={{ fontFamily: 'var(--font-mono)' }}>{row.summary.chromosomes}</span>,
    },
    {
      id: 'worst_case_loss',
      header: 'Worst-case loss',
      align: 'num',
      render: (_v, row) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}>
          {row.summary.worst_case_loss == null ? '—' : row.summary.worst_case_loss.toFixed(3)}
        </span>
      ),
    },
    {
      id: 'format_version',
      header: 'Format',
      render: (_v, row) => <Badge variant="neutral">v{row.summary.format_version}</Badge>,
    },
    {
      id: 'content_hash',
      header: 'Content hash',
      render: (_v, row) => (
        <span
          style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}
          title={row.summary.content_hash}
        >
          {row.summary.content_hash.slice(0, 12)}…
        </span>
      ),
    },
  ];

  return (
    <div className="qe-vb">
      <div className="qe-vb__hd">
        <div>
          <h2>Strategies</h2>
          <div className="qe-vb__sub">
            Sealed vintages and the evolved genomes within them. Open one to inspect its provenance, gate
            evidence, composition, and holdout regime coverage.
          </div>
        </div>
        {onLeaderboard && (
          <Button
            variant="secondary"
            size="sm"
            onClick={onLeaderboard}
            iconLeft={<Icon name="layout-dashboard" size={15} />}
          >
            Leaderboard
          </Button>
        )}
      </div>

      {error && (
        <Callout variant="danger" title="Could not load vintages">
          {error}
        </Callout>
      )}

      <Card>
        {vintages == null && !error && <div className="qe-vb__empty">Loading vintages…</div>}
        {vintages != null && vintages.length === 0 && (
          <div className="qe-vb__empty">No sealed vintages yet. Run a train campaign to produce one.</div>
        )}
        {vintages != null && vintages.length > 0 && (
          <DataTable columns={columns} rows={vintages} keyField="id" onRowClick={(row) => onOpen(row.id)} />
        )}
      </Card>
    </div>
  );
}
