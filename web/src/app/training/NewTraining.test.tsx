import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NewTraining } from './NewTraining';

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

describe('NewTraining', () => {
  afterEach(() => vi.restoreAllMocks());

  it('POSTs a type:"train" run with the entered window + budget and reports the new id', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'train-1' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);

    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.type(screen.getByLabelText('Generations'), '3');
    await userEvent.type(screen.getByLabelText('Seed'), '7');

    await userEvent.click(screen.getByRole('button', { name: /start training/i }));

    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('train-1'));

    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    expect(postCall).toBeTruthy();
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.type).toBe('train');
    expect(body.params.start).toBe('2021-01-01');
    expect(body.params.end).toBe('2021-02-01');
    expect(body.params.resolution).toBe('1h');
    expect(body.params.generations).toBe(3);
    expect(body.params.seed).toBe(7);
    // Blank optional budget fields are omitted (the CLI applies its own defaults).
    expect(body.params.population).toBeUndefined();
  });

  it('blocks submit with a client-side validation message when the window is missing', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.click(screen.getByRole('button', { name: /start training/i }));

    expect(await screen.findByText(/window start date/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
    expect(
      fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST'),
    ).toBe(false);
  });

  it('surfaces a server 400 inline and does not navigate', async () => {
    const onCreated = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'no bars for the training window' }, 400)),
    );

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.click(screen.getByRole('button', { name: /start training/i }));

    expect(await screen.findByText(/no bars for the training window/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
  });
});
