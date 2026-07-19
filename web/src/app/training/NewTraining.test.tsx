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

  // ---- QE-459 steering controls ------------------------------------------------------------------

  async function postedBody(fetchMock: ReturnType<typeof mockApi>) {
    const postCall = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    expect(postCall).toBeTruthy();
    return JSON.parse((postCall![1] as RequestInit).body as string) as {
      params: Record<string, unknown>;
    };
  }

  it('submits a STRICT indicator subset (deselected id absent) and its whitelisted windows/folds', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'train-2' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    // Deselect one catalogue indicator → a strict subset.
    await userEvent.click(screen.getByRole('button', { name: 'rsi_14', pressed: true }));
    await userEvent.type(screen.getByLabelText(/WFO windows/), '6');
    await userEvent.type(screen.getByLabelText(/CV folds/), '4');

    await userEvent.click(screen.getByRole('button', { name: /start training/i }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('train-2'));

    const body = await postedBody(fetchMock);
    const subset = body.params.indicator_subset as string[];
    expect(Array.isArray(subset)).toBe(true);
    expect(subset).not.toContain('rsi_14');
    expect(subset).toContain('atr_pct_14');
    expect(body.params.windows).toBe(6);
    expect(body.params.folds).toBe(4);
    // Never the not-yet-supported / blocklisted fields.
    expect(body.params.evolved_pool).toBeUndefined();
    expect(body.params.evolved_formulas).toBeUndefined();
    expect(body.params.cost_stress_multiplier).toBeUndefined();
    expect(body.params.dsr_cutoff).toBeUndefined();
  });

  it('OMITS indicator_subset when the full catalogue is selected (engine default)', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'train-3' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.click(screen.getByRole('button', { name: /start training/i }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('train-3'));

    const body = await postedBody(fetchMock);
    expect(body.params.indicator_subset).toBeUndefined();
  });

  it('blocks a windows value below the compiled floor and does not POST', async () => {
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewTraining onCreated={onCreated} onCancel={() => {}} />);
    await userEvent.type(screen.getByLabelText('Start'), '2021-01-01');
    await userEvent.type(screen.getByLabelText('End'), '2021-02-01');
    await userEvent.type(screen.getByLabelText(/WFO windows/), '2'); // below the ≥4 floor

    await userEvent.click(screen.getByRole('button', { name: /start training/i }));

    expect(await screen.findByText(/compiled floor 4/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
    expect(
      fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST'),
    ).toBe(false);
  });

  it('shows a projected distinct-trial N that grows as the budget rises', async () => {
    render(<NewTraining onCreated={() => {}} onCancel={() => {}} />);
    const readN = () =>
      Number(screen.getByLabelText('Projected distinct-trial N').textContent!.replace(/[^0-9]/g, ''));

    const before = readN();
    await userEvent.type(screen.getByLabelText('Generations'), '200');
    expect(readN()).toBeGreaterThan(before);

    // The honest coverage framing is present; no fabricated pre-run percentage is shown.
    expect(screen.getByText(/recorded after the run/i)).toBeInTheDocument();
  });

  it('renders the blocklisted thresholds as disabled chips with no enabled control that can set them', () => {
    render(<NewTraining onCreated={() => {}} onCancel={() => {}} />);

    // Every compiled-floor chip is disabled (a fixed guardrail, not a control).
    const floorGroup = screen.getByRole('group', { name: /compiled gate floors/i });
    const chips = floorGroup.querySelectorAll('button');
    expect(chips.length).toBeGreaterThan(0);
    chips.forEach((c) => expect(c).toBeDisabled());

    // The blocklisted thresholds have NO enabled input control anywhere in the form.
    for (const bad of [/cost.?stress/i, /turnover/i, /capacity/i, /dsr/i, /pbo/i, /ic.?fdr/i]) {
      expect(screen.queryByRole('textbox', { name: bad })).toBeNull();
      expect(screen.queryByRole('spinbutton', { name: bad })).toBeNull();
      expect(screen.queryByRole('checkbox', { name: bad })).toBeNull();
    }

    // Evolved-pool inclusion is a disabled affordance, never an enabled toggle that would 400.
    const evolvedGroup = screen.getByRole('group', { name: /evolved-pool formulas/i });
    evolvedGroup.querySelectorAll('button').forEach((b) => expect(b).toBeDisabled());
  });
});
