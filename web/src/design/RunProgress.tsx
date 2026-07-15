import { Card } from './Card';
import { injectCss } from './injectCss';
import type { Progress, RunStatus } from '../api/runs';

/* The run progress-bar card, ported once from the duplicated `.qe-run` blocks in
   `BacktestResult`/`TrainingMonitor` (QE-410). */
const CSS = `
.qe-run { display: flex; flex-direction: column; gap: 10px; }
.qe-run__hd { display: flex; justify-content: space-between; font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); }
.qe-run__bar { height: 6px; background: var(--surface-inset); border-radius: var(--radius-full); overflow: hidden; }
.qe-run__fill { height: 100%; background: var(--accent); transition: width 0.15s linear; }
`;

injectCss('qe-run-css', CSS);

export interface RunProgressProps {
  /** The run's current status (drives the fallback label when `progress.msg` is empty). */
  status: RunStatus;
  /** The latest coarse progress (`pct`/`msg`). */
  progress: Progress;
}

/**
 * RunProgress (QE-410) — the one shared queued/running progress card: a message line, a percent, and an
 * accessible progress bar. Both detail screens render it identically while a run is queued/running.
 */
export function RunProgress({ status, progress }: RunProgressProps) {
  return (
    <Card>
      <div className="qe-run">
        <div className="qe-run__hd">
          <span>{progress.msg || `${status}…`}</span>
          <span>{`${progress.pct}%`}</span>
        </div>
        <div className="qe-run__bar">
          <div
            className="qe-run__fill"
            role="progressbar"
            aria-valuenow={progress.pct}
            aria-valuemin={0}
            aria-valuemax={100}
            style={{ width: `${progress.pct}%` }}
          />
        </div>
      </div>
    </Card>
  );
}
