import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import { VintageInspector, ProvenanceBanner } from './VintageInspector';
import type { DataProvenance, VintageDetail } from '../../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** A fully-populated vintage detail fixture, parameterised by provenance. */
function detail(provenance: DataProvenance = 'real'): VintageDetail {
  return {
    id: 'vintage-2026-07-18',
    label: 'vintage-2026-07-18',
    content_hash: 'a'.repeat(64),
    format_version: 8,
    data_provenance: provenance,
    composition: [
      {
        index: 0,
        weight: 0.6,
        indicators: [
          { feature: 3, id: 'rsi_14', source: 'catalogue' },
          { feature: 999, source: 'evolved' },
        ],
      },
      {
        index: 1,
        weight: 0.4,
        indicators: [{ feature: 5, id: 'macd', source: 'catalogue' }],
      },
    ],
    seal_evidence: {
      dsr: 0.91,
      pbo: 0.12,
      spa_pvalue: 0.03,
      n_trials: 240,
      realised_turnover: 0.0123,
      capacity_usd: 4_200_000,
      cost_stress_net_min: 0.087,
      uncensored_pbo: 0.31,
      ic: 0.05,
      fdr: 0.1,
    },
    holdout_series_handle: 'b'.repeat(64),
    holdout_series_len: 512,
    holdout_split: {
      train_range: { start: '2020-01-01', end: '2024-01-01' },
      holdout_range: { start: '2024-02-01', end: '2025-01-01' },
      embargo_bars: 20,
    },
    regime_composition: [
      { regime: 'bull', bars: 300 },
      { regime: 'bear', bars: 150 },
      { regime: 'chop', bars: 62 },
    ],
    consultation_count: 3,
    sidecars: { worst_case_loss: -0.42 },
    producing_runs: [{ run_id: 'run-abc', run_type: 'train', status: 'succeeded', created_ms: 1 }],
    primary_run: 'run-abc',
  };
}

/** Route `GET /api/vintages/{id}` via `getState()`; everything else 404s. */
function mockApi(getState: () => VintageDetail | Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (/\/api\/vintages\/[^/]+$/.test(url) && method === 'GET') {
      const s = getState();
      return s instanceof Response ? s : json(s);
    }
    return new Response(null, { status: 404 });
  });
}

describe('VintageInspector — the read-only inspection screen', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders the composition (chromosome → indicators → weight) with catalogue vs evolved labelled', async () => {
    vi.stubGlobal('fetch', mockApi(() => detail('real')));
    render(<VintageInspector vintageId="vintage-2026-07-18" onBack={() => {}} />);

    expect(await screen.findByText('Chromosome #0')).toBeInTheDocument();
    expect(screen.getByText('Chromosome #1')).toBeInTheDocument();
    expect(screen.getByText('weight 0.6000')).toBeInTheDocument();
    expect(screen.getByText('weight 0.4000')).toBeInTheDocument();
    // A catalogue indicator shows its id; an evolved reference is labelled EVOLVED.
    expect(screen.getAllByText('rsi_14').length).toBeGreaterThan(0);
    expect(screen.getAllByText('CATALOGUE').length).toBeGreaterThan(0);
    expect(screen.getAllByText('EVOLVED').length).toBeGreaterThan(0);
  });

  it('leads the gate card with net-of-cost / tradability and shows the honest deflation basis', async () => {
    vi.stubGlobal('fetch', mockApi(() => detail('real')));
    render(<VintageInspector vintageId="vintage-2026-07-18" onBack={() => {}} />);

    // Net-of-cost / tradability lead (matches both the card title and the lead paragraph).
    expect((await screen.findAllByText(/Net-of-cost \/ tradability/i)).length).toBeGreaterThan(0);
    expect(screen.getByText('Cost-stress net min{1×,2×}')).toBeInTheDocument();
    expect(screen.getByText('0.087')).toBeInTheDocument(); // cost-stress net min
    expect(screen.getByText('Realised turnover')).toBeInTheDocument();
    expect(screen.getByText('0.0123')).toBeInTheDocument();
    expect(screen.getByText('Capacity (USD)')).toBeInTheDocument();
    expect(screen.getByText('$4,200,000')).toBeInTheDocument();

    // Honest deflation basis, demoted — DSR labelled necessary-not-sufficient, uncensored PBO + population.
    expect(screen.getByText(/Deflation basis/i)).toBeInTheDocument();
    expect(screen.getByText('necessary — not sufficient')).toBeInTheDocument();
    expect(screen.getByText('Uncensored PBO')).toBeInTheDocument();
    expect(screen.getByText('Distinct-trial N')).toBeInTheDocument();
    expect(screen.getByText('240')).toBeInTheDocument();
  });

  it('carries the "not paper-confirmed" backtest-holdout label', async () => {
    vi.stubGlobal('fetch', mockApi(() => detail('real')));
    render(<VintageInspector vintageId="vintage-2026-07-18" onBack={() => {}} />);
    // Appears in both the callout title and its body copy.
    expect((await screen.findAllByText(/not paper-confirmed/i)).length).toBeGreaterThan(0);
    expect(screen.getByText(/backtest-holdout evaluation/i)).toBeInTheDocument();
  });

  it('shows the frozen holdout split and its regime composition', async () => {
    vi.stubGlobal('fetch', mockApi(() => detail('real')));
    render(<VintageInspector vintageId="vintage-2026-07-18" onBack={() => {}} />);
    expect(await screen.findByText('2020-01-01 → 2024-01-01')).toBeInTheDocument();
    expect(screen.getByText('2024-02-01 → 2025-01-01')).toBeInTheDocument();
    expect(screen.getByText('bull')).toBeInTheDocument();
    expect(screen.getByText('bear')).toBeInTheDocument();
    expect(screen.getByText('chop')).toBeInTheDocument();
    expect(screen.getByText('300 bars')).toBeInTheDocument();
  });

  it('renders NO seal / promote / select / approve / revoke / reject control (inspection only)', async () => {
    vi.stubGlobal('fetch', mockApi(() => detail('real')));
    render(<VintageInspector vintageId="vintage-2026-07-18" onBack={() => {}} />);
    await screen.findByText('Vintage inspector');

    // The only button on the screen is the back navigation; no state-mutating governance control exists.
    const buttons = screen.getAllByRole('button');
    for (const b of buttons) {
      expect(b).toHaveAccessibleName(/all vintages/i);
    }
    for (const name of [/seal/i, /promote/i, /select/i, /approve/i, /revoke/i, /reject/i]) {
      expect(screen.queryByRole('button', { name })).not.toBeInTheDocument();
    }
  });

  it('surfaces a load error in a danger callout', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'vintage `x` not found' }, 500)),
    );
    render(<VintageInspector vintageId="x" onBack={() => {}} />);
    expect(await screen.findByText('Could not load the vintage')).toBeInTheDocument();
  });
});

describe('ProvenanceBanner — real / synthetic / mixed', () => {
  it('renders a calm, un-flagged banner for real data', () => {
    render(<ProvenanceBanner provenance="real" />);
    const banner = screen.getByRole('note', { name: /data provenance/i });
    expect(within(banner).getByText(/Real market data/i)).toBeInTheDocument();
    // Real is not flagged as synthetic/not-real anywhere in the banner.
    expect(within(banner).queryByText(/NOT REAL/i)).not.toBeInTheDocument();
    expect(within(banner).queryByText(/synthetic/i)).not.toBeInTheDocument();
  });

  it('makes a synthetic-derived vintage unmistakable', () => {
    render(<ProvenanceBanner provenance="synthetic" />);
    const banner = screen.getByRole('note', { name: /data provenance/i });
    expect(within(banner).getByText(/NOT REAL/i)).toBeInTheDocument();
    expect(within(banner).getAllByText(/synthetic/i).length).toBeGreaterThan(0);
  });

  it('calls out a mixed vintage distinctly and never softens it to real', () => {
    render(<ProvenanceBanner provenance="mixed" />);
    const banner = screen.getByRole('note', { name: /data provenance/i });
    expect(within(banner).getByText(/Mixed real \+ synthetic/i)).toBeInTheDocument();
    // "mixed" is explicitly NOT a pure real-data vintage.
    expect(within(banner).getByText(/NOT a pure real-data vintage/i)).toBeInTheDocument();
    // The banner tag never reads simply "Real market data" for a mixed vintage.
    expect(within(banner).queryByText('Real market data')).not.toBeInTheDocument();
  });
});
