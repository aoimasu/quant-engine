import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { TrainingMonitor } from './TrainingMonitor';
import type { RunMeta } from '../../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const BASE = {
  id: 'train-1',
  type: 'train',
  params: { start: '2021-01-01', end: '2021-02-01', resolution: '1h' },
  created_ms: 1_700_000_000_000,
  started_ms: 1_700_000_001_000,
  exit: null,
  error: null,
} as const;

const RUNNING: RunMeta = {
  ...BASE,
  status: 'running',
  progress: { pct: 70, stage: 'search', msg: 'generation 1/2' },
  train: {
    generation: {
      generation: 1,
      generations: 2,
      coverage: 3,
      coverage_long: 2,
      coverage_short: 1,
      best_fitness: 1.23,
    },
  },
  finished_ms: null,
  artifacts: [],
} as unknown as RunMeta;

const SUCCEEDED: RunMeta = {
  ...BASE,
  status: 'succeeded',
  progress: { pct: 85, stage: 'gate', msg: 'G1 passed' },
  train: {
    generation: {
      generation: 2,
      generations: 2,
      coverage: 5,
      coverage_long: 3,
      coverage_short: 2,
      best_fitness: 2.5,
    },
    ensemble: { folds: 4, members: 3, score: 0.42 },
    gate: {
      promoted: true,
      failed: [],
      in_sample_sharpe: 1.5,
      holdout_sharpe: 1.1,
      dsr: 0.8,
      spa_pvalue: 0.03,
      n_trials: 12,
    },
    vintage: 'vintage-abc123',
  },
  exit: 0,
  finished_ms: 1_700_000_002_000,
  artifacts: ['result.json'],
} as unknown as RunMeta;

describe('TrainingMonitor', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders live generations/coverage/folds/best-so-far, then the G1 verdict + vintage link', async () => {
    const onBacktestVintage = vi.fn();
    // Deterministic running→succeeded flip (no timer race), like the QE-259 BacktestResult test.
    let phase: 'running' | 'done' = 'running';
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/train-1')) return json(phase === 'running' ? RUNNING : SUCCEEDED);
        return new Response(null, { status: 404 });
      }),
    );

    const { container } = render(
      <TrainingMonitor runId="train-1" onBack={() => {}} onBacktestVintage={onBacktestVintage} pollMs={20} />,
    );

    // While running: the generation counter, the archive-coverage grid, and best-so-far render live.
    expect(await screen.findByText('1/2')).toBeInTheDocument();
    expect(screen.getByText('1.230')).toBeInTheDocument(); // best-so-far fitness
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '70');
    // Long (2) + Short (1) occupied cells are drawn from the coverage grid.
    expect(container.querySelectorAll('.qe-cov__cell--long').length).toBe(2);
    expect(container.querySelectorAll('.qe-cov__cell--short').length).toBe(1);

    // Flip to succeeded: the G1 verdict, CV folds, and the sealed-vintage link appear.
    phase = 'done';
    expect(await screen.findByText('G1 PASS')).toBeInTheDocument();
    expect(screen.getByText('1.50')).toBeInTheDocument(); // in-sample Sharpe
    // CV folds surfaced from the ensemble snapshot.
    const folds = screen.getByText('CV folds').parentElement!;
    expect(folds).toHaveTextContent('4');

    // The deep-link carries the sealed vintage id into the backtest flow.
    const link = await screen.findByRole('button', { name: /backtest this vintage/i });
    await userEvent.click(link);
    expect(onBacktestVintage).toHaveBeenCalledWith('vintage-abc123');
  });

  it('surfaces a failed run with its error message', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/train-1')) {
          return json({
            ...BASE,
            status: 'failed',
            progress: { pct: 20, stage: 'features', msg: '' },
            error: 'training window too short',
            finished_ms: 1_700_000_002_000,
            artifacts: [],
          });
        }
        return new Response(null, { status: 404 });
      }),
    );

    render(<TrainingMonitor runId="train-1" onBack={() => {}} onBacktestVintage={() => {}} pollMs={20} />);

    expect(await screen.findByText(/training window too short/i)).toBeInTheDocument();
  });
});
