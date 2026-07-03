import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { listRuns, type RunMeta, type RunStatus } from '../../api/runs';

const CSS = `
.qe-tl { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-tl__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-tl__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-tl__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-tl__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-tl-css', CSS);

function statusVariant(status: RunStatus): 'up' | 'info' | 'neutral' | 'down' {
  switch (status) {
    case 'succeeded':
      return 'up';
    case 'running':
      return 'info';
    case 'queued':
      return 'neutral';
    case 'failed':
      return 'down';
  }
}

function fmtDate(ms: number): string {
  const d = new Date(ms);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, '0');
  const day = String(d.getUTCDate()).padStart(2, '0');
  const hh = String(d.getUTCHours()).padStart(2, '0');
  const mm = String(d.getUTCMinutes()).padStart(2, '0');
  return `${y}-${m}-${day} ${hh}:${mm}`;
}

export interface TrainingListProps {
  onOpen: (id: string) => void;
  onNew: () => void;
}

/** Training-runs list — the `type:"train"` rows from `GET /api/runs`. Row click opens the monitor. */
export function TrainingList({ onOpen, onNew }: TrainingListProps) {
  const [runs, setRuns] = useState<RunMeta[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listRuns()
      .then((r) => {
        if (!cancelled) setRuns(r.filter((run) => run.type === 'train'));
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Failed to load runs.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const columns: Column<RunMeta & Record<string, unknown>>[] = [
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
      key: 'window',
      header: 'Window',
      render: (_v, row) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-secondary)' }}>
          {row.params.start} → {row.params.end}
        </span>
      ),
    },
    {
      key: 'resolution',
      header: 'Res',
      align: 'num',
      render: (_v, row) => row.params.resolution || '—',
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
      render: (_v, row) => (
        <Badge variant={statusVariant(row.status)} dot>
          {row.status === 'running' ? `RUNNING ${row.progress.pct}%` : row.status.toUpperCase()}
        </Badge>
      ),
    },
    {
      key: 'created_ms',
      header: 'Created',
      align: 'num',
      render: (v) => <span style={{ color: 'var(--text-tertiary)' }}>{fmtDate(Number(v))}</span>,
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
            rows={runs as (RunMeta & Record<string, unknown>)[]}
            keyField="id"
            onRowClick={(row) => onOpen(row.id)}
          />
        )}
      </Card>
    </div>
  );
}
