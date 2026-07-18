import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { PoolReview } from './PoolReview';
import type { PoolDetail, PoolLifecycleState, PoolMode } from '../../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function detail(mode: PoolMode, lifecycle: PoolLifecycleState): PoolDetail {
  return {
    content_hash: 'a'.repeat(64),
    lifecycle,
    history: [],
    content: {
      format_version: 1,
      pool_id: 'campaign-abc',
      mode,
      formulas: [{ sexpr: 'rank(delta(close)/roll_std(close,20),50)', formula_hash: 'b'.repeat(64) }],
      deflation: {
        gp_aware: true,
        distinct_evaluations: 200,
        n_trials: 200,
        analytic_floor: 90,
        variance_trials: 200,
        trial_variance: '0.12',
        expected_max_sharpe: '2.1',
        champion_dsr: '0.97',
        uncensored_pbo: '0.42',
      },
      lineage: {
        campaign_id: 'campaign-abc',
        seed: 20260718,
        mode,
        code_commit: 'commit-deadbeef',
        input_snapshot_id: '',
        config_hash: 'cfg-hash',
        pool_hash: 'c'.repeat(64),
      },
    },
  };
}

/** Route GET detail via `getState()` and POST transitions via `onPost`. */
function mockApi(getState: () => PoolDetail, onPost: (transition: string) => Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    const m = url.match(/\/api\/formula-pools\/([^/]+)(?:\/(\w+))?$/);
    if (m && method === 'GET') return json(getState());
    if (m && method === 'POST') return onPost(m[2]);
    return new Response(null, { status: 404 });
  });
}

describe('PoolReview — the governance gate', () => {
  afterEach(() => vi.restoreAllMocks());

  it('surfaces the production-seal 409 and keeps the lifecycle at approved (fail-closed, never faked)', async () => {
    // Production pool, approved. The server refuses the seal with a 409 and the state never changes.
    const state = detail('production', 'approved');
    const fetchMock = mockApi(
      () => state,
      (t) =>
        t === 'seal'
          ? json(
              {
                error:
                  'governance not yet enabled — sealing to production is gated on QE-454 (seal_allowed / DEFLATION_BASIS_VERSION)',
                pool_id: 'campaign-abc',
                mode: 'production',
              },
              409,
            )
          : json({ pool_id: 'campaign-abc', lifecycle: 'approved' }),
    );
    vi.stubGlobal('fetch', fetchMock);

    render(<PoolReview poolId="campaign-abc" onBack={() => {}} />);

    // Seal is offered from `approved` (mirrors the state machine), marked gated in production.
    const seal = await screen.findByRole('button', { name: /seal/i });
    await userEvent.click(seal);

    // The server's named-blocker 409 message is surfaced, NOT hidden or faked as success.
    expect(await screen.findByText(/gated on QE-454/i)).toBeInTheDocument();
    // The lifecycle badge still reads APPROVED (the re-fetch reflects the true, unchanged server state).
    await waitFor(() => expect(screen.getByText('APPROVED')).toBeInTheDocument());
    expect(screen.queryByText('SEALED')).not.toBeInTheDocument();
  });

  it('seals a sandbox pool and reflects the returned sealed lifecycle', async () => {
    // Mutable state so the POST advances what the subsequent GET returns (server truth).
    let current = detail('sandbox', 'approved');
    const fetchMock = mockApi(
      () => current,
      (t) => {
        if (t === 'seal') {
          current = detail('sandbox', 'sealed');
          return json({ pool_id: 'campaign-abc', lifecycle: 'sealed' });
        }
        return json({ pool_id: 'campaign-abc', lifecycle: current.lifecycle });
      },
    );
    vi.stubGlobal('fetch', fetchMock);

    render(<PoolReview poolId="campaign-abc" onBack={() => {}} />);

    const seal = await screen.findByRole('button', { name: /seal/i });
    await userEvent.click(seal);

    expect(await screen.findByText('SEALED')).toBeInTheDocument();
  });

  it('omits Seal from a non-approved (draft) state — mirrors the server state machine', async () => {
    const state = detail('sandbox', 'draft');
    vi.stubGlobal(
      'fetch',
      mockApi(
        () => state,
        () => json({ pool_id: 'campaign-abc', lifecycle: 'draft' }),
      ),
    );

    render(<PoolReview poolId="campaign-abc" onBack={() => {}} />);

    // Approve + Reject are legal from draft; Seal is NOT offered.
    expect(await screen.findByRole('button', { name: /approve/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /reject/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /seal/i })).not.toBeInTheDocument();
  });
});
