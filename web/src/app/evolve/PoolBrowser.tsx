import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, DataTable, Icon } from '../../design';
import type { BadgeVariant, Column } from '../../design';
import { injectCss } from '../../design/injectCss';
import { ApiError, listFormulaPools, type PoolLifecycleState, type PoolSummary } from '../../api/runs';

const CSS = `
.qe-pb { max-width: var(--content-max); margin: 0 auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.qe-pb__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.qe-pb__hd h2 { font-family: var(--font-display); font-size: var(--fs-lg); font-weight: 600; }
.qe-pb__sub { font-size: var(--fs-sm); color: var(--text-tertiary); margin-top: 2px; }
.qe-pb__empty { padding: 40px 24px; text-align: center; color: var(--text-tertiary); font-size: var(--fs-sm); }
.qe-pb__back { margin-bottom: 4px; }
`;

injectCss('qe-pb-css', CSS);

/** Lifecycle → badge variant (neutral resting state; danger only for genuine terminals). */
const LIFECYCLE_BADGE: Record<PoolLifecycleState, BadgeVariant> = {
  draft: 'neutral',
  approved: 'info',
  sealed: 'up',
  rejected: 'down',
  revoked: 'down',
};

/** A pool's lifecycle rendered as a badge. */
export function LifecycleBadge({ state }: { state: PoolLifecycleState }) {
  return <Badge variant={LIFECYCLE_BADGE[state]}>{state.toUpperCase()}</Badge>;
}

export interface PoolBrowserProps {
  onBack: () => void;
  onOpen: (poolId: string) => void;
}

/**
 * PoolBrowser (QE-453 screen 5) — the read-only browse table of frozen formula pools from
 * `GET /api/formula-pools`, mirroring {@link TrainingList}. Each row shows mode, lifecycle state, formula
 * count, and the GP-aware trial basis; a row-click opens the {@link PoolReview} governance gate. Pools are
 * human-paced (not live), so this fetches once on mount rather than polling.
 */
export function PoolBrowser({ onBack, onOpen }: PoolBrowserProps) {
  const [pools, setPools] = useState<PoolSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    listFormulaPools()
      .then((p) => {
        if (!cancelled) setPools(p);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : 'Failed to load formula pools.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const columns: Column<PoolSummary>[] = [
    {
      key: 'id',
      header: 'Pool',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }}>
          {String(v).slice(0, 12)}
        </span>
      ),
    },
    {
      key: 'mode',
      header: 'Mode',
      render: (_v, row) => (
        <Badge variant={row.mode === 'production' ? 'warn' : 'neutral'}>{row.mode.toUpperCase()}</Badge>
      ),
    },
    {
      key: 'lifecycle',
      header: 'Lifecycle',
      render: (_v, row) => <LifecycleBadge state={row.lifecycle} />,
    },
    {
      key: 'formula_count',
      header: 'Formulas',
      align: 'num',
      render: (v) => <span style={{ fontFamily: 'var(--font-mono)' }}>{String(v)}</span>,
    },
    {
      key: 'gp_aware',
      header: 'GP-aware',
      render: (_v, row) =>
        row.gp_aware ? (
          <Badge variant="up">YES</Badge>
        ) : (
          <Badge variant="warn">FLOOR</Badge>
        ),
    },
    {
      key: 'pool_hash',
      header: 'Pool hash',
      render: (v) => (
        <span style={{ fontFamily: 'var(--font-mono)', color: 'var(--text-tertiary)' }} title={String(v)}>
          {String(v).slice(0, 12)}…
        </span>
      ),
    },
  ];

  return (
    <div className="qe-pb">
      <div className="qe-pb__back">
        <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
          All campaigns
        </Button>
      </div>

      <div className="qe-pb__hd">
        <div>
          <h2>Formula pools</h2>
          <div className="qe-pb__sub">
            Browse frozen &amp; historical pools with their governance lifecycle; open one to review its
            formulas, deflation basis, and lineage.
          </div>
        </div>
      </div>

      {error && (
        <Callout variant="danger" title="Could not load pools">
          {error}
        </Callout>
      )}

      <Card>
        {pools == null && !error && <div className="qe-pb__empty">Loading pools…</div>}
        {pools != null && pools.length === 0 && (
          <div className="qe-pb__empty">No formula pools yet. Launch an evolve campaign to produce one.</div>
        )}
        {pools != null && pools.length > 0 && (
          <DataTable columns={columns} rows={pools} keyField="id" onRowClick={(row) => onOpen(row.id)} />
        )}
      </Card>
    </div>
  );
}
