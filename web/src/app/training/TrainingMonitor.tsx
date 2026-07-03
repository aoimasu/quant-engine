import { useEffect, useRef, useState } from 'react';
import { Badge, Button, Callout, Card, Icon } from '../../design';
import { injectCss } from '../../design/injectCss';
import { getRun, type RunMeta } from '../../api/runs';

/* Composed from the ported design tokens/primitives (CSP-safe: injectCss, no runtime CDN). */
const CSS = `
.qe-tm { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-tm__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-tm__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-tm__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-tm__grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; align-items: start; }
.qe-tm__stat { display: flex; flex-direction: column; gap: 4px; }
.qe-tm__stat .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .08em; color: var(--text-muted); }
.qe-tm__stat .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 22px; font-weight: 600; color: var(--text-primary); }
.qe-tm__stats { display: grid; grid-template-columns: repeat(2, 1fr); gap: 14px; }
.qe-run { display: flex; flex-direction: column; gap: 10px; }
.qe-run__bar { height: 6px; background: var(--surface-inset); border-radius: var(--radius-full); overflow: hidden; }
.qe-run__fill { height: 100%; background: var(--accent); transition: width 0.15s linear; }
.qe-cov { display: flex; flex-direction: column; gap: 10px; }
.qe-cov__row { display: flex; flex-direction: column; gap: 6px; }
.qe-cov__lbl { display: flex; align-items: center; justify-content: space-between; font: 500 11px var(--font-mono); color: var(--text-tertiary); }
.qe-cov__grid { display: grid; grid-template-columns: repeat(20, 1fr); gap: 3px; }
.qe-cov__cell { aspect-ratio: 1; border-radius: var(--radius-xs); background: var(--surface-inset); }
.qe-cov__cell--long { background: var(--violet-400); }
.qe-cov__cell--short { background: var(--cyan-400, #22d3ee); }
.qe-gate { display: flex; flex-direction: column; gap: 12px; }
.qe-gate__crit { display: flex; flex-direction: column; gap: 6px; }
.qe-gate__crit .row { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--text-secondary); }
.qe-metrics2 { display: grid; grid-template-columns: repeat(2, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-metrics2 .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-metrics2 .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-metrics2 .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 15px; font-weight: 600; color: var(--text-primary); }
.qe-tm__back { margin-bottom: 4px; }
`;

injectCss('qe-tm-css', CSS);

/** Poll cadence while a run is queued/running (ms). */
const POLL_MS = 2000;
/** Consecutive poll failures tolerated before giving up with a fatal error (resilience). */
const MAX_POLL_FAILURES = 4;
/** Max coverage cells drawn per direction in the archive grid (visual cap; a `+N` note covers overflow). */
const MAX_CELLS = 60;

const MINUS = '−';

function fmt(v: number | null | undefined, digits = 3): string {
  if (v == null || !Number.isFinite(v)) return '—';
  return v < 0 ? `${MINUS}${Math.abs(v).toFixed(digits)}` : v.toFixed(digits);
}

function statusBadge(status: RunMeta['status']) {
  switch (status) {
    case 'succeeded':
      return (
        <Badge variant="up" dot>
          SUCCEEDED
        </Badge>
      );
    case 'running':
      return (
        <Badge variant="info" dot>
          RUNNING
        </Badge>
      );
    case 'queued':
      return (
        <Badge variant="neutral" dot>
          QUEUED
        </Badge>
      );
    case 'failed':
      return (
        <Badge variant="down" dot>
          FAILED
        </Badge>
      );
  }
}

/** One direction's occupied-cell strip of the MAP-Elites archive-coverage grid. */
function CoverageRow({ label, count, kind }: { label: string; count: number; kind: 'long' | 'short' }) {
  const shown = Math.min(count, MAX_CELLS);
  const overflow = count - shown;
  return (
    <div className="qe-cov__row">
      <div className="qe-cov__lbl">
        <span>{label}</span>
        <span>
          {count} cell{count === 1 ? '' : 's'}
          {overflow > 0 ? ` (+${overflow})` : ''}
        </span>
      </div>
      <div className="qe-cov__grid" aria-label={`${label} occupied cells`}>
        {Array.from({ length: shown }, (_, i) => (
          <div key={i} className={`qe-cov__cell qe-cov__cell--${kind}`} />
        ))}
        {shown === 0 && <div className="qe-cov__cell" />}
      </div>
    </div>
  );
}

export interface TrainingMonitorProps {
  runId: string;
  /** Go back to the training-runs list. */
  onBack: () => void;
  /** Deep-link into the New-backtest flow for the sealed vintage id. */
  onBacktestVintage: (vintage: string) => void;
  /** Poll cadence while queued/running (ms). Overridable for tests; defaults to {@link POLL_MS}. */
  pollMs?: number;
}

/**
 * Training monitor — data-driven from `GET /api/runs/:id` (QE-261). Resiliently polls the run while it
 * is queued/running (bounded retry on transient failures, streak reset on a clean tick) and renders the
 * QE-260 rich progress: generations + progress bar, the MAP-Elites archive-coverage grid, CV folds,
 * best-so-far fitness, and the G1 gate verdict. On completion it shows the verdict and links to the
 * produced vintage's backtest.
 */
export function TrainingMonitor({ runId, onBack, onBacktestVintage, pollMs = POLL_MS }: TrainingMonitorProps) {
  const [meta, setMeta] = useState<RunMeta | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  const terminal = useRef(false);

  useEffect(() => {
    terminal.current = false;
    setMeta(null);
    setError(null);
    setRetrying(false);

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let failures = 0;

    const tick = async () => {
      try {
        const m = await getRun(runId);
        if (cancelled) return;
        setMeta(m);
        failures = 0;
        setRetrying(false);
        if (m.status === 'succeeded') {
          terminal.current = true;
          return; // terminal — stop polling
        }
        if (m.status === 'failed') {
          terminal.current = true;
          setError(m.error ?? 'The training run failed.');
          return; // terminal
        }
        timer = setTimeout(tick, pollMs); // queued | running — keep polling
      } catch (e) {
        if (cancelled) return;
        failures += 1;
        if (failures > MAX_POLL_FAILURES) {
          setError(e instanceof Error ? e.message : 'Failed to load the training run.');
          return;
        }
        setRetrying(true);
        timer = setTimeout(tick, pollMs);
      }
    };

    void tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [runId, pollMs]);

  const running = meta != null && (meta.status === 'running' || meta.status === 'queued');
  const train = meta?.train;
  const gen = train?.generation;
  const ensemble = train?.ensemble;
  const gate = train?.gate;
  const vintage = train?.vintage;
  const done = meta?.status === 'succeeded';

  const back = (
    <div className="qe-tm__back">
      <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
        All training runs
      </Button>
    </div>
  );

  return (
    <div className="qe-tm">
      {back}

      <div className="qe-tm__hd">
        <div className="qe-tm__title">
          <h2>Training run</h2>
          {meta && statusBadge(meta.status)}
          {gate && (
            <Badge variant={gate.promoted ? 'up' : 'down'}>{gate.promoted ? 'G1 PASS' : 'G1 FAIL'}</Badge>
          )}
        </div>
        {done && vintage && (
          <Button
            variant="primary"
            onClick={() => onBacktestVintage(vintage)}
            iconLeft={<Icon name="flask-conical" size={15} />}
          >
            Backtest this vintage
          </Button>
        )}
      </div>

      {retrying && !error && (
        <Callout variant="warn" title="Connection issue">
          Couldn’t reach the server — retrying…
        </Callout>
      )}

      {error && (
        <Callout variant="danger" title="Training failed">
          {error}
        </Callout>
      )}

      {running && meta && (
        <Card>
          <div className="qe-run">
            <div
              style={{
                display: 'flex',
                justifyContent: 'space-between',
                fontFamily: 'var(--font-mono)',
                fontSize: 12,
                color: 'var(--text-secondary)',
              }}
            >
              <span>{meta.progress.msg || `${meta.status}…`}</span>
              <span>{`${meta.progress.pct}%`}</span>
            </div>
            <div className="qe-run__bar">
              <div
                className="qe-run__fill"
                role="progressbar"
                aria-valuenow={meta.progress.pct}
                aria-valuemin={0}
                aria-valuemax={100}
                style={{ width: `${meta.progress.pct}%` }}
              />
            </div>
          </div>
        </Card>
      )}

      <div className="qe-tm__grid">
        <Card title="Evolution">
          <div className="qe-tm__stats">
            <div className="qe-tm__stat">
              <span className="k">Generation</span>
              <span className="v">{gen ? `${gen.generation}/${gen.generations}` : '—'}</span>
            </div>
            <div className="qe-tm__stat">
              <span className="k">Best-so-far fitness</span>
              <span className="v">{fmt(gen?.best_fitness)}</span>
            </div>
          </div>
        </Card>

        <Card title="Cross-validation">
          <div className="qe-tm__stats">
            <div className="qe-tm__stat">
              <span className="k">CV folds</span>
              <span className="v">{ensemble ? ensemble.folds : '—'}</span>
            </div>
            <div className="qe-tm__stat">
              <span className="k">Ensemble members</span>
              <span className="v">{ensemble ? ensemble.members : '—'}</span>
            </div>
          </div>
        </Card>
      </div>

      <Card title="MAP-Elites archive coverage">
        <div className="qe-cov">
          <CoverageRow label="Long" count={gen?.coverage_long ?? 0} kind="long" />
          <CoverageRow label="Short" count={gen?.coverage_short ?? 0} kind="short" />
          <div className="qe-cov__lbl">
            <span>Total occupied cells</span>
            <span>{gen?.coverage ?? 0}</span>
          </div>
        </div>
      </Card>

      <Card title="G1 gate">
        {gate ? (
          <div className="qe-gate">
            <div className="qe-gate__crit">
              <div className="row">
                <Badge variant={gate.promoted ? 'up' : 'down'} dot>
                  {gate.promoted ? 'PROMOTED' : 'BLOCKED'}
                </Badge>
                <span>
                  {gate.promoted
                    ? 'The vintage cleared every G1 criterion.'
                    : `Failed criteria: ${gate.failed.length ? gate.failed.join(', ') : '—'}`}
                </span>
              </div>
            </div>
            <div className="qe-metrics2">
              {(
                [
                  ['In-sample Sharpe', fmt(gate.in_sample_sharpe, 2)],
                  ['Holdout Sharpe', fmt(gate.holdout_sharpe, 2)],
                  ['DSR', fmt(gate.dsr, 3)],
                  ['SPA p-value', fmt(gate.spa_pvalue, 3)],
                  ['Trials', String(gate.n_trials)],
                  ['Ensemble score', fmt(ensemble?.score, 3)],
                ] as const
              ).map(([k, v]) => (
                <div className="m" key={k}>
                  <span className="k">{k}</span>
                  <span className="v" style={v.startsWith(MINUS) ? { color: 'var(--down-500)' } : undefined}>
                    {v}
                  </span>
                </div>
              ))}
            </div>
          </div>
        ) : (
          <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
            The G1 verdict appears once the search, ensemble, and validation stages complete.
          </div>
        )}
      </Card>
    </div>
  );
}
