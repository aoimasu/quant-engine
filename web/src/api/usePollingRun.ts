import { useEffect, useState } from 'react';
import { getRun, type RunMeta } from './runs';

/** Default poll cadence while a run is queued/running (ms). */
export const DEFAULT_POLL_MS = 2000;
/** Consecutive poll failures tolerated before giving up with a fatal error (resilience). */
const MAX_POLL_FAILURES = 4;

export interface UsePollingRunOptions {
  /** Poll cadence while queued/running (ms). Overridable for tests; defaults to {@link DEFAULT_POLL_MS}. */
  pollMs?: number;
  /** Message used when a run reaches `failed` with no `meta.error`. */
  failedFallback?: string;
}

export interface PollingRunState {
  /** The latest polled run meta, or `null` before the first successful poll. */
  meta: RunMeta | null;
  /** A fatal error: a `failed` run's reason, or a transient fetch error past the retry cap. */
  error: string | null;
  /** True while a transient fetch error is being retried (a non-fatal "connection issue" note). */
  retrying: boolean;
}

/**
 * usePollingRun (QE-410) — the one shared run-polling hook. Polls `GET /api/runs/:id` while the run is
 * queued/running and **stops** once it is terminal (succeeded/failed). Transient fetch errors are
 * retried up to a bounded streak (surfaced as {@link PollingRunState.retrying}); only past the cap does
 * it give up with a fatal {@link PollingRunState.error}. Requests never overlap: the next tick is
 * scheduled only after the previous one resolves (chained `setTimeout`, not an interval).
 *
 * Promoted from the near-verbatim polling `useEffect` duplicated in `BacktestResult` and
 * `TrainingMonitor`. Screen-specific follow-on work (e.g. fetching a succeeded backtest's result) stays
 * in the screen, keyed off `meta.status`.
 */
export function usePollingRun(runId: string, options: UsePollingRunOptions = {}): PollingRunState {
  const { pollMs = DEFAULT_POLL_MS, failedFallback = 'The run failed.' } = options;
  const [meta, setMeta] = useState<RunMeta | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);

  useEffect(() => {
    // Reset when the run (or cadence) changes.
    setMeta(null);
    setError(null);
    setRetrying(false);

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Consecutive poll failures; reset on a clean tick so only a sustained outage hits the cap.
    let failures = 0;

    const tick = async () => {
      try {
        const m = await getRun(runId);
        if (cancelled) return;
        setMeta(m);
        failures = 0;
        setRetrying(false);
        if (m.status === 'succeeded') return; // terminal — stop polling
        if (m.status === 'failed') {
          setError(m.error ?? failedFallback);
          return; // terminal
        }
        timer = setTimeout(tick, pollMs); // queued | running — keep polling
      } catch (e) {
        if (cancelled) return;
        // Resilience: a transient fetch error must not freeze the view — keep polling up to a bounded
        // streak, surfacing a non-fatal "retrying" note; only give up (fatal) past the cap.
        failures += 1;
        if (failures > MAX_POLL_FAILURES) {
          setError(e instanceof Error ? e.message : 'Failed to load the run.');
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
  }, [runId, pollMs, failedFallback]);

  return { meta, error, retrying };
}
