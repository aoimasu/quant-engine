import { useEffect, useState } from 'react';
import { listRuns, UnauthorizedError, type RunListItem, type RunListQuery } from './runs';
import { DEFAULT_POLL_MS } from './usePollingRun';

/** Consecutive poll failures tolerated before giving up with a fatal error (resilience). */
const MAX_POLL_FAILURES = 4;

export interface UseRunListPollingOptions extends RunListQuery {
  /** Poll cadence while any row is active (ms). Overridable for tests; defaults to {@link DEFAULT_POLL_MS}. */
  pollMs?: number;
}

export interface RunListState {
  /** The latest page's slim rows, or `null` before the first successful fetch. */
  runs: RunListItem[] | null;
  /** The cursor for the next (older) page, or `null` on the last page / before the first fetch. */
  nextCursor: string | null;
  /** A fatal error surfaced after a transient fetch error persists past the retry cap. */
  error: string | null;
}

/** Whether any row is still queued/running — i.e. the list should keep refreshing. */
function anyActive(runs: RunListItem[]): boolean {
  return runs.some((r) => r.status === 'queued' || r.status === 'running');
}

/**
 * useRunListPolling (QE-410) — polls `GET /api/runs` (the slim page) on mount and, **while any row is
 * queued/running**, every `pollMs`; it stops once every row is terminal, so an idle list makes no
 * further requests. Overlapping requests are guarded (a poll is skipped while one is in flight) and
 * transient errors are retried up to a bounded streak (keeping the last good rows on screen) before a
 * fatal error is surfaced. Shared by `BacktestsList` and `TrainingList`.
 */
export function useRunListPolling(options: UseRunListPollingOptions = {}): RunListState {
  const { pollMs = DEFAULT_POLL_MS, type, status, limit, cursor } = options;
  const [runs, setRuns] = useState<RunListItem[] | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setRuns(null);
    setNextCursor(null);
    setError(null);

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let inFlight = false; // overlapping-request guard
    let failures = 0;

    const tick = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const page = await listRuns({ type, status, limit, cursor });
        if (cancelled) return;
        setRuns(page.runs);
        setNextCursor(page.nextCursor);
        setError(null);
        failures = 0;
        // Keep polling only while something is still in flight; stop when all rows are terminal.
        if (anyActive(page.runs)) timer = setTimeout(tick, pollMs);
      } catch (e) {
        if (cancelled) return;
        // Terminal-auth (QE-409): a 401 stops the list poll immediately without consuming the retry
        // budget or surfacing a fatal error. The API client already emitted the `unauthorized` signal
        // that flips the shell back to Login.
        if (e instanceof UnauthorizedError) return;
        failures += 1;
        if (failures > MAX_POLL_FAILURES) {
          setError(e instanceof Error ? e.message : 'Failed to load runs.');
          return; // give up — keep the last good rows on screen
        }
        timer = setTimeout(tick, pollMs); // transient — retry without clearing the list
      } finally {
        inFlight = false;
      }
    };

    void tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [type, status, limit, cursor, pollMs]);

  return { runs, nextCursor, error };
}
