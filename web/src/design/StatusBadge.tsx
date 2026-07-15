import { Badge, type BadgeVariant } from './Badge';
import type { RunStatus } from '../api/runs';

/** Map a run lifecycle status to a {@link Badge} variant. */
const VARIANT: Record<RunStatus, BadgeVariant> = {
  succeeded: 'up',
  running: 'info',
  queued: 'neutral',
  failed: 'down',
};

export interface StatusBadgeProps {
  /** The run's lifecycle status. */
  status: RunStatus;
  /**
   * Completion percent to fold into a `running` label as `RUNNING {pct}%` (the run lists). Omit to
   * render the plain status label `RUNNING` (the detail screens).
   */
  pct?: number;
}

/**
 * StatusBadge (QE-410) — the one shared run-status pill. Promoted from the near-identical `statusBadge`
 * helpers in `BacktestResult`/`TrainingMonitor` and the `statusVariant` + inline `RUNNING {pct}%` badge
 * in `BacktestsList`/`TrainingList`, so all four screens render status identically.
 */
export function StatusBadge({ status, pct }: StatusBadgeProps) {
  const label = status === 'running' && pct != null ? `RUNNING ${pct}%` : status.toUpperCase();
  return (
    <Badge variant={VARIANT[status]} dot>
      {label}
    </Badge>
  );
}
