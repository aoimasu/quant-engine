import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NewIngest } from './NewIngest';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** Route POST /api/ingest via `post`; everything else 404. */
function mockApi(post: (body: unknown) => Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (url.endsWith('/api/ingest') && method === 'POST') {
      return post(init?.body ? JSON.parse(init.body as string) : {});
    }
    return new Response(null, { status: 404 });
  });
}

function postCalls(fetchMock: ReturnType<typeof mockApi>) {
  return fetchMock.mock.calls.filter(
    ([url, init]) =>
      (init as RequestInit | undefined)?.method === 'POST' &&
      String(url).endsWith('/api/ingest'),
  );
}

function postedBody(fetchMock: ReturnType<typeof mockApi>): Record<string, unknown> {
  const call = postCalls(fetchMock)[0];
  expect(call).toBeTruthy();
  return JSON.parse((call![1] as RequestInit).body as string) as Record<string, unknown>;
}

async function fillWindow(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByLabelText('Start'), '2021-01-01');
  await user.type(screen.getByLabelText('End'), '2021-06-01');
}

describe('NewIngest — the ingest-trigger screen', () => {
  afterEach(() => vi.restoreAllMocks());

  it('POSTs the exact IngestParams body to /api/ingest (instruments / window / resolution / synthetic:false)', async () => {
    const user = userEvent.setup();
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'ingest-1' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewIngest onCreated={onCreated} onCancel={() => {}} />);

    await user.type(screen.getByLabelText('Instruments'), 'btcusdt, ethusdt');
    await fillWindow(user);
    await user.click(screen.getByRole('button', { name: /launch ingest/i }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('ingest-1'));

    // Exactly one POST, and its body is the bare IngestParams object (NOT a {type,params} envelope).
    expect(postCalls(fetchMock)).toHaveLength(1);
    const body = postedBody(fetchMock);
    expect(body.type).toBeUndefined();
    expect(body.instruments).toEqual(['BTCUSDT', 'ETHUSDT']);
    expect(body.fetch_all).toBe(false);
    expect(body.start).toBe('2021-01-01');
    expect(body.end).toBe('2021-06-01');
    expect(body.resolution).toBe('1h');
    // The trigger form is a REAL ingest — synthetic is always false (design §8.2).
    expect(body.synthetic).toBe(false);
  });

  it('fetch-all sends fetch_all:true with an empty instruments list', async () => {
    const user = userEvent.setup();
    const fetchMock = mockApi(() => json({ id: 'ingest-2' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewIngest onCreated={vi.fn()} onCancel={() => {}} />);

    // Even with a stray typed symbol, enabling fetch-all drops the explicit list (mirrors validate_ingest).
    await user.type(screen.getByLabelText('Instruments'), 'BTCUSDT');
    await user.click(screen.getByLabelText('Fetch all instruments'));
    await fillWindow(user);
    await user.click(screen.getByRole('button', { name: /launch ingest/i }));

    await waitFor(() => expect(postCalls(fetchMock)).toHaveLength(1));
    const body = postedBody(fetchMock);
    expect(body.fetch_all).toBe(true);
    expect(body.instruments).toEqual([]);
  });

  it('client hint: neither instruments nor fetch-all ⇒ inline warn, no POST', async () => {
    const user = userEvent.setup();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewIngest onCreated={vi.fn()} onCancel={() => {}} />);
    await fillWindow(user);
    await user.click(screen.getByRole('button', { name: /launch ingest/i }));

    expect(await screen.findByText(/at least one instrument, or enable fetch-all/i)).toBeInTheDocument();
    expect(postCalls(fetchMock)).toHaveLength(0);
  });

  it('surfaces a server 400 inline (validate_ingest is the enforcement point)', async () => {
    const user = userEvent.setup();
    const fetchMock = mockApi(() => json({ error: '`resolution` is required' }, 400));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewIngest onCreated={vi.fn()} onCancel={() => {}} />);
    await user.type(screen.getByLabelText('Instruments'), 'BTCUSDT');
    await fillWindow(user);
    await user.click(screen.getByRole('button', { name: /launch ingest/i }));

    expect(await screen.findByText(/`resolution` is required/)).toBeInTheDocument();
  });
});
