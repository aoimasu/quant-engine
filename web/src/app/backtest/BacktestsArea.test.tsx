import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BacktestsArea } from './BacktestsArea';
import type { CoverageRow, VintageListItem } from '../../api/runs';

const VINTAGES: VintageListItem[] = [
  { id: 'v-a', label: 'v-a', summary: { chromosomes: 8, content_hash: 'a', worst_case_loss: -0.1, format_version: 1 } },
  { id: 'v-b', label: 'v-b', summary: { chromosomes: 8, content_hash: 'b', worst_case_loss: -0.1, format_version: 1 } },
];

const COVERAGE: CoverageRow[] = [
  { symbol: 'BTCUSDT', resolution: '1h', from: 1_600_000_000_000, to: 1_700_000_000_000, bars: 1000, provenance: 'real', calibrated: true },
];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

function mockApi() {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (url.endsWith('/api/vintages')) return json(VINTAGES);
    if (url.endsWith('/api/market-data/coverage')) return json(COVERAGE);
    if (url.endsWith('/api/runs') && method === 'GET') return json([]);
    return new Response(null, { status: 404 });
  });
}

describe('BacktestsArea deep-link preselect', () => {
  afterEach(() => vi.restoreAllMocks());

  it('preselects the deep-linked vintage once, then a manual New-backtest starts blank', async () => {
    vi.stubGlobal('fetch', mockApi());

    // A QE-261 training → backtest deep-link: the area opens on the New-backtest form with `v-b`
    // preselected (not the first vintage, `v-a`).
    render(<BacktestsArea initialVintage="v-b" />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-b'));

    // Leave the seeded form → the runs list.
    await userEvent.click(screen.getByRole('button', { name: /cancel/i }));
    await screen.findByRole('button', { name: /new backtest/i });

    // A fresh *manual* New-backtest must NOT re-preselect the consumed vintage — it defaults to the
    // first vintage (`v-a`).
    await userEvent.click(screen.getByRole('button', { name: /new backtest/i }));
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-a'));
  });
});
