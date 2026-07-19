import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NewBacktest } from './NewBacktest';
import type { CoverageRow, VintageListItem } from '../../api/runs';

const VINTAGES: VintageListItem[] = [
  {
    id: 'v-2024-q4',
    label: 'v-2024-q4',
    summary: { chromosomes: 8, content_hash: 'abc', worst_case_loss: -0.12, format_version: 1 },
  },
];

const COVERAGE: CoverageRow[] = [
  { symbol: 'BTCUSDT', resolution: '1h', from: 1_600_000_000_000, to: 1_700_000_000_000, bars: 1000, provenance: 'real', calibrated: true },
  { symbol: 'ETHUSDT', resolution: '1h', from: 1_600_000_000_000, to: 1_700_000_000_000, bars: 1000, provenance: 'real', calibrated: true },
];

/** Route GET /api/vintages + /api/market-data/coverage; POST /api/runs via `post`. */
function mockApi(post: (body: unknown) => Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (url.endsWith('/api/vintages')) return json(VINTAGES);
    if (url.endsWith('/api/market-data/coverage')) return json(COVERAGE);
    if (url.endsWith('/api/runs') && method === 'POST') {
      return post(init?.body ? JSON.parse(init.body as string) : {});
    }
    return new Response(null, { status: 404 });
  });
}

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

describe('NewBacktest', () => {
  afterEach(() => vi.restoreAllMocks());

  it('POSTs the entered params to /api/runs and reports the new id', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'run-new-1' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewBacktest onCreated={onCreated} onCancel={() => {}} />);

    // Vintage select is populated from GET /api/vintages.
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));

    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2024-12-31');

    await userEvent.click(screen.getByRole('button', { name: /run backtest/i }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('run-new-1'));

    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    expect(postCall).toBeTruthy();
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.type).toBe('backtest');
    expect(body.params.vintage).toBe('v-2024-q4');
    expect(body.params.start).toBe('2021-01-01');
    expect(body.params.end).toBe('2024-12-31');
    // Universe defaults to every stored symbol.
    expect(body.params.universe).toEqual(['BTCUSDT', 'ETHUSDT']);
    expect(body.params.taker_fee_bps).toBe(2);
  });

  it('surfaces a server 400 inline and does not navigate', async () => {
    const onCreated = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'unknown vintage `v-2024-q4`' }, 400)),
    );

    render(<NewBacktest onCreated={onCreated} onCancel={() => {}} />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2024-12-31');
    await userEvent.click(screen.getByRole('button', { name: /run backtest/i }));

    expect(await screen.findByText(/unknown vintage/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
  });

  it('shows the v1 single-instrument universe hint', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ id: 'x' }, 201)),
    );
    render(<NewBacktest onCreated={() => {}} onCancel={() => {}} />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));
    // The first selected symbol (BTCUSDT, alpha-sorted) is named in the hint.
    expect(screen.getByText(/v1 backtests the first selected symbol \(BTCUSDT\)/i)).toBeInTheDocument();
  });

  it('exposes universe chips as real checkboxes and toggles one via keyboard (QE-422)', async () => {
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ id: 'x' }, 201)),
    );
    render(<NewBacktest onCreated={() => {}} onCancel={() => {}} />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));

    // Each chip announces as a checkbox named by its symbol, checked by default.
    const btc = screen.getByRole('checkbox', { name: 'BTCUSDT' });
    const eth = screen.getByRole('checkbox', { name: 'ETHUSDT' });
    expect(btc).toBeChecked();
    expect(eth).toBeChecked();

    // Keyboard alone: focus + Space toggles BTC off (selection state follows checked).
    btc.focus();
    expect(btc).toHaveFocus();
    await userEvent.keyboard(' ');
    expect(btc).not.toBeChecked();
    expect(eth).toBeChecked(); // independent — only the focused chip changed.

    // Enter toggles it back on (native checkboxes ignore Enter; wired explicitly).
    await userEvent.keyboard('{Enter}');
    expect(btc).toBeChecked();
  });

  it('drops a keyboard-deselected symbol from the submitted universe (QE-422)', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'run-new-2' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewBacktest onCreated={onCreated} onCancel={() => {}} />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2024-12-31');

    // Deselect ETHUSDT purely by keyboard, then submit.
    const eth = screen.getByRole('checkbox', { name: 'ETHUSDT' });
    eth.focus();
    await userEvent.keyboard(' ');
    expect(eth).not.toBeChecked();

    await userEvent.click(screen.getByRole('button', { name: /run backtest/i }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('run-new-2'));

    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.params.universe).toEqual(['BTCUSDT']);
  });

  it('blocks submit with a client-side validation message when the window is missing', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewBacktest onCreated={onCreated} onCancel={() => {}} />);
    await waitFor(() => expect(screen.getByLabelText('Vintage')).toHaveValue('v-2024-q4'));

    await userEvent.click(screen.getByRole('button', { name: /run backtest/i }));

    expect(await screen.findByText(/window start date/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
    expect(fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(
      false,
    );
  });
});
