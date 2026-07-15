import { Button, Callout, Card, DataTable, Icon, StatusBadge, formatRunDate } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { useRunListPolling } from '../../api/useRunListPolling';
import type { RunListItem } from '../../api/runs';

const CSS = `
.qe-list { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-list__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-list__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-list__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-list__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-list-css', CSS);

export interface BacktestsListProps {
  onOpen: (id: string) => void;
  onNew: () => void;
  /** Live-poll cadence while any row is queued/running (ms). Overridable for tests. */
  pollMs?: number;
}

/**
 * Backtests list — the runs table from `GET /api/runs?type=backtest` (the slim QE-410 projection). It
 * polls live via {@link useRunListPolling} while any row is queued/running (so a `RUNNING {pct}%` cell
 * updates without navigating) and stops once every row is terminal. The heavy `params` are deferred to
 * the result screen; the vintage column reads the server's slim `label`.
 */
export function BacktestsList({ onOpen, onNew, pollMs }: BacktestsListProps) {
  // Server-side `?type=backtest` filter; still narrow client-side (defense-in-depth) so a leaked train
  // row can never be clickable and route to a permanently-409/404 result.
  const { runs: allRuns, error } = useRunListPolling({ type: 'backtest', pollMs });
  const runs = allRuns?.filter((r) => r.type === 'backtest') ?? null;

  const columns: Column<RunListItem & Record<string, unknown>>[] = [
    {
      key: 'id',
      header: 'Run',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}>
          {String(v).slice(0, 8)}
        </span>
      ),
    },
    {
      key: 'label',
      header: 'Vintage',
      render: (_v, row) => <span style={{ fontWeight: 600 }}>{row.label || '—'}</span>,
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
      render: (v) => (
        <span style={{ color: 'var(--text-tertiary)' }}>{formatRunDate(Number(v))}</span>
      ),
    },
  ];

  return (
    <div className="qe-list">
      <div className="qe-list__hd">
        <div>
          <h2>Backtests</h2>
          <div className="qe-list__sub">
            Trigger a backtest of a sealed vintage over a window, watch progress, and review results.
          </div>
        </div>
        <Button variant="primary" onClick={onNew} iconLeft={<Icon name="plus" size={15} />}>
          New backtest
        </Button>
      </div>

      {error && (
        <Callout variant="danger" title="Could not load runs">
          {error}
        </Callout>
      )}

      <Card>
        {runs == null && !error && <div className="qe-list__empty">Loading runs…</div>}
        {runs != null && runs.length === 0 && (
          <div className="qe-list__empty">No backtests yet. Start one with “New backtest”.</div>
        )}
        {runs != null && runs.length > 0 && (
          <DataTable
            columns={columns}
            rows={runs as (RunListItem & Record<string, unknown>)[]}
            keyField="id"
            onRowClick={(row) => onOpen(row.id)}
          />
        )}
      </Card>
    </div>
  );
}
