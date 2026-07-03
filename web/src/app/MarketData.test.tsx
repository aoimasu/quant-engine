import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MarketData } from './MarketData';
import type { CoverageRow } from '../api/runs';

const COVERAGE: CoverageRow[] = [
  { symbol: 'BTCUSDT', resolution: '1h', from: Date.UTC(2020, 0, 1), to: Date.UTC(2024, 11, 31), bars: 43000 },
  { symbol: 'ETHUSDT', resolution: '4h', from: Date.UTC(2021, 0, 1), to: Date.UTC(2024, 11, 31), bars: 8760 },
];

function mockCoverage(rows: CoverageRow[]) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (url.endsWith('/api/market-data/coverage')) {
      return new Response(JSON.stringify(rows), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    return new Response(null, { status: 404 });
  });
}

describe('MarketData coverage', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders symbol × range rows from GET /api/market-data/coverage', async () => {
    vi.stubGlobal('fetch', mockCoverage(COVERAGE));
    render(<MarketData />);

    expect(await screen.findByText('BTCUSDT')).toBeInTheDocument();
    expect(screen.getByText('ETHUSDT')).toBeInTheDocument();
    // Ranges rendered as UTC days (both rows end 2024-12-31).
    expect(screen.getByText('2020-01-01')).toBeInTheDocument();
    expect(screen.getAllByText('2024-12-31').length).toBe(2);
    // Bar counts localised.
    expect(screen.getByText('43,000')).toBeInTheDocument();
  });

  it('shows an empty state when the store is empty', async () => {
    vi.stubGlobal('fetch', mockCoverage([]));
    render(<MarketData />);
    expect(await screen.findByText(/market-data store is empty/i)).toBeInTheDocument();
  });
});
