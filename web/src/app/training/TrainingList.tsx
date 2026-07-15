import { Badge, Button, Callout, Card, DataTable, Icon, StatusBadge, formatRunDate } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { useRunListPolling } from '../../api/useRunListPolling';
import type { RunListItem } from '../../api/runs';

const CSS = `
.qe-tl { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-tl__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-tl__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-tl__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-tl__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-tl-css', CSS);

export interface TrainingListProps {
  onOpen: (id: string) => void;
  onNew: () => void;
  /** Live-poll cadence while any row is queued/running (ms). Overridable for tests. */
  pollMs?: number;
}

/**
 * Training-runs list — the `type:"train"` rows from `GET /api/runs?type=train` (the slim QE-410
 * projection). Polls live via {@link useRunListPolling} while any row is queued/running (so the
 * `RUNNING {pct}%`, Generation, and G1 cells update without navigating) and stops once all rows are
 * terminal. The window column reads the server's slim `label`; Generation/G1 read the live `train`
 * progress; the heavy `params` are deferred to the monitor.
 */
export function TrainingList({ onOpen, onNew, pollMs }: TrainingListProps) {
  const { runs: allRuns, error } = useRunListPolling({ type: 'train', pollMs });
  const runs = allRuns?.filter((r) => r.type === 'train') ?? null;

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
      header: 'Window',
      render: (_v, row) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-secondary)' }}>
          {row.label || '—'}
        </span>
      ),
    },
    {
      key: 'gen',
      header: 'Generation',
      align: 'num',
      render: (_v, row) => {
        const g = row.train?.generation;
        return (
          <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-secondary)' }}>
            {g ? `${g.generation}/${g.generations}` : '—'}
          </span>
        );
      },
    },
    {
      key: 'g1',
      header: 'G1',
      render: (_v, row) => {
        const gate = row.train?.gate;
        if (!gate) return <span style={{ color: 'var(--text-muted)' }}>—</span>;
        return <Badge variant={gate.promoted ? 'up' : 'down'}>{gate.promoted ? 'PASS' : 'FAIL'}</Badge>;
      },
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
    <div className="qe-tl">
      <div className="qe-tl__hd">
        <div>
          <h2>Training</h2>
          <div className="qe-tl__sub">
            Trigger a training search over a window, watch generations and coverage, and monitor to a G1
            verdict.
          </div>
        </div>
        <Button variant="primary" onClick={onNew} iconLeft={<Icon name="plus" size={15} />}>
          New training run
        </Button>
      </div>

      {error && (
        <Callout variant="danger" title="Could not load runs">
          {error}
        </Callout>
      )}

      <Card>
        {runs == null && !error && <div className="qe-tl__empty">Loading runs…</div>}
        {runs != null && runs.length === 0 && (
          <div className="qe-tl__empty">No training runs yet. Start one with “New training run”.</div>
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
