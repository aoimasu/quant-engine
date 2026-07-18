import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NewCampaign } from './NewCampaign';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** Route POST /api/runs via `post`; everything else 404. */
function mockApi(post: (body: unknown) => Response) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input.toString();
    const method = (init?.method ?? 'GET').toUpperCase();
    if (url.endsWith('/api/runs') && method === 'POST') {
      return post(init?.body ? JSON.parse(init.body as string) : {});
    }
    return new Response(null, { status: 404 });
  });
}

function posted(fetchMock: ReturnType<typeof vi.fn>) {
  return fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST');
}

describe('NewCampaign', () => {
  afterEach(() => vi.restoreAllMocks());

  it('blocks submit when the seed is missing (seed is REQUIRED)', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewCampaign onCreated={onCreated} onCancel={() => {}} />);
    // Fill everything EXCEPT the seed.
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.click(screen.getByRole('button', { name: /launch campaign/i }));

    expect(await screen.findByText(/a seed is required/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
    expect(posted(fetchMock)).toBe(false);
  });

  it('POSTs a type:"evolve" run with the seed + window + sandbox mode on a valid submit', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'evolve-1' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewCampaign onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText(/seed/i), '20260718');
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.click(screen.getByRole('button', { name: /launch campaign/i }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('evolve-1'));

    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.type).toBe('evolve');
    expect(body.params.seed).toBe(20260718);
    expect(body.params.mode).toBe('sandbox');
    expect(body.params.start).toBe('2021-01-01');
    expect(body.params.end).toBe('2021-02-01');
    expect(body.params.resolution).toBe('1h');
    // The full lattice is the default ⇒ `windows` is omitted (defers to the engine default).
    expect(body.params.windows).toBeUndefined();
  });

  it('blocks submit on a client-side cap violation (nodes > 16) — mirrors validate_evolve', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewCampaign onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText(/seed/i), '7');
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.type(screen.getByLabelText(/nodes/i), '99');
    await userEvent.click(screen.getByRole('button', { name: /launch campaign/i }));

    expect(await screen.findByText(/must be ≤ 16/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
    expect(posted(fetchMock)).toBe(false);
  });

  it('surfaces a server 400 inline and does not navigate', async () => {
    const onCreated = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'production launch refused — DEFLATION_BASIS_VERSION < REQUIRED' }, 400)),
    );

    render(<NewCampaign onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText(/seed/i), '7');
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.click(screen.getByRole('button', { name: /launch campaign/i }));

    expect(await screen.findByText(/production launch refused/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
  });
});
