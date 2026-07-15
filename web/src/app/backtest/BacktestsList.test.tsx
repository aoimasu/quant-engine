import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BacktestsList } from './BacktestsList';
import type { RunListItem } from '../../api/runs';

/** A slim `type:"backtest"` list row (the QE-410 projection `GET /api/runs` returns). */
function item(over: Partial<RunListItem>): RunListItem {
  return {
    id: 'run-aaaaaaaa-1',
    type: 'backtest',
    label: 'v-2024-q4',
    status: 'succeeded',
    progress: { pct: 100, stage: 'report', msg: 'Scoring' },
    created_ms: 1_700_000_000_000,
    ...over,
  };
}

/** A slim `type:"train"` row — the kind that must NOT leak into the Backtests table (QE-408). */
function trainItem(over: Partial<RunListItem>): RunListItem {
  return {
    id: 'train-zzzzzzzz-9',
    type: 'train',
    label: 'train 2019-06-06→2019-09-09',
    status: 'succeeded',
    progress: { pct: 100, stage: 'done', msg: 'Sealed' },
    created_ms: 1_700_000_003_000,
    ...over,
  };
}

/** A `GET /api/runs` page envelope from a set of slim rows (query-agnostic path match). */
function page(rows: RunListItem[], nextCursor: string | null = null) {
  return new Response(JSON.stringify({ runs: rows, next_cursor: nextCursor }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

function mockRuns(rows: RunListItem[]) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (new URL(url, 'http://localhost').pathname === '/api/runs') return page(rows);
    return new Response(null, { status: 404 });
  });
}

describe('BacktestsList', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders rows from the slim GET /api/runs page (vintage label, live status/percent)', async () => {
    vi.stubGlobal(
      'fetch',
      mockRuns([
        item({}),
        item({ id: 'run-bbbb', status: 'running', progress: { pct: 42, stage: 'simulate', msg: '…' } }),
      ]),
    );
    render(<BacktestsList onOpen={() => {}} onNew={() => {}} />);

    expect((await screen.findAllByText('v-2024-q4')).length).toBe(2);
    expect(screen.getByText('SUCCEEDED')).toBeInTheDocument();
    expect(screen.getByText('RUNNING 42%')).toBeInTheDocument();
  });

  it('opens a run on row click', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal('fetch', mockRuns([item({ id: 'run-xyz' })]));
    render(<BacktestsList onOpen={onOpen} onNew={() => {}} />);
    await userEvent.click(await screen.findByText('v-2024-q4'));
    expect(onOpen).toHaveBeenCalledWith('run-xyz');
  });

  it('opens a run from the list via keyboard alone — Enter and Space (QE-422)', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal('fetch', mockRuns([item({ id: 'run-xyz' })]));
    render(<BacktestsList onOpen={onOpen} onNew={() => {}} />);
    await screen.findByText('v-2024-q4');

    // The clickable row is a focusable button; keyboard alone opens the run.
    const row = screen.getByRole('button', { name: /run-xyz/i });
    row.focus();
    expect(row).toHaveFocus();

    await userEvent.keyboard('{Enter}');
    expect(onOpen).toHaveBeenCalledWith('run-xyz');

    onOpen.mockClear();
    row.focus();
    await userEvent.keyboard(' ');
    expect(onOpen).toHaveBeenCalledWith('run-xyz');
  });

  it('renders only backtest rows from a mixed payload; no training run is reachable (QE-408)', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockRuns([
        item({ id: 'bktst-11-aaaa', label: 'v-keeper' }),
        trainItem({ id: 'train-99-bbbb' }),
      ]),
    );
    render(<BacktestsList onOpen={onOpen} onNew={() => {}} />);

    // The backtest row renders…
    expect(await screen.findByText('v-keeper')).toBeInTheDocument();
    expect(screen.getByText('bktst-11')).toBeInTheDocument();

    // …and the training run is filtered out entirely — its id and distinctive label are absent.
    expect(screen.queryByText('train-99')).not.toBeInTheDocument();
    expect(screen.queryByText(/2019-06-06→2019-09-09/)).not.toBeInTheDocument();

    // Exactly one data row is rendered.
    expect(document.querySelectorAll('tbody tr').length).toBe(1);

    await userEvent.click(screen.getByText('v-keeper'));
    expect(onOpen).toHaveBeenCalledExactlyOnceWith('bktst-11-aaaa');
    expect(onOpen).not.toHaveBeenCalledWith('train-99-bbbb');
  });

  it('fires onNew from the "New backtest" action', async () => {
    const onNew = vi.fn();
    vi.stubGlobal('fetch', mockRuns([]));
    render(<BacktestsList onOpen={() => {}} onNew={onNew} />);
    await waitFor(() => expect(screen.getByText(/no backtests yet/i)).toBeInTheDocument());
    await userEvent.click(screen.getByRole('button', { name: /new backtest/i }));
    expect(onNew).toHaveBeenCalledOnce();
  });

  it('live-refreshes a running row and stops polling once it is terminal (QE-410)', async () => {
    // The list polls `GET /api/runs` while any row is queued/running: the running row's percent
    // advances on the next poll, and once it reaches a terminal status the list stops fetching.
    let phase: 'running-42' | 'running-73' | 'done' = 'running-42';
    const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input.toString();
      if (new URL(url, 'http://localhost').pathname !== '/api/runs') {
        return new Response(null, { status: 404 });
      }
      const pct = phase === 'running-42' ? 42 : phase === 'running-73' ? 73 : 100;
      const status = phase === 'done' ? 'succeeded' : 'running';
      return page([
        item({ id: 'run-live', status, progress: { pct, stage: 'simulate', msg: '…' } }),
      ]);
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<BacktestsList onOpen={() => {}} onNew={() => {}} pollMs={15} />);

    // First poll: 42% while running.
    expect(await screen.findByText('RUNNING 42%')).toBeInTheDocument();

    // Next poll picks up the advanced percent WITHOUT any navigation.
    phase = 'running-73';
    expect(await screen.findByText('RUNNING 73%')).toBeInTheDocument();

    // Terminal: the row settles and polling stops (no further fetches after it is drawn).
    phase = 'done';
    expect(await screen.findByText('SUCCEEDED')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText(/RUNNING/)).not.toBeInTheDocument());

    const callsAfterTerminal = fetchMock.mock.calls.length;
    await new Promise((r) => setTimeout(r, 60)); // ~4 poll intervals
    expect(fetchMock.mock.calls.length).toBe(callsAfterTerminal);
  });
});
