import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import { MarketData } from './MarketData';
import type { CoverageRow } from '../api/runs';

const COVERAGE: CoverageRow[] = [
  {
    symbol: 'BTCUSDT',
    resolution: '1h',
    from: Date.UTC(2020, 0, 1),
    to: Date.UTC(2024, 11, 31),
    bars: 43000,
    provenance: 'real',
    calibrated: true,
  },
  {
    symbol: 'ETHUSDT',
    resolution: '4h',
    from: Date.UTC(2021, 0, 1),
    to: Date.UTC(2024, 11, 31),
    bars: 8760,
    provenance: 'synthetic',
    calibrated: false,
  },
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

  it('renders a provenance badge per row (real vs synthetic), never unmarked', async () => {
    vi.stubGlobal('fetch', mockCoverage(COVERAGE));
    render(<MarketData />);

    expect(await screen.findByText('REAL')).toBeInTheDocument();
    expect(screen.getByText('SYNTHETIC')).toBeInTheDocument();
    // Calibration is surfaced on the real row.
    expect(screen.getByText('calibrated')).toBeInTheDocument();
  });

  it('marks EVERY row of an interleaved-provenance instrument (row-per-run, never one blended range)', async () => {
    // BTCUSDT @ 1h with three contiguous provenance runs: real → synthetic → real. The server splits a
    // mixed store into one row per run; the table must mark each — never a single unmarked range.
    const interleaved: CoverageRow[] = [
      {
        symbol: 'BTCUSDT',
        resolution: '1h',
        from: Date.UTC(2020, 0, 1),
        to: Date.UTC(2020, 5, 30),
        bars: 100,
        provenance: 'real',
        calibrated: true,
      },
      {
        symbol: 'BTCUSDT',
        resolution: '1h',
        from: Date.UTC(2020, 6, 1),
        to: Date.UTC(2020, 11, 31),
        bars: 100,
        provenance: 'synthetic',
        calibrated: false,
      },
      {
        symbol: 'BTCUSDT',
        resolution: '1h',
        from: Date.UTC(2021, 0, 1),
        to: Date.UTC(2021, 5, 30),
        bars: 100,
        provenance: 'real',
        calibrated: true,
      },
    ];
    vi.stubGlobal('fetch', mockCoverage(interleaved));
    render(<MarketData />);

    // Three rows, each carrying a provenance badge — two REAL, one SYNTHETIC.
    await screen.findAllByText('BTCUSDT');
    expect(screen.getAllByText('REAL')).toHaveLength(2);
    expect(screen.getAllByText('SYNTHETIC')).toHaveLength(1);

    // Assert NO body row is unmarked: every data row has exactly one provenance badge (REAL/SYNTHETIC/UNKNOWN).
    const rowGroups = screen.getAllByRole('rowgroup');
    const body = rowGroups[rowGroups.length - 1];
    const bodyRows = within(body).getAllByRole('row');
    expect(bodyRows).toHaveLength(3);
    for (const row of bodyRows) {
      const marks = within(row).getAllByText(/^(REAL|SYNTHETIC|UNKNOWN)$/);
      expect(marks).toHaveLength(1);
    }
  });

  it('marks a legacy untagged run UNKNOWN — never softened to real', async () => {
    const legacy: CoverageRow[] = [
      {
        symbol: 'BTCUSDT',
        resolution: '1h',
        from: Date.UTC(2019, 0, 1),
        to: Date.UTC(2019, 11, 31),
        bars: 50,
        provenance: 'unknown',
        calibrated: false,
      },
    ];
    vi.stubGlobal('fetch', mockCoverage(legacy));
    render(<MarketData />);
    expect(await screen.findByText('UNKNOWN')).toBeInTheDocument();
    expect(screen.queryByText('REAL')).not.toBeInTheDocument();
  });

  it('shows the Ingest-data affordance when onNewIngest is provided', async () => {
    const onNewIngest = vi.fn();
    vi.stubGlobal('fetch', mockCoverage(COVERAGE));
    render(<MarketData onNewIngest={onNewIngest} />);
    const btn = await screen.findByRole('button', { name: /ingest data/i });
    btn.click();
    expect(onNewIngest).toHaveBeenCalled();
  });

  it('shows an empty state when the store is empty', async () => {
    vi.stubGlobal('fetch', mockCoverage([]));
    render(<MarketData />);
    expect(await screen.findByText(/market-data store is empty/i)).toBeInTheDocument();
  });
});
