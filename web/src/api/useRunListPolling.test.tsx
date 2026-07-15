import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { useRunListPolling } from './useRunListPolling';
import type { UseRunListPollingOptions } from './useRunListPolling';
import { onUnauthorized } from './authEvents';
import type { RunListItem } from './runs';

function item(over: Partial<RunListItem>): RunListItem {
  return {
    id: 'run-1',
    type: 'backtest',
    label: 'v',
    status: 'running',
    progress: { pct: 0, stage: 'simulate', msg: '' },
    created_ms: 1,
    ...over,
  };
}

function page(rows: RunListItem[], nextCursor: string | null = null) {
  return new Response(JSON.stringify({ runs: rows, next_cursor: nextCursor }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

function Probe(opts: UseRunListPollingOptions) {
  const { runs, nextCursor, error } = useRunListPolling(opts);
  return (
    <div>
      <span data-testid="rows">{runs ? runs.map((r) => `${r.id}:${r.status}:${r.progress.pct}`).join('|') : 'null'}</span>
      <span data-testid="cursor">{nextCursor ?? ''}</span>
      <span data-testid="error">{error ?? ''}</span>
    </div>
  );
}

describe('useRunListPolling', () => {
  afterEach(() => vi.restoreAllMocks());

  it('consumes the slim page envelope (rows + next cursor)', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => page([item({ id: 'a', status: 'succeeded', progress: { pct: 100, stage: 'report', msg: '' } })], 'a')));
    render(<Probe pollMs={15} />);
    await waitFor(() => expect(screen.getByTestId('rows').textContent).toBe('a:succeeded:100'));
    expect(screen.getByTestId('cursor').textContent).toBe('a');
  });

  it('sends type/status/limit/cursor as query params', async () => {
    const fetchMock = vi.fn(async () => page([]));
    vi.stubGlobal('fetch', fetchMock);
    render(<Probe type="train" status="running" limit={10} cursor="cur-9" pollMs={999} />);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [first] = fetchMock.mock.calls[0] as unknown as [RequestInfo | URL];
    const url = new URL(typeof first === 'string' ? first : first.toString(), 'http://localhost');
    expect(url.pathname).toBe('/api/runs');
    expect(url.searchParams.get('type')).toBe('train');
    expect(url.searchParams.get('status')).toBe('running');
    expect(url.searchParams.get('limit')).toBe('10');
    expect(url.searchParams.get('cursor')).toBe('cur-9');
  });

  it('keeps polling while a row is active then stops once all rows are terminal', async () => {
    let phase: 'active' | 'done' = 'active';
    const fetchMock = vi.fn(async () =>
      page([item({ id: 'x', status: phase === 'active' ? 'running' : 'succeeded', progress: { pct: phase === 'active' ? 50 : 100, stage: 's', msg: '' } })]),
    );
    vi.stubGlobal('fetch', fetchMock);
    render(<Probe pollMs={15} />);

    await waitFor(() => expect(screen.getByTestId('rows').textContent).toBe('x:running:50'));
    phase = 'done';
    await waitFor(() => expect(screen.getByTestId('rows').textContent).toBe('x:succeeded:100'));

    const calls = fetchMock.mock.calls.length;
    await new Promise((r) => setTimeout(r, 60));
    expect(fetchMock.mock.calls.length).toBe(calls); // stopped
  });

  it('makes no second request when the first page is already all-terminal', async () => {
    const fetchMock = vi.fn(async () => page([item({ id: 'z', status: 'succeeded', progress: { pct: 100, stage: 'report', msg: '' } })]));
    vi.stubGlobal('fetch', fetchMock);
    render(<Probe pollMs={15} />);
    await waitFor(() => expect(screen.getByTestId('rows').textContent).toBe('z:succeeded:100'));
    await new Promise((r) => setTimeout(r, 60));
    expect(fetchMock.mock.calls.length).toBe(1);
  });

  it('treats a 401 as terminal-auth: stops, no retry, no fatal error, fires unauthorized (QE-409)', async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ error: 'authentication required' }), { status: 401, headers: { 'Content-Type': 'application/json' } }));
    vi.stubGlobal('fetch', fetchMock);
    const seen = vi.fn();
    const off = onUnauthorized(seen);

    render(<Probe pollMs={10} />);
    await waitFor(() => expect(seen).toHaveBeenCalled());

    await new Promise((r) => setTimeout(r, 60));
    expect(fetchMock.mock.calls.length).toBe(1); // no retry budget burned
    expect(screen.getByTestId('error').textContent).toBe(''); // no fatal "failed" surface
    expect(screen.getByTestId('rows').textContent).toBe('null');
    off();
  });
});
