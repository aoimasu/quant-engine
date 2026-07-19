import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IngestMonitor } from './IngestMonitor';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const RUNNING_META = {
  id: 'ingest-1',
  type: 'ingest',
  params: {
    instruments: ['BTCUSDT', 'ETHUSDT'],
    fetch_all: false,
    start: '2021-01-01',
    end: '2021-06-01',
    resolution: '1h',
    synthetic: false,
  },
  status: 'running',
  progress: { pct: 35, stage: 'fetch', msg: 'ingesting BTCUSDT' },
  created_ms: 1_700_000_000_000,
  started_ms: 1_700_000_001_000,
  finished_ms: null,
  exit: null,
  error: null,
  artifacts: [],
};

describe('IngestMonitor — standard run monitor for an ingest run', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders the standard RunProgress from the polled meta (coarse stage/msg, no fabricated %)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/runs/ingest-1')) return json(RUNNING_META);
        return new Response(null, { status: 404 });
      }),
    );

    render(<IngestMonitor runId="ingest-1" onBack={() => {}} pollMs={20} />);

    // The coarse standard progress line the server emits is shown verbatim.
    expect(await screen.findByText(/ingesting BTCUSDT/i)).toBeInTheDocument();
    // The ingest request summary reflects the polled ingest params.
    expect(screen.getByText('BTCUSDT, ETHUSDT')).toBeInTheDocument();
    expect(screen.getByText('2021-01-01 → 2021-06-01')).toBeInTheDocument();
  });

  it('Cancel fires POST /api/runs/{id}/halt (the run-type-agnostic halt path)', async () => {
    const fetchMock = vi.fn<typeof fetch>(async (input: RequestInfo | URL) => {
      const url = typeof input === 'string' ? input : input.toString();
      if (url.endsWith('/api/runs/ingest-1/halt')) return json({ id: 'ingest-1', status: 'failed', halted: true });
      if (url.endsWith('/api/runs/ingest-1')) return json(RUNNING_META);
      return new Response(null, { status: 404 });
    });
    vi.stubGlobal('fetch', fetchMock);

    render(<IngestMonitor runId="ingest-1" onBack={() => {}} pollMs={20} />);

    const cancel = await screen.findByRole('button', { name: /cancel ingest/i });
    await userEvent.click(cancel);

    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(
          ([input, init]) =>
            (typeof input === 'string' ? input : input.toString()).endsWith('/api/runs/ingest-1/halt') &&
            (init as RequestInit | undefined)?.method === 'POST',
        ),
      ).toBe(true),
    );
  });
});
