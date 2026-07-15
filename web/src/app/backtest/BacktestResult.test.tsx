import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BacktestResult } from './BacktestResult';
import type { BacktestParams, BacktestResult as ResultContract, RunMeta } from '../../api/runs';

const PARAMS: BacktestParams = {
  vintage: 'v-2024-q4',
  start: '2021-01-01',
  end: '2024-12-31',
  resolution: '1h',
  universe: ['BTC-PERP', 'ETH-PERP', 'SOL-PERP'],
  taker_fee_bps: 2,
  slippage_model: 'square-root-impact',
};

function meta(status: RunMeta['status'], pct = 100, msg = 'Scoring'): RunMeta {
  return {
    id: 'run-1',
    type: 'backtest',
    status,
    params: PARAMS,
    progress: { pct, stage: status === 'running' ? 'simulate' : 'report', msg },
    created_ms: 1_700_000_000_000,
    started_ms: status === 'queued' ? null : 1_700_000_001_000,
    finished_ms: status === 'succeeded' || status === 'failed' ? 1_700_000_002_000 : null,
    exit: status === 'succeeded' ? 0 : null,
    error: null,
    artifacts: status === 'succeeded' ? ['result.json'] : [],
  };
}

const RESULT: ResultContract = {
  strategy: {
    name: 'Momentum v3',
    status: 'sealed',
    tags: ['crypto', 'perp'],
    params: { lookback: '48h', z_entry: 1.8, max_pos: 6 },
  },
  window: { start: '2021-01-01', end: '2024-12-31', resolution: '1h' },
  universe: { symbols: ['BTC-PERP', 'ETH-PERP', 'SOL-PERP'], count: 3 },
  costs: { taker_fee_bps: 2, slippage_model: 'square-root-impact' },
  metrics: { cagr: 0.412, sharpe: 2.14, sortino: 3.08, max_dd: -0.083, win_rate: 0.582, profit_factor: 1.94 },
  equity_curve: [100, 101, 103, 106, 110, 108, 112],
  drawdown: [0, -0.5, -1.2, -0.3, 0, -0.8, 0],
  monthly_returns: [{ year: 2021, months: [1, 2, -3, 4, 5, -1, 2, 3, 4, 5, 6, -2] }],
  trades: [
    {
      id: '#2041',
      symbol: 'BTC-PERP',
      side: 'LONG',
      entry: '61,204',
      exit: '63,180',
      hold: '4d 6h',
      return_pct: 3.23,
      result: 'WIN',
    },
  ],
};

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

describe('BacktestResult', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders the full §8.1 contract for a succeeded run', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/run-1/result')) return json(RESULT);
        if (url.endsWith('/api/runs/run-1')) return json(meta('succeeded'));
        return new Response(null, { status: 404 });
      }),
    );

    const { container } = render(
      <BacktestResult runId="run-1" onBack={() => {}} onReRun={() => {}} />,
    );

    // Header + strategy name.
    expect(await screen.findByRole('heading', { name: 'Momentum v3' })).toBeInTheDocument();

    // 6-metric strip.
    expect(screen.getByText('+41.2%')).toBeInTheDocument(); // CAGR
    expect(screen.getByText('2.14')).toBeInTheDocument(); // Sharpe
    expect(screen.getByText('3.08')).toBeInTheDocument(); // Sortino
    expect(screen.getByText('−8.3%')).toBeInTheDocument(); // Max DD (typographic minus)
    expect(screen.getByText('58.2%')).toBeInTheDocument(); // Win rate
    expect(screen.getByText('1.94')).toBeInTheDocument(); // Profit factor

    // Equity + drawdown area charts (inline SVG — no chart lib).
    expect(container.querySelectorAll('svg.qe-chart2').length).toBe(2);

    // Monthly-returns heatmap cells.
    expect(container.querySelectorAll('.qe-heat__cell').length).toBe(12);

    // Trades table.
    await userEvent.click(screen.getByRole('tab', { name: /trades/i }));
    const table = within(screen.getByRole('table'));
    expect(table.getByText('BTC-PERP')).toBeInTheDocument();
    expect(table.getByText('WIN')).toBeInTheDocument();
  });

  it('renders genome params read-only (tags in the header, no editable inputs)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/run-1/result')) return json(RESULT);
        if (url.endsWith('/api/runs/run-1')) return json(meta('succeeded'));
        return new Response(null, { status: 404 });
      }),
    );

    render(<BacktestResult runId="run-1" onBack={() => {}} onReRun={() => {}} />);

    // The genome params render as read-only tags.
    const paramGroup = await screen.findByLabelText(/strategy parameters \(read-only\)/i);
    expect(within(paramGroup).getByText('lookback=48h')).toBeInTheDocument();
    expect(within(paramGroup).getByText('z_entry=1.8')).toBeInTheDocument();
    // …and there is no textbox/input to edit them (the group contains tags, not inputs).
    expect(within(paramGroup).queryByRole('textbox')).toBeNull();

    // The Config tab's eval params are read-only (disabled) inputs. ("Start" also appears in the
    // right-rail run-config — every occurrence must be read-only.)
    await userEvent.click(screen.getByRole('tab', { name: /config/i }));
    const starts = screen.getAllByLabelText('Start') as HTMLInputElement[];
    expect(starts.length).toBeGreaterThan(0);
    for (const el of starts) {
      expect(el).toBeDisabled();
      expect(el).toHaveAttribute('readonly');
    }
  });

  it('"Re-run" clones the run params into a new POST /api/runs', async () => {
    const onReRun = vi.fn();
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === 'string' ? input : input.toString();
      const method = (init?.method ?? 'GET').toUpperCase();
      if (url.endsWith('/api/runs') && method === 'POST') return json({ id: 'run-2' }, 201);
      if (url.endsWith('/api/runs/run-1/result')) return json(RESULT);
      if (url.endsWith('/api/runs/run-1')) return json(meta('succeeded'));
      return new Response(null, { status: 404 });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<BacktestResult runId="run-1" onBack={() => {}} onReRun={onReRun} />);
    await screen.findByRole('heading', { name: 'Momentum v3' });

    await userEvent.click(screen.getByRole('button', { name: /re-run backtest/i }));
    await waitFor(() => expect(onReRun).toHaveBeenCalledWith('run-2'));

    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.type).toBe('backtest');
    expect(body.params.vintage).toBe('v-2024-q4');
    expect(body.params.universe).toEqual(['BTC-PERP', 'ETH-PERP', 'SOL-PERP']);
  });

  it('shows the progress card while running, then swaps to the result on completion', async () => {
    // Deterministic: every poll returns `running` until the test flips `phase`, so the running state
    // is reliably observed before completion (no timer race). A short `pollMs` keeps the test fast.
    let phase: 'running' | 'done' = 'running';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/run-1/result')) return json(RESULT);
        if (url.endsWith('/api/runs/run-1')) {
          return phase === 'running'
            ? json(meta('running', 64, 'Simulating 2021-01-01 → 2024-12-31…'))
            : json(meta('succeeded'));
        }
        return new Response(null, { status: 404 });
      }),
    );

    render(<BacktestResult runId="run-1" onBack={() => {}} onReRun={() => {}} pollMs={20} />);

    // While running: the progress card is visible (polls keep it running), no metrics yet.
    expect(await screen.findByText('64%')).toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '64');
    expect(screen.queryByText('+41.2%')).not.toBeInTheDocument();

    // Flip to succeeded: the next poll fetches the result and swaps the card for the metrics strip.
    phase = 'done';
    expect(await screen.findByText('+41.2%')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole('progressbar')).not.toBeInTheDocument());
  });

  it('keeps polling through a transient fetch error and still reaches the result', async () => {
    // While `phase==='error'` every poll 500s → the non-fatal "retrying" note shows and polling
    // continues (never terminates). Flipping to `succeeded` lets the next poll recover and render.
    let phase: 'error' | 'done' = 'error';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/run-1/result')) return json(RESULT);
        if (url.endsWith('/api/runs/run-1')) {
          return phase === 'error' ? new Response(null, { status: 500 }) : json(meta('succeeded'));
        }
        return new Response(null, { status: 404 });
      }),
    );

    render(<BacktestResult runId="run-1" onBack={() => {}} onReRun={() => {}} pollMs={20} />);

    // The transient error surfaces as a non-fatal retry note (no fatal error banner).
    expect(await screen.findByText(/retrying/i)).toBeInTheDocument();
    expect(screen.queryByText(/backtest failed/i)).not.toBeInTheDocument();

    // Recovery: the run reports succeeded → the result renders and the retry note clears.
    phase = 'done';
    expect(await screen.findByText('+41.2%')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText(/retrying/i)).not.toBeInTheDocument());
  });
});
