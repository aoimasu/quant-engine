import { Button, Callout, Card, DataTable, Icon, StatusBadge, formatRunDate } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { useRunListPolling } from '../../api/useRunListPolling';
import type { RunListItem } from '../../api/runs';

const CSS = `
.qe-cl { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-cl__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-cl__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-cl__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-cl__actions { display: flex; align-items: center; gap: 10px; }
.qe-cl__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-cl-css', CSS);

export interface CampaignListProps {
  onOpen: (id: string) => void;
  onNew: () => void;
  onBrowsePools: () => void;
  /** Live-poll cadence while any row is queued/running (ms). Overridable for tests. */
  pollMs?: number;
}

/**
 * Evolve-campaign list — the `type:"evolve"` rows from `GET /api/runs?type=evolve` (the slim QE-410
 * projection). Mirrors {@link TrainingList}: polls live via {@link useRunListPolling} while any row is
 * queued/running and stops once all rows are terminal; the window column reads the server's slim `label`.
 * A "Browse pools" action crosses into the {@link PoolBrowser}.
 */
export function CampaignList({ onOpen, onNew, onBrowsePools, pollMs }: CampaignListProps) {
  const { runs: allRuns, error } = useRunListPolling({ type: 'evolve', pollMs });
  const runs = allRuns?.filter((r) => r.type === 'evolve') ?? null;

  const columns: Column<RunListItem>[] = [
    {
      key: 'id',
      header: 'Campaign',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}>
          {String(v).slice(0, 8)}
        </span>
      ),
    },
    {
      key: 'label',
      header: 'Window / mode',
      render: (_v, row) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-secondary)' }}>
          {row.label || '—'}
        </span>
      ),
    },
    {
      key: 'status',
      header: 'Status',
      render: (_v, row) => <StatusBadge status={row.status} pct={row.progress.pct} />,
    },
    {
      key: 'created_ms',
      header: 'Created',
      align: 'num',
      render: (v) => <span style={{ color: 'var(--text-tertiary)' }}>{formatRunDate(Number(v))}</span>,
    },
  ];

  return (
    <div className="qe-cl">
      <div className="qe-cl__hd">
        <div>
          <h2>Indicator evolution</h2>
          <div className="qe-cl__sub">
            Launch a GP campaign to evolve indicator formulas, watch the MAP-Elites archive fill, and
            govern the frozen pools through the review gate.
          </div>
        </div>
        <div className="qe-cl__actions">
          <Button variant="secondary" onClick={onBrowsePools} iconLeft={<Icon name="layers" size={15} />}>
            Browse pools
          </Button>
          <Button variant="primary" onClick={onNew} iconLeft={<Icon name="plus" size={15} />}>
            New campaign
          </Button>
        </div>
      </div>

      {error && (
        <Callout variant="danger" title="Could not load campaigns">
          {error}
        </Callout>
      )}

      <Card>
        {runs == null && !error && <div className="qe-cl__empty">Loading campaigns…</div>}
        {runs != null && runs.length === 0 && (
          <div className="qe-cl__empty">No evolve campaigns yet. Start one with “New campaign”.</div>
        )}
        {runs != null && runs.length > 0 && (
          <DataTable columns={columns} rows={runs} keyField="id" onRowClick={(row) => onOpen(row.id)} />
        )}
      </Card>
    </div>
  );
}
