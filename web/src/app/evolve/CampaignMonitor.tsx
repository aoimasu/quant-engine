import { useCallback, useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, Icon, RunProgress, StatusBadge } from '../../design';
import { injectCss } from '../../design/injectCss';
import { usePollingRun } from '../../api/usePollingRun';
import { ApiError, getRunArchive, haltRun, type EvolveArchive } from '../../api/runs';

const CSS = `
.qe-cm { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-cm__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-cm__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-cm__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-cm__mode { padding: 10px 14px; border-radius: var(--radius-md); font: 500 12px var(--font-mono); border: 1px solid var(--border-subtle); }
.qe-cm__mode--sandbox { background: var(--surface-inset); color: var(--text-secondary); }
.qe-cm__mode--production { background: var(--warn-fill-soft, rgba(180,120,0,.14)); color: var(--warn-500, #d08700); border-color: var(--warn-500, #d08700); }
.qe-cm__heat { display: grid; grid-template-columns: repeat(auto-fill, minmax(64px, 1fr)); gap: 6px; }
.qe-cm__cell { aspect-ratio: 1; border-radius: var(--radius-sm); background: var(--surface-inset); border: 1px solid var(--border-subtle); display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; padding: 4px; }
.qe-cm__cell--filled { background: var(--accent-fill-soft); border-color: var(--violet-400); }
.qe-cm__cell .cx { font: 600 12px var(--font-mono); color: var(--violet-200); font-variant-numeric: tabular-nums; }
.qe-cm__cell .cl { font: 500 8px var(--font-mono); text-transform: uppercase; letter-spacing: .04em; color: var(--text-muted); text-align: center; }
.qe-cm__bars { display: grid; grid-template-columns: repeat(2, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-cm__bars .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-cm__bars .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-cm__bars .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 15px; font-weight: 600; color: var(--text-primary); }
.qe-cm__back { margin-bottom: 4px; }
`;

injectCss('qe-cm-css', CSS);

/** Occupied-cell cap drawn in the heatmap (a visual bound; a `+N` note covers overflow). */
const MAX_CELLS = 90;

function fmt(v: number | null | undefined, digits = 2): string {
  if (v == null || !Number.isFinite(v)) return '—';
  return v.toFixed(digits);
}

export interface CampaignMonitorProps {
  runId: string;
  onBack: () => void;
  /** Poll cadence while queued/running (ms). Overridable for tests. */
  pollMs?: number;
}

/**
 * Campaign monitor (QE-453 screen 2) — mirrors {@link TrainingMonitor}. Run status/progress come from the
 * shared {@link usePollingRun} (bounded-retry, terminal-stop); the MAP-Elites archive comes from a separate
 * `GET /api/runs/{id}/archive` fetched on mount and refreshed while the run is live. Renders the
 * **ArchiveHeatmap** (family × timescale × complexity cells), the **TrialCountBar** (distinct-canonical `N`
 * vs the analytic floor vs the finite `E[maxSharpe]` bar — amber on the QE-439 "blind floor" tell), a
 * persistent **mode banner**, and an authz'd **Halt** control.
 */
export function CampaignMonitor({ runId, onBack, pollMs }: CampaignMonitorProps) {
  const { meta, error, retrying } = usePollingRun(runId, {
    pollMs,
    failedFallback: 'The evolve campaign failed.',
  });

  const running = meta != null && (meta.status === 'running' || meta.status === 'queued');
  const evolve = meta?.type === 'evolve' ? meta.params : undefined;

  const [archive, setArchive] = useState<EvolveArchive | null>(null);
  const [halting, setHalting] = useState(false);
  const [haltError, setHaltError] = useState<string | null>(null);

  // Fetch the archive snapshot; a 404 (no archive yet) is not an error — the campaign just hasn't written
  // one. Re-fetched by the poll effect while the run is live.
  const loadArchive = useCallback(async () => {
    try {
      setArchive(await getRunArchive(runId));
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return; // no archive yet
      // Other errors are non-fatal for the monitor — the run poller owns the fatal surface.
    }
  }, [runId]);

  useEffect(() => {
    void loadArchive();
    if (!running) return;
    const ms = pollMs ?? 2000;
    const t = setInterval(() => void loadArchive(), ms);
    return () => clearInterval(t);
  }, [loadArchive, running, pollMs]);

  const doHalt = async () => {
    setHaltError(null);
    setHalting(true);
    try {
      await haltRun(runId);
      // The run poller picks up the terminal state on its next tick.
    } catch (e) {
      setHaltError(e instanceof ApiError ? e.message : 'Failed to halt the campaign.');
    } finally {
      setHalting(false);
    }
  };

  const mode = evolve?.mode ?? archive?.mode ?? 'sandbox';
  const basis = archive?.trial_basis;
  // The QE-439 "blind floor" tell: the distinct-canonical count never exceeded the analytic floor.
  const blindFloor = basis != null && basis.distinct_evaluations <= basis.analytic_floor;
  const cells = archive?.cells ?? [];
  const shown = cells.slice(0, MAX_CELLS);
  const overflow = cells.length - shown.length;

  return (
    <div className="qe-cm">
      <div className="qe-cm__back">
        <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
          All campaigns
        </Button>
      </div>

      <div className="qe-cm__hd">
        <div className="qe-cm__title">
          <h2>Evolve campaign</h2>
          {meta && <StatusBadge status={meta.status} />}
          <Badge variant={mode === 'production' ? 'warn' : 'neutral'}>{mode.toUpperCase()}</Badge>
        </div>
        {running && (
          <Button
            variant="danger"
            loading={halting}
            onClick={doHalt}
            iconLeft={<Icon name="octagon-x" size={15} />}
          >
            Halt campaign
          </Button>
        )}
      </div>

      <div
        className={`qe-cm__mode qe-cm__mode--${mode === 'production' ? 'production' : 'sandbox'}`}
        role="note"
      >
        {mode === 'production'
          ? 'PRODUCTION — a sealed pool here is on the production governance path (seal itself is gated on QE-454).'
          : 'RESEARCH (sandbox) — this campaign cannot reach a production vintage.'}
      </div>

      {retrying && !error && (
        <Callout variant="warn" title="Connection issue">
          Couldn’t reach the server — retrying…
        </Callout>
      )}
      {error && (
        <Callout variant="danger" title="Campaign failed">
          {error}
        </Callout>
      )}
      {haltError && (
        <Callout variant="danger" title="Halt failed">
          {haltError}
        </Callout>
      )}

      {running && meta && <RunProgress status={meta.status} progress={meta.progress} />}

      <Card title="MAP-Elites archive">
        {cells.length === 0 ? (
          <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
            The archive heatmap appears once the campaign has illuminated its first niches.
          </div>
        ) : (
          <div className="qe-cm__heat" aria-label="archive heatmap">
            {shown.map((c, i) => (
              <div
                key={`${c.family}/${c.timescale}/${c.complexity}/${i}`}
                className="qe-cm__cell qe-cm__cell--filled"
                title={`${c.family} · ${c.timescale} · ${c.complexity} — ${c.node_count} nodes, fitness ${fmt(c.best_fitness, 3)}`}
              >
                <span className="cx">{fmt(c.best_fitness, 2)}</span>
                <span className="cl">{c.family}</span>
                <span className="cl">
                  {c.timescale}/{c.complexity}
                </span>
              </div>
            ))}
            {overflow > 0 && (
              <div className="qe-cm__cell" title="additional occupied niches">
                <span className="cx">+{overflow}</span>
              </div>
            )}
          </div>
        )}
      </Card>

      <Card title="Trial-count basis (deflation)">
        {basis ? (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
            {blindFloor && (
              <Callout variant="warn" title="Trial basis is the blind analytic floor">
                The distinct-canonical count ({basis.distinct_evaluations}) does not exceed the analytic
                floor ({basis.analytic_floor}) — the GP-aware trial counter (QE-439) is not driving the
                basis. A later production seal blocks on this.
              </Callout>
            )}
            <div className="qe-cm__bars">
              {(
                [
                  ['Distinct-canonical N', String(basis.distinct_evaluations)],
                  ['Analytic floor', String(basis.analytic_floor)],
                  ['Trial basis N', String(basis.n_trials)],
                  ['E[max Sharpe] bar', fmt(basis.expected_max_sharpe, 3)],
                  ['Occupied cells', `${basis.occupied_cells}/${basis.total_cells}`],
                  ['Generations × offspring', archive ? `${archive.generations} × ${archive.offspring}` : '—'],
                ] as const
              ).map(([k, v]) => (
                <div className="m" key={k}>
                  <span className="k">{k}</span>
                  <span className="v">{v}</span>
                </div>
              ))}
            </div>
          </div>
        ) : (
          <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
            The trial-count basis appears once the archive snapshot is written.
          </div>
        )}
      </Card>
    </div>
  );
}
