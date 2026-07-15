import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BacktestsList } from './BacktestsList';
import type { BacktestRunMeta, RunMeta, TrainRunMeta } from '../../api/runs';

function meta(over: Partial<BacktestRunMeta>): RunMeta {
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

/** A `type:"train"` run — the kind that must NOT leak into the Backtests table (QE-408). */
function trainMeta(over: Partial<TrainRunMeta>): RunMeta {
  return {
    id: 'train-zzzzzzzz-9',
    type: 'train',
    status: 'succeeded',
    params: {
      start: '2019-06-06',
      end: '2019-09-09',
      resolution: '4h',
    },
    progress: { pct: 100, stage: 'done', msg: 'Sealed' },
    created_ms: 1_700_000_003_000,
    started_ms: 1_700_000_004_000,
    finished_ms: 1_700_000_005_000,
    exit: 0,
    error: null,
    artifacts: ['result.json'],
    ...over,
  };
}

// Match on the `/api/runs` pathname (query-agnostic) so the `?type=backtest` the list now sends still
// resolves; the mock deliberately returns the SAME payload regardless of the query so a test can prove
// the client-side `isBacktestRun` filter (defense-in-depth), not just the server filter.
function mockRuns(runs: RunMeta[]) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (new URL(url, 'http://localhost').pathname === '/api/runs') {
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

  it('renders only backtest rows from a mixed payload; no training run is reachable (QE-408)', async () => {
    const onOpen = vi.fn();
    // A mixed `listRuns` response: one backtest, one training run. The training run must not leak.
    vi.stubGlobal(
      'fetch',
      mockRuns([
        meta({ id: 'bktst-11-aaaa', params: { ...meta({}).params, vintage: 'v-keeper' } as BacktestRunMeta['params'] }),
        trainMeta({ id: 'train-99-bbbb' }),
      ]),
    );
    render(<BacktestsList onOpen={onOpen} onNew={() => {}} />);

    // The backtest row renders…
    expect(await screen.findByText('v-keeper')).toBeInTheDocument();
    expect(screen.getByText('bktst-11')).toBeInTheDocument();

    // …and the training run is filtered out entirely — its id, its distinctive window, and its
    // resolution are all absent, so there is nothing to click through to a 409/404 result screen.
    expect(screen.queryByText('train-99')).not.toBeInTheDocument();
    expect(screen.queryByText(/2019-06-06 → 2019-09-09/)).not.toBeInTheDocument();

    // Exactly one data row is rendered (one <tbody> row).
    const bodyRows = document.querySelectorAll('tbody tr');
    expect(bodyRows.length).toBe(1);

    // Clicking the only row opens the backtest — the training run is never reachable.
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
});
