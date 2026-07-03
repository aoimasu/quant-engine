import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BacktestsList } from './BacktestsList';
import type { RunMeta } from '../../api/runs';

function meta(over: Partial<RunMeta>): RunMeta {
  return {
    id: 'run-aaaaaaaa-1',
    type: 'backtest',
    status: 'succeeded',
    params: {
      vintage: 'v-2024-q4',
      start: '2021-01-01',
      end: '2024-12-31',
      resolution: '1h',
      universe: ['BTCUSDT'],
      taker_fee_bps: 2,
      slippage_model: 'square-root-impact',
    },
    progress: { pct: 100, stage: 'report', msg: 'Scoring' },
    created_ms: 1_700_000_000_000,
    started_ms: 1_700_000_001_000,
    finished_ms: 1_700_000_002_000,
    exit: 0,
    error: null,
    artifacts: ['result.json'],
    ...over,
  };
}

function mockRuns(runs: RunMeta[]) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (url.endsWith('/api/runs')) {
      return new Response(JSON.stringify(runs), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    return new Response(null, { status: 404 });
  });
}

describe('BacktestsList', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders rows from GET /api/runs (vintage, window, status)', async () => {
    vi.stubGlobal('fetch', mockRuns([meta({}), meta({ id: 'run-bbbb', status: 'running', progress: { pct: 42, stage: 'simulate', msg: '…' } })]));
    render(<BacktestsList onOpen={() => {}} onNew={() => {}} />);

    expect((await screen.findAllByText('v-2024-q4')).length).toBe(2);
    expect(screen.getAllByText(/2021-01-01 → 2024-12-31/).length).toBe(2);
    expect(screen.getByText('SUCCEEDED')).toBeInTheDocument();
    expect(screen.getByText('RUNNING 42%')).toBeInTheDocument();
  });

  it('opens a run on row click', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal('fetch', mockRuns([meta({ id: 'run-xyz' })]));
    render(<BacktestsList onOpen={onOpen} onNew={() => {}} />);
    await userEvent.click(await screen.findByText('v-2024-q4'));
    expect(onOpen).toHaveBeenCalledWith('run-xyz');
  });

  it('fires onNew from the "New backtest" action', async () => {
    const onNew = vi.fn();
    vi.stubGlobal('fetch', mockRuns([]));
    render(<BacktestsList onOpen={() => {}} onNew={onNew} />);
    await waitFor(() => expect(screen.getByText(/no backtests yet/i)).toBeInTheDocument());
    await userEvent.click(screen.getByRole('button', { name: /new backtest/i }));
    expect(onNew).toHaveBeenCalledOnce();
  });
});
