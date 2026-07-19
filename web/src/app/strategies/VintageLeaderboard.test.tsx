import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, within } from '@testing-library/react';
import { VintageLeaderboard } from './VintageLeaderboard';
import type { Leaderboard, LeaderboardEntry } from '../../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function entry(over: Partial<LeaderboardEntry> & Pick<LeaderboardEntry, 'id' | 'rank'>): LeaderboardEntry {
  return {
    label: over.id,
    content_hash: 'a'.repeat(64),
    format_version: 8,
    data_provenance: 'real',
    cost_stress_net_min: 0.1,
    realised_turnover: 0.0123,
    capacity_usd: 4_200_000,
    dsr: 0.8,
    dsr_status: 'ok',
    consultation_count: 1,
    over_consulted: false,
    holdout_series_len: 100,
    steer_delta: {
      indicator_subset_hash: 'c'.repeat(64),
      generations: 30,
      population: 10,
      windows: 5,
      folds: 3,
    },
    not_paper_confirmed: true,
    ...over,
  };
}

/** A leaderboard fixture: a clean top vintage + a DEMOTED over-consulted one with a higher raw net. */
function board(): Leaderboard {
  return {
    entries: [
      entry({ id: 'clean-top', rank: 1, cost_stress_net_min: 0.12, dsr: 0.6 }),
      entry({
        id: 'over-consulted',
        rank: 2,
        cost_stress_net_min: 0.99,
        dsr: 0.95,
        dsr_status: 'escalated',
        consultation_count: 3,
        over_consulted: true,
        data_provenance: 'synthetic',
      }),
    ],
    cross_vintage_correlation: 0.42,
    effective_n: 96,
    effective_n_note: 'Aligned to the displayed-set minimum length (v1 limitation).',
    enforcement_posture: 'own-evidence-only',
    consultation_budget: 1,
    not_paper_confirmed: true,
    caveat:
      'Cross-vintage ranking is INSPECTION, not selection. Acting on it by re-running until the top slot improves IS the rejected best-of-N pattern.',
  };
}

/** Route `GET /api/vintages/leaderboard` via `getState()`; everything else 404s. */
function mockApi(getState: () => Leaderboard | Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (/\/api\/vintages\/leaderboard$/.test(url) && method === 'GET') {
      const s = getState();
      return s instanceof Response ? s : json(s);
    }
    return new Response(null, { status: 404 });
  });
}

describe('VintageLeaderboard — the read-only, non-selecting leaderboard (QE-466)', () => {
  afterEach(() => vi.restoreAllMocks());

  it('ranks on the persisted net-of-cost figure and shows capacity + turnover + steer diffs', async () => {
    vi.stubGlobal('fetch', mockApi(board));
    render(<VintageLeaderboard onOpen={() => {}} onBack={() => {}} />);

    // Ranked rows in server order (rank 1 = clean-top).
    const rows = await screen.findAllByRole('button', { name: /Inspect vintage/i });
    expect(within(rows[0]).getByText('clean-top')).toBeInTheDocument();
    expect(within(rows[1]).getByText('over-consulted')).toBeInTheDocument();

    // Net-of-cost lead + capacity + steer diff surfaced.
    expect(screen.getByText('0.120')).toBeInTheDocument(); // clean-top net-of-cost
    expect(screen.getAllByText('$4,200,000').length).toBeGreaterThan(0);
    expect(screen.getAllByText(/gens 30/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/folds 3/).length).toBeGreaterThan(0);
  });

  it('surfaces the QE-430 cross-vintage correlation + effective N as a diversity diagnostic', async () => {
    vi.stubGlobal('fetch', mockApi(board));
    render(<VintageLeaderboard onOpen={() => {}} onBack={() => {}} />);

    expect(await screen.findByText('Deflated correlation')).toBeInTheDocument();
    expect(screen.getByText('0.420')).toBeInTheDocument();
    expect(screen.getByText('Effective N')).toBeInTheDocument();
    expect(screen.getByText('96')).toBeInTheDocument();
    expect(screen.getByText('own-evidence-only')).toBeInTheDocument();
  });

  it('escalates/greys the over-consulted vintage DSR bar (consultation budget ENFORCED)', async () => {
    vi.stubGlobal('fetch', mockApi(board));
    render(<VintageLeaderboard onOpen={() => {}} onBack={() => {}} />);

    // The over-consulted row is flagged escalated + greyed.
    const over = await screen.findByRole('button', { name: /over-consulted \(holdout over-consulted\)/i });
    expect(over.className).toContain('qe-lb__row--escalated');
    expect(within(over).getByText('ESCALATED')).toBeInTheDocument();
  });

  it('labels every vintage not-paper-confirmed and carries the standing best-of-N caveat', async () => {
    vi.stubGlobal('fetch', mockApi(board));
    render(<VintageLeaderboard onOpen={() => {}} onBack={() => {}} />);

    expect((await screen.findAllByText(/not paper-confirmed/i)).length).toBeGreaterThan(0);
    expect(screen.getByText(/rejected best-of-N pattern/i)).toBeInTheDocument();
  });

  it('exposes NO promote/select/run action — the only row action is opening the read-only inspector', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal('fetch', mockApi(board));
    render(<VintageLeaderboard onOpen={onOpen} onBack={() => {}} />);
    await screen.findByText('clean-top');

    // No selection/promotion affordance anywhere.
    expect(screen.queryByRole('button', { name: /promote/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /select/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /seal/i })).toBeNull();
    expect(screen.queryByRole('button', { name: /^run/i })).toBeNull();

    // A row click only opens the inspector (inspection), nothing mutating.
    fireEvent.click(screen.getByRole('button', { name: /Inspect vintage clean-top/i }));
    expect(onOpen).toHaveBeenCalledWith('clean-top');
  });

  it('renders a load error without throwing', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'boom' }, 500)),
    );
    render(<VintageLeaderboard onOpen={() => {}} onBack={() => {}} />);
    expect(await screen.findByText(/Could not load the leaderboard/i)).toBeInTheDocument();
  });
});
