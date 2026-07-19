import { useState } from 'react';
import { Badge, Button, Callout, Card, Icon, RunProgress, StatusBadge } from '../../design';
import { injectCss } from '../../design/injectCss';
import { usePollingRun } from '../../api/usePollingRun';
import { ApiError, haltRun, isIngestRun } from '../../api/runs';

const CSS = `
.qe-im { max-width: var(--content-max); margin: 0 auto; padding: 18px; display: flex; flex-direction: column; gap: 16px; }
.qe-im__hd { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 18px; background: var(--surface-card); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); }
.qe-im__title { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.qe-im__title h2 { font-size: 18px; font-family: var(--font-display); }
.qe-im__back { margin-bottom: 4px; }
.qe-im__sum { display: grid; grid-template-columns: repeat(2, 1fr); gap: 1px; background: var(--border-subtle); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }
.qe-im__sum .m { background: var(--surface-card); padding: 12px 14px; display: flex; flex-direction: column; gap: 3px; }
.qe-im__sum .m .k { font: 500 10px var(--font-mono); text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
.qe-im__sum .m .v { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 14px; font-weight: 600; color: var(--text-primary); word-break: break-word; }
`;

injectCss('qe-im-css', CSS);

export interface IngestMonitorProps {
  runId: string;
  onBack: () => void;
  /** Poll cadence while queued/running (ms). Overridable for tests. */
  pollMs?: number;
}

/**
 * Ingest run monitor (QE-465) — the standard run monitor for a `type:"ingest"` run. Status/progress come
 * from the shared {@link usePollingRun} (bounded-retry, terminal-stop) rendered by the coarse
 * `<RunProgress>` bar (the same standard progress every other run-kind shows), and a **Cancel ingest**
 * control that hits the run-type-agnostic halt path (`POST /api/runs/{id}/halt`) exactly like the evolve
 * campaign monitor.
 *
 * NOTE (flagged follow-up): fine-grained per-page / percentage progress for a long real ingest needs the
 * `HistoricalSource::fetch() → one window` seam to stream/page and emit incremental `progress` lines
 * (server/engine change, out of QE-465 scope). This monitor renders whatever `meta.progress` the server
 * provides — it gets finer automatically once that streaming seam lands. No fabricated percentage.
 */
export function IngestMonitor({ runId, onBack, pollMs }: IngestMonitorProps) {
  const { meta, error, retrying } = usePollingRun(runId, {
    pollMs,
    failedFallback: 'The ingest run failed.',
  });

  const running = meta != null && (meta.status === 'running' || meta.status === 'queued');
  const params = meta && isIngestRun(meta) ? meta.params : undefined;

  const [halting, setHalting] = useState(false);
  const [haltError, setHaltError] = useState<string | null>(null);

  const doHalt = async () => {
    setHaltError(null);
    setHalting(true);
    try {
      await haltRun(runId);
      // The run poller picks up the terminal state on its next tick.
    } catch (e) {
      setHaltError(e instanceof ApiError ? e.message : 'Failed to cancel the ingest run.');
    } finally {
      setHalting(false);
    }
  };

  const scope = params ? (params.fetch_all ? 'fetch-all' : params.instruments.join(', ') || '—') : '—';

  return (
    <div className="qe-im">
      <div className="qe-im__back">
        <Button variant="ghost" size="sm" onClick={onBack} iconLeft={<Icon name="arrow-left" size={15} />}>
          Market data
        </Button>
      </div>

      <div className="qe-im__hd">
        <div className="qe-im__title">
          <h2>Ingest run</h2>
          {meta && <StatusBadge status={meta.status} />}
          <Badge variant="up">REAL</Badge>
        </div>
        {running && (
          <Button
            variant="danger"
            loading={halting}
            onClick={doHalt}
            iconLeft={<Icon name="octagon-x" size={15} />}
          >
            Cancel ingest
          </Button>
        )}
      </div>

      {retrying && !error && (
        <Callout variant="warn" title="Connection issue">
          Couldn’t reach the server — retrying…
        </Callout>
      )}
      {error && (
        <Callout variant="danger" title="Ingest failed">
          {error}
        </Callout>
      )}
      {haltError && (
        <Callout variant="danger" title="Cancel failed">
          {haltError}
        </Callout>
      )}

      {meta && <RunProgress status={meta.status} progress={meta.progress} />}

      {params && (
        <Card title="Ingest request">
          <div className="qe-im__sum">
            <div className="m">
              <span className="k">Instruments</span>
              <span className="v">{scope}</span>
            </div>
            <div className="m">
              <span className="k">Resolution</span>
              <span className="v">{params.resolution}</span>
            </div>
            <div className="m">
              <span className="k">Window</span>
              <span className="v">
                {params.start} → {params.end}
              </span>
            </div>
            <div className="m">
              <span className="k">Provenance</span>
              <span className="v">{params.synthetic ? 'SYNTHETIC' : 'REAL'}</span>
            </div>
          </div>
        </Card>
      )}
    </div>
  );
}
