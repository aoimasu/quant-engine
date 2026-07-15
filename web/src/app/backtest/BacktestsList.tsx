import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon } from '../../design';
import type { Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { listRuns, type RunMeta, type RunStatus } from '../../api/runs';

const CSS = `
.qe-list { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-list__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-list__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-list__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-list__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
`;

injectCss('qe-list-css', CSS);

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

export interface BacktestsListProps {
  onOpen: (id: string) => void;
  onNew: () => void;
}

/** Backtests list — the runs table from `GET /api/runs`. Row click opens the result. */
export function BacktestsList({ onOpen, onNew }: BacktestsListProps) {
  const [runs, setRuns] = useState<RunMeta[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listRuns()
      .then((r) => {
        if (!cancelled) setRuns(r);
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
      key: 'params',
      header: 'Vintage',
      // `vintage` is backtest-only — narrow on the discriminated `type` before reading it (a train run
      // in the list has no vintage).
      render: (_v, row) => (
        <span style={{ fontWeight: 600 }}>
          {row.type === 'backtest' ? row.params.vintage || '—' : '—'}
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
      render: (v) => (
        <span style={{ color: 'var(--text-tertiary)' }}>{fmtDate(Number(v))}</span>
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
            rows={runs as (RunMeta & Record<string, unknown>)[]}
            keyField="id"
            onRowClick={(row) => onOpen(row.id)}
          />
        )}
      </Card>
    </div>
  );
}
