import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { CampaignMonitor } from './CampaignMonitor';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const RUNNING_META = {
  id: 'evolve-1',
  type: 'evolve',
  params: { seed: 7, mode: 'sandbox', start: '2021-01-01', end: '2021-02-01', resolution: '1h' },
  status: 'running',
  progress: { pct: 40, stage: 'illumination', msg: 'generation 2/5' },
  created_ms: 1_700_000_000_000,
  started_ms: 1_700_000_001_000,
  finished_ms: null,
  exit: null,
  error: null,
  artifacts: [],
};

const ARCHIVE = {
  pool_id: 'campaign-abc',
  mode: 'sandbox',
  generations: 5,
  offspring: 64,
  cells: [
    { family: 'trend', timescale: 'fast', complexity: 'simple', node_count: 3, best_fitness: 1.5 },
    { family: 'flow', timescale: 'slow', complexity: 'complex', node_count: 7, best_fitness: 0.8 },
  ],
  trial_basis: {
    distinct_evaluations: 200,
    n_trials: 200,
    analytic_floor: 90,
    expected_max_sharpe: 2.1,
    occupied_cells: 2,
    total_cells: 45,
  },
};

describe('CampaignMonitor', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders the archive heatmap + trial-count basis from GET /archive', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/evolve-1/archive')) return json(ARCHIVE);
        if (url.endsWith('/api/runs/evolve-1')) return json(RUNNING_META);
        return new Response(null, { status: 404 });
      }),
    );

    render(<CampaignMonitor runId="evolve-1" onBack={() => {}} pollMs={20} />);

    // The heatmap renders occupied niches (family labels visible in the cells).
    expect(await screen.findByLabelText('archive heatmap')).toBeInTheDocument();
    expect(await screen.findByText('trend')).toBeInTheDocument();
    expect(screen.getByText('flow')).toBeInTheDocument();
    // The trial-count basis surfaces the distinct-canonical N and the analytic floor together.
    expect(screen.getByText('90')).toBeInTheDocument(); // analytic floor
    // The mode banner marks a sandbox campaign as RESEARCH.
    expect(screen.getByRole('note')).toHaveTextContent(/research/i);
  });

  it('POSTs /halt when the operator halts a running campaign', async () => {
    const fetchMock = vi.fn<typeof fetch>(async (input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input.toString();
      if (url.endsWith('/api/runs/evolve-1/halt')) return json({ id: 'evolve-1', status: 'failed', halted: true });
      if (url.endsWith('/api/runs/evolve-1/archive')) return json(ARCHIVE);
      if (url.endsWith('/api/runs/evolve-1')) return json(RUNNING_META);
      return new Response(null, { status: 404 });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<CampaignMonitor runId="evolve-1" onBack={() => {}} pollMs={20} />);

    const halt = await screen.findByRole('button', { name: /halt campaign/i });
    await userEvent.click(halt);

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(
          ([input, init]) =>
            (typeof input === 'string' ? input : input.toString()).endsWith('/api/runs/evolve-1/halt') &&
            (init as RequestInit | undefined)?.method === 'POST',
        ),
      ).toBe(true),
    );
  });
});
