import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { usePollingRun } from './usePollingRun';
import { onUnauthorized } from './authEvents';
import type { RunMeta } from './runs';

function meta(status: RunMeta['status'], pct = 0, error: string | null = null): RunMeta {
  return {
    id: 'run-1',
    type: 'backtest',
    status,
    params: {},
    progress: { pct, stage: 'simulate', msg: '' },
    train: undefined,
    created_ms: 1,
    started_ms: status === 'queued' ? null : 2,
    finished_ms: status === 'succeeded' || status === 'failed' ? 3 : null,
    exit: status === 'succeeded' ? 0 : null,
    error,
    artifacts: [],
  } as unknown as RunMeta;
}

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

/** A tiny probe that renders the hook's state so assertions can read it from the DOM. */
function Probe({ pollMs }: { pollMs: number }) {
  const { meta: m, error, retrying } = usePollingRun('run-1', { pollMs, failedFallback: 'fallback failure' });
  return (
    <div>
      <span data-testid="status">{m?.status ?? 'none'}</span>
      <span data-testid="pct">{m?.progress.pct ?? -1}</span>
      <span data-testid="error">{error ?? ''}</span>
      <span data-testid="retrying">{retrying ? 'yes' : 'no'}</span>
    </div>
  );
}

describe('usePollingRun', () => {
  afterEach(() => vi.restoreAllMocks());

  it('polls while running, advances progress, then stops on the terminal succeeded status', async () => {
    let phase: 'run-30' | 'run-80' | 'done' = 'run-30';
    const fetchMock = vi.fn(async () =>
      json(phase === 'run-30' ? meta('running', 30) : phase === 'run-80' ? meta('running', 80) : meta('succeeded', 100)),
    );
    vi.stubGlobal('fetch', fetchMock);

    render(<Probe pollMs={15} />);

    await waitFor(() => expect(screen.getByTestId('pct').textContent).toBe('30'));
    phase = 'run-80';
    await waitFor(() => expect(screen.getByTestId('pct').textContent).toBe('80'));
    phase = 'done';
    await waitFor(() => expect(screen.getByTestId('status').textContent).toBe('succeeded'));

    // Terminal — polling stops (no further fetches).
    const calls = fetchMock.mock.calls.length;
    await new Promise((r) => setTimeout(r, 60));
    expect(fetchMock.mock.calls.length).toBe(calls);
  });

  it('sets the failed reason from meta.error and stops (terminal)', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => json(meta('failed', 20, 'boom'))));
    render(<Probe pollMs={15} />);
    await waitFor(() => expect(screen.getByTestId('error').textContent).toBe('boom'));
    expect(screen.getByTestId('status').textContent).toBe('failed');
  });

  it('falls back to failedFallback when a failed run carries no error', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => json(meta('failed', 20, null))));
    render(<Probe pollMs={15} />);
    await waitFor(() => expect(screen.getByTestId('error').textContent).toBe('fallback failure'));
  });

  it('retries transient errors (non-fatal) then recovers without a fatal error', async () => {
    let phase: 'error' | 'done' = 'error';
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => (phase === 'error' ? new Response(null, { status: 500 }) : json(meta('succeeded', 100)))),
    );
    render(<Probe pollMs={15} />);

    await waitFor(() => expect(screen.getByTestId('retrying').textContent).toBe('yes'));
    expect(screen.getByTestId('error').textContent).toBe(''); // not fatal yet

    phase = 'done';
    await waitFor(() => expect(screen.getByTestId('status').textContent).toBe('succeeded'));
    expect(screen.getByTestId('retrying').textContent).toBe('no');
  });

  it('gives up with a fatal error after the retry cap is exceeded', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
    render(<Probe pollMs={10} />);
    await waitFor(() => expect(screen.getByTestId('error').textContent).not.toBe(''), { timeout: 2000 });
  });

  it('treats a 401 as terminal-auth: stops immediately, no retry, no fatal error, fires unauthorized (QE-409)', async () => {
    const fetchMock = vi.fn(async () => json({ error: 'authentication required' }, 401));
    vi.stubGlobal('fetch', fetchMock);
    const seen = vi.fn();
    const off = onUnauthorized(seen);

    render(<Probe pollMs={10} />);

    // The app-level unauthorized signal fires (the shell flips to Login elsewhere).
    await waitFor(() => expect(seen).toHaveBeenCalled());

    // Terminal-auth: exactly one fetch (no retry budget consumed), no "retrying", no fatal error.
    await new Promise((r) => setTimeout(r, 60));
    expect(fetchMock.mock.calls.length).toBe(1);
    expect(screen.getByTestId('retrying').textContent).toBe('no');
    expect(screen.getByTestId('error').textContent).toBe('');
    expect(screen.getByTestId('status').textContent).toBe('none');
    off();
  });
});
