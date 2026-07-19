import { useEffect, useState } from 'react';
import { Badge, Button, Callout, Card, Icon, RunProgress, StatusBadge } from '../../design';
import { injectCss } from '../../design/injectCss';
import { usePollingRun } from '../../api/usePollingRun';
import { ApiError, getVintage, type FlowProgress, type VintageDetail } from '../../api/runs';
import { NotPaperConfirmedCallout, RegimeComposition } from '../strategies/VintageInspector';

const CSS = `
.qe-fm { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-fm__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-fm__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-fm__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-fm__phases { display: flex; flex-direction: column; gap: 10px; }
.qe-fm__phase { display: flex; align-items: center; gap: 12px; padding: 12px 14px; border: 1px solid var(--border-subtle); border-radius: var(--radius-md); }
.qe-fm__phase .ic { display: inline-flex; }
.qe-fm__phase .tx { display: flex; flex-direction: column; gap: 2px; }
.qe-fm__phase .nm { font: 600 13px var(--font-sans); color: var(--text-primary); }
.qe-fm__phase .st { font: 500 11px var(--font-mono); color: var(--text-tertiary); }
.qe-fm__phase--active { border-color: var(--violet-400); background: var(--accent-fill-soft); }
.qe-fm__phase--done { border-color: var(--up-500, #16a34a); }
.qe-fm__lineage { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.qe-fm__lineage .row { display: flex; flex-direction: column; gap: 2px; }
.qe-fm__lineage .row .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-fm__lineage .row .v { font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; }
.qe-fm__back { margin-bottom: 4px; }
`;

injectCss('qe-fm-css', CSS);

type PhaseState = 'pending' | 'active' | 'done' | 'skipped';

const PHASE_ICON: Record<PhaseState, string> = {
  pending: 'circle',
  active: 'activity',
  done: 'check',
  skipped: 'ban',
};

const PHASE_LABEL: Record<PhaseState, string> = {
  pending: 'pending',
  active: 'in progress',
  done: 'complete',
  skipped: 'skipped',
};

/**
 * Derive the two-phase progression from the flow supervision record + run status (design §5.2):
 * - **train**: active once `train_run` is set (and not yet sealed); done once `vintage` is set (sealed) or the
 *   backtest phase has started; on a failed run with no vintage it stays active-then-failed.
 * - **backtest**: pending until `backtest_run` is set; active while set and the run is still running; done on
 *   a succeeded run. `skipped` when the run failed at train with no vintage (no backtest ⇒ no vintage).
 */
function phases(
  flow: FlowProgress | undefined,
  status: 'queued' | 'running' | 'succeeded' | 'failed',
): { train: PhaseState; backtest: PhaseState } {
  const sealed = Boolean(flow?.vintage);
  const backtestStarted = Boolean(flow?.backtest_run);
  const trainStarted = Boolean(flow?.train_run);

  if (status === 'succeeded') return { train: 'done', backtest: 'done' };
  if (status === 'failed') {
    // Failed before sealing a vintage ⇒ the backtest phase never ran.
    if (!sealed) return { train: trainStarted ? 'active' : 'pending', backtest: 'skipped' };
    return { train: 'done', backtest: backtestStarted ? 'active' : 'skipped' };
  }
  // queued / running
  const train: PhaseState = sealed || backtestStarted ? 'done' : trainStarted ? 'active' : 'pending';
  const backtest: PhaseState = backtestStarted ? 'active' : 'pending';
  return { train, backtest };
}

function PhaseRow({ name, detail, state }: { name: string; detail: string; state: PhaseState }) {
  const cls =
    state === 'active' ? ' qe-fm__phase--active' : state === 'done' ? ' qe-fm__phase--done' : '';
  return (
    <div className={`qe-fm__phase${cls}`}>
      <span className="ic">
        <Icon name={PHASE_ICON[state]} size={18} />
      </span>
      <span className="tx">
        <span className="nm">{name}</span>
        <span className="st">
          {PHASE_LABEL[state]}
          {detail ? ` · ${detail}` : ''}
        </span>
      </span>
    </div>
  );
}

export interface FlowMonitorProps {
  runId: string;
  /** Go back to the runs list. */
  onBack: () => void;
  /** Deep-link into the Vintage Inspector for the sealed vintage id (QE-457). */
  onInspectVintage: (vintage: string) => void;
  /** Poll cadence while queued/running (ms). Overridable for tests; the hook default applies otherwise. */
  pollMs?: number;
}

/**
 * Composite-flow monitor (QE-462) — the single run row / single status view for a `type:"flow"` run
 * (QE-460). Polls `GET /api/runs/:id` via {@link usePollingRun} and renders the **train → backtest per-phase
 * progression** derived from `meta.flow` (`train_run`/`vintage`/`backtest_run`) + the coarse progress bar. On
 * success it loads the sealed vintage (`GET /api/vintages/{id}`) and mirrors the QE-457 Inspector: the
 * **holdout / regime chips** and the standing **"backtest-holdout only — not paper-confirmed"** label, so the
 * flow verdict reads as a backtest-holdout evaluation still owing G2/G3 — plus a link into the Inspector.
 */
export function FlowMonitor({ runId, onBack, onInspectVintage, pollMs }: FlowMonitorProps) {
  const { meta, error, retrying } = usePollingRun(runId, {
    pollMs,
    failedFallback: 'The flow run failed.',
  });

  const flow = meta?.type === 'flow' ? meta.flow : undefined;
  const status = meta?.status ?? 'queued';
  const running = meta != null && (status === 'running' || status === 'queued');
  const done = status === 'succeeded';
  const vintage = flow?.vintage;

  // On a succeeded flow, load the sealed vintage so the holdout/regime chips + not-paper-confirmed label
  // mirror the Inspector verbatim (the regime composition lives only on the persisted vintage, QE-467/456).
  const [vintageDetail, setVintageDetail] = useState<VintageDetail | null>(null);
  const [vintageError, setVintageError] = useState<string | null>(null);
  useEffect(() => {
    if (!done || !vintage) return;
    let cancelled = false;
    getVintage(vintage)
      .then((d) => {
        if (!cancelled) {
          setVintageDetail(d);
          setVintageError(null);
        }
      })
      .catch((e) => {
        if (!cancelled) setVintageError(e instanceof ApiError ? e.message : 'Failed to load the vintage.');
      });
    return () => {
      cancelled = true;
    };
  }, [done, vintage]);

  const { train, backtest } = phases(flow, status);

  const back = (
    <div className="qe-fm__back">
      <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
        All training runs
      </Button>
    </div>
  );

  const holdoutWindow =
    flow?.holdout_start && flow?.holdout_end ? `${flow.holdout_start} → ${flow.holdout_end}` : '—';

  return (
    <div className="qe-fm">
      {back}

      <div className="qe-fm__hd">
        <div className="qe-fm__title">
          <h2>Flow run</h2>
          {meta && <StatusBadge status={meta.status} />}
          <Badge variant="neutral">TRAIN → BACKTEST</Badge>
        </div>
        {done && vintage && (
          <Button
            variant="primary"
            onClick={() => onInspectVintage(vintage)}
            iconLeft={<Icon name="git-branch" size={15} />}
          >
            Open in Vintage Inspector
          </Button>
        )}
      </div>

      {retrying && !error && (
        <Callout variant="warn" title="Connection issue">
          Couldn’t reach the server — retrying…
        </Callout>
      )}

      {error && (
        <Callout variant="danger" title="Flow failed">
          {error}
        </Callout>
      )}

      {running && meta && <RunProgress status={meta.status} progress={meta.progress} />}

      <Card title="Phases">
        <div className="qe-fm__phases">
          <PhaseRow
            name="Train phase"
            state={train}
            detail={flow?.train_run ? `run ${flow.train_run.slice(0, 8)}` : ''}
          />
          <PhaseRow
            name="Backtest phase (frozen holdout)"
            state={backtest}
            detail={flow?.backtest_run ? `run ${flow.backtest_run.slice(0, 8)}` : ''}
          />
        </div>
      </Card>

      <Card title="Frozen holdout handoff">
        <div className="qe-fm__lineage">
          {(
            [
              ['Sealed vintage', vintage ?? '—'],
              ['Holdout window', holdoutWindow],
              ['Train sub-run', flow?.train_run ?? '—'],
              ['Backtest sub-run', flow?.backtest_run ?? '—'],
            ] as const
          ).map(([k, v]) => (
            <div className="row" key={k}>
              <span className="k">{k}</span>
              <span className="v" title={v}>
                {v}
              </span>
            </div>
          ))}
        </div>
      </Card>

      {done && (
        <>
          <NotPaperConfirmedCallout />
          <Card title="Holdout regime composition">
            {vintageError ? (
              <Callout variant="danger" title="Could not load the sealed vintage">
                {vintageError}
              </Callout>
            ) : vintageDetail ? (
              <RegimeComposition regimes={vintageDetail.regime_composition} />
            ) : (
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>Loading the sealed vintage…</div>
            )}
          </Card>
        </>
      )}
    </div>
  );
}
