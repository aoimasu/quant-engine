import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { FlowMonitor } from './FlowMonitor';
import type { FlowProgress, FlowRunMeta, RunStatus, VintageDetail } from '../../api/runs';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** A `type:"flow"` run meta fixture. */
function flowMeta(status: RunStatus, flow: FlowProgress): FlowRunMeta {
  return {
    id: 'flow-1',
    type: 'flow',
    status,
    params: { seed: 9, start: '2021-01-01', end: '2021-06-01', resolution: '1h' },
    progress: { pct: status === 'succeeded' ? 100 : 40, stage: 'flow', msg: 'flow: training over the holdout' },
    created_ms: 1,
    started_ms: 1,
    finished_ms: status === 'succeeded' ? 2 : null,
    exit: status === 'succeeded' ? 0 : null,
    error: null,
    artifacts: [],
    flow,
  };
}

function vintageDetail(): VintageDetail {
  return {
    id: 'vintage-flow-1',
    label: 'vintage-flow-1',
    content_hash: 'a'.repeat(64),
    format_version: 8,
    data_provenance: 'real',
    composition: [],
    seal_evidence: {
      dsr: 0.9,
      pbo: 0.1,
      spa_pvalue: 0.03,
      n_trials: 200,
      realised_turnover: 0.01,
      capacity_usd: 1_000_000,
    },
    holdout_series_handle: 'b'.repeat(64),
    holdout_series_len: 400,
    holdout_split: { embargo_bars: 20 },
    regime_composition: [
      { regime: 'bull', bars: 200 },
      { regime: 'bear', bars: 100 },
      { regime: 'chop', bars: 50 },
    ],
    consultation_count: 1,
    sidecars: { worst_case_loss: -0.2 },
    producing_runs: [],
  };
}

/** Route GET /api/runs/:id via `getMeta`, GET /api/vintages/:id via `getVintage`. */
function mockApi(getMeta: () => FlowRunMeta, getVintage: () => VintageDetail) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (/\/api\/runs\/[^/]+$/.test(url)) return json(getMeta());
    if (/\/api\/vintages\/[^/]+$/.test(url)) return json(getVintage());
    return new Response(null, { status: 404 });
  });
}

describe('FlowMonitor — the composite-flow per-phase monitor', () => {
  afterEach(() => vi.restoreAllMocks());

  it('shows the train phase active and the backtest phase pending while the flow runs', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi(() => flowMeta('running', { train_run: 'train-abc' }), vintageDetail),
    );

    render(
      <FlowMonitor runId="flow-1" onBack={() => {}} onInspectVintage={() => {}} pollMs={10_000} />,
    );

    // The two-phase progression is rendered; train is in progress, backtest is pending.
    expect(await screen.findByText('Train phase')).toBeInTheDocument();
    expect(screen.getByText('Backtest phase (frozen holdout)')).toBeInTheDocument();
    const states = screen.getAllByText(/in progress|pending/).map((n) => n.textContent);
    // Train row shows "in progress …"; backtest row shows "pending".
    expect(states.some((s) => /in progress/.test(s ?? ''))).toBe(true);
    expect(states.some((s) => /pending/.test(s ?? ''))).toBe(true);
  });

  it('on success renders the regime chips + the not-paper-confirmed label and links to the Inspector', async () => {
    const onInspect = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockApi(
        () => flowMeta('succeeded', { train_run: 'train-abc', backtest_run: 'bt-xyz', vintage: 'vintage-flow-1' }),
        vintageDetail,
      ),
    );

    render(
      <FlowMonitor runId="flow-1" onBack={() => {}} onInspectVintage={onInspect} pollMs={10_000} />,
    );

    // The "backtest-holdout only — not paper-confirmed" label mirrors the Inspector.
    expect((await screen.findAllByText(/not paper-confirmed/i)).length).toBeGreaterThan(0);
    expect(screen.getByText(/backtest-holdout evaluation/i)).toBeInTheDocument();

    // The holdout / regime chips are loaded from the sealed vintage.
    await waitFor(() => expect(screen.getByText('bull')).toBeInTheDocument());
    expect(screen.getByText('bear')).toBeInTheDocument();
    expect(screen.getByText('chop')).toBeInTheDocument();
    expect(screen.getByText('200 bars')).toBeInTheDocument();

    // The Inspector deep-link carries the sealed vintage id.
    await userEvent.click(screen.getByRole('button', { name: /open in vintage inspector/i }));
    expect(onInspect).toHaveBeenCalledWith('vintage-flow-1');
  });
});
