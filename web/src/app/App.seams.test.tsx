import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useState } from 'react';
import { render, screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from './App';
import { ErrorBoundary } from './ErrorBoundary';
import type { CoverageRow, RunListItem, RunMeta, VintageListItem } from '../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** A `GET /api/runs` slim page envelope (QE-410 `{ runs, next_cursor }`). */
function page(rows: RunListItem[], nextCursor: string | null = null) {
  return json({ runs: rows, next_cursor: nextCursor });
}

function pathOf(input: RequestInfo | URL): { path: string; params: URLSearchParams } {
  const raw = typeof input === 'string' ? input : input.toString();
  const url = new URL(raw, 'http://localhost');
  return { path: url.pathname, params: url.searchParams };
}

// ---------------------------------------------------------------------------
// Error boundary — genuinely new (no boundary existed anywhere before QE-424).
// ---------------------------------------------------------------------------

/** A child that throws while `bomb.armed`, else renders — lets a test flip it off then recover. */
function Boom({ bomb }: { bomb: { armed: boolean } }) {
  if (bomb.armed) throw new Error('kaboom');
  return <div>recovered content</div>;
}

describe('ErrorBoundary (QE-424)', () => {
  afterEach(() => vi.restoreAllMocks());

  it('shows a recoverable fallback on a render throw and the reset control recovers', async () => {
    // React logs caught boundary errors to console.error; silence it so the run output stays clean.
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const bomb = { armed: true };

    render(
      <ErrorBoundary>
        <Boom bomb={bomb} />
      </ErrorBoundary>,
    );

    // A thrown render error shows the fallback (an accessible alert), NOT a blank page.
    const alert = screen.getByRole('alert');
    expect(alert).toBeInTheDocument();
    expect(within(alert).getByText(/something went wrong/i)).toBeInTheDocument();
    expect(screen.queryByText(/recovered content/i)).not.toBeInTheDocument();

    // The fallback is recoverable, not a dead end: clear the fault and click "Try again".
    bomb.armed = false;
    await userEvent.click(screen.getByRole('button', { name: /try again/i }));

    expect(await screen.findByText(/recovered content/i)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    errSpy.mockRestore();
  });

  it('auto-resets when a resetKeys entry changes (the App auth-change recovery path)', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const bomb = { armed: true };

    function Harness() {
      const [key, setKey] = useState('user-a');
      return (
        <div>
          <button
            type="button"
            onClick={() => {
              bomb.armed = false; // the fault is gone (e.g. a re-auth cleared the bad state)
              setKey('user-b'); // …and the reset context changes
            }}
          >
            change identity
          </button>
          <ErrorBoundary resetKeys={[key]}>
            <Boom bomb={bomb} />
          </ErrorBoundary>
        </div>
      );
    }

    render(<Harness />);
    expect(screen.getByRole('alert')).toBeInTheDocument();

    // A resetKeys change (not the in-fallback button) clears the error and re-renders children.
    await userEvent.click(screen.getByRole('button', { name: /change identity/i }));
    expect(await screen.findByText(/recovered content/i)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    errSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------------
// App-level integration of the highest-risk seams.
// ---------------------------------------------------------------------------

describe('App seams (QE-424)', () => {
  beforeEach(() => {
    window.history.replaceState({}, '', '/');
  });
  afterEach(() => vi.restoreAllMocks());

  it('seam 1 — the authed Backtests screen shows only backtest rows from a mixed payload (QE-408)', async () => {
    // App → AppShell → BacktestsArea → BacktestsList, end to end: a mixed `GET /api/runs` payload must
    // never leak a train row into the Backtests table (the client-side type filter). App-level companion
    // to the component-level BacktestsList test.
    const bktst: RunListItem = {
      id: 'bktst-keeper-1',
      type: 'backtest',
      label: 'v-keeper',
      status: 'succeeded',
      progress: { pct: 100, stage: 'report', msg: 'Scoring' },
      created_ms: 1_700_000_000_000,
    };
    const train: RunListItem = {
      id: 'train-leak-9',
      type: 'train',
      label: 'train 2019-06-06→2019-09-09',
      status: 'succeeded',
      progress: { pct: 100, stage: 'done', msg: 'Sealed' },
      created_ms: 1_700_000_003_000,
    };

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { path } = pathOf(input);
        if (path === '/api/me') return json({ email: 'ada@quant.example' });
        if (path === '/api/runs') return page([bktst, train]); // server ignores the ?type filter here
        return new Response(null, { status: 404 });
      }),
    );

    render(<App />);

    // The authed shell mounts on Backtests and the backtest row renders…
    expect(await screen.findByText('v-keeper')).toBeInTheDocument();
    // …while the training run is filtered out entirely (its id and distinctive label are absent).
    expect(screen.queryByText('train-le')).not.toBeInTheDocument();
    expect(screen.queryByText(/2019-06-06→2019-09-09/)).not.toBeInTheDocument();
    // Exactly one data row in the table.
    expect(document.querySelectorAll('tbody tr').length).toBe(1);
  });

  it('seam 4 — the Training→Backtest deep-link works end-to-end through App.tsx (router-less)', async () => {
    // The bespoke, router-less cross-area deep-link: opening a succeeded training run's "Backtest this
    // vintage" must land on the New-backtest form with THAT sealed vintage preselected — exercising
    // App.openBacktestForVintage → backtestVintage → BacktestsArea initialVintage → NewBacktest. This is
    // the seam the ticket calls out as untested (the two halves are tested in isolation, never wired).
    const SEALED = 'v-sealed-1';

    const trainRow: RunListItem = {
      id: 'train-1',
      type: 'train',
      label: 'train 2019-06-06→2019-09-09',
      status: 'succeeded',
      progress: { pct: 100, stage: 'done', msg: 'Sealed' },
      created_ms: 1_700_000_000_000,
    };

    const trainMeta: RunMeta = {
      id: 'train-1',
      type: 'train',
      status: 'succeeded',
      progress: { pct: 100, stage: 'gate', msg: 'G1 passed' },
      created_ms: 1_700_000_000_000,
      started_ms: 1_700_000_001_000,
      finished_ms: 1_700_000_002_000,
      exit: 0,
      error: null,
      artifacts: ['result.json'],
      params: { start: '2019-06-06', end: '2019-09-09', resolution: '1h' },
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
        vintage: SEALED,
      },
    };

    const vintages: VintageListItem[] = [
      { id: 'v-other', label: 'v-other', summary: { chromosomes: 8, content_hash: 'o', worst_case_loss: -0.1, format_version: 1 } },
      { id: SEALED, label: SEALED, summary: { chromosomes: 8, content_hash: 's', worst_case_loss: -0.1, format_version: 1 } },
    ];
    const coverage: CoverageRow[] = [
      { symbol: 'BTCUSDT', resolution: '1h', from: 1_600_000_000_000, to: 1_700_000_000_000, bars: 1000 },
    ];

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const { path, params } = pathOf(input);
        if (path === '/api/me') return json({ email: 'ada@quant.example' });
        if (path === '/api/runs') {
          // The training list asks for ?type=train; the initial Backtests list (empty) for ?type=backtest.
          return params.get('type') === 'train' ? page([trainRow]) : page([]);
        }
        if (path === '/api/runs/train-1') return json(trainMeta);
        if (path === '/api/vintages') return json(vintages);
        if (path === '/api/market-data/coverage') return json(coverage);
        return new Response(null, { status: 404 });
      }),
    );

    render(<App />);

    // Authenticated shell mounts; navigate to Training via the primary nav.
    const nav = await screen.findByRole('navigation', { name: /primary/i });
    await userEvent.click(within(nav).getByRole('button', { name: /training/i }));

    // Open the succeeded training run from the list.
    await userEvent.click(await screen.findByRole('button', { name: /train-1/i }));

    // The monitor shows the terminal run's sealed-vintage deep-link; follow it.
    await userEvent.click(await screen.findByRole('button', { name: /backtest this vintage/i }));

    // We land on the New-backtest form with the sealed vintage preselected — NOT the first vintage.
    expect(await screen.findByRole('heading', { name: /new backtest/i })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue(SEALED));
  });
});
