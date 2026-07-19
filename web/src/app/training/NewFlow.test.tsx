import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NewFlow } from './NewFlow';

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

async function postedBody(fetchMock: ReturnType<typeof mockApi>) {
  const postCall = fetchMock.mock.calls.find(
    ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
  );
  expect(postCall).toBeTruthy();
  return JSON.parse((postCall![1] as RequestInit).body as string) as {
    type: string;
    params: Record<string, unknown>;
  };
}

/** Fill the required configure step (window + required seed) and advance to review. */
async function configureAndReview(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByLabelText('Start'), '2021-01-01');
  await user.type(screen.getByLabelText('End'), '2021-06-01');
  await user.type(screen.getByLabelText(/Seed/), '9');
  await user.click(screen.getByRole('button', { name: /next: review/i }));
}

describe('NewFlow — the single stepped composite-flow page', () => {
  afterEach(() => vi.restoreAllMocks());

  it('steps configure → review and POSTs a single type:"flow" run with the FlowParams body shape', async () => {
    const user = userEvent.setup();
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'flow-1' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewFlow onCreated={onCreated} onCancel={() => {}} />);

    // Step 1: configure. No POST happens on "Next".
    await configureAndReview(user);
    expect(
      fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST'),
    ).toBe(false);

    // Step 2: review shows the exact request body, then Launch fires ONE POST.
    expect(screen.getByLabelText('Flow request body')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /launch flow/i }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('flow-1'));

    const posts = fetchMock.mock.calls.filter(
      ([, init]) => (init as RequestInit | undefined)?.method === 'POST',
    );
    expect(posts).toHaveLength(1);

    const body = await postedBody(fetchMock);
    // The single create is type:"flow" with a FlowParams params object: required seed + window present.
    expect(body.type).toBe('flow');
    expect(body.params.seed).toBe(9);
    expect(body.params.start).toBe('2021-01-01');
    expect(body.params.end).toBe('2021-06-01');
    expect(body.params.resolution).toBe('1h');
    // The full catalogue is selected ⇒ indicator_subset omitted (engine default).
    expect(body.params.indicator_subset).toBeUndefined();
    // NEVER a blocklisted gate-decision knob, an evolved-pool field, or a separate backtest window.
    for (const bad of [
      'cost_stress_multiplier',
      'max_turnover_frac',
      'capacity_floor_usd',
      'dsr_cutoff',
      'pbo_cutoff',
      'ic_fdr_threshold',
      'purge',
      'evolved_pool',
      'evolved_formulas',
      'vintage',
      'universe',
      'strategy',
      'backtest_start',
      'backtest_end',
    ]) {
      expect(body.params[bad]).toBeUndefined();
    }
  });

  it('submits a STRICT indicator subset + whitelisted windows/folds and no blocklisted field', async () => {
    const user = userEvent.setup();
    const onCreated = vi.fn();
    const fetchMock = mockApi(() => json({ id: 'flow-2' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewFlow onCreated={onCreated} onCancel={() => {}} />);
    await user.type(screen.getByLabelText('Start'), '2021-01-01');
    await user.type(screen.getByLabelText('End'), '2021-06-01');
    await user.type(screen.getByLabelText(/Seed/), '9');
    // Deselect one catalogue indicator → a strict subset; set whitelisted windows/folds above their floors.
    await user.click(screen.getByRole('button', { name: 'rsi_14', pressed: true }));
    await user.type(screen.getByLabelText(/WFO windows/), '6');
    await user.type(screen.getByLabelText(/CV folds/), '4');

    await user.click(screen.getByRole('button', { name: /next: review/i }));
    await user.click(screen.getByRole('button', { name: /launch flow/i }));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith('flow-2'));

    const body = await postedBody(fetchMock);
    const subset = body.params.indicator_subset as string[];
    expect(Array.isArray(subset)).toBe(true);
    expect(subset).not.toContain('rsi_14');
    expect(subset).toContain('atr_pct_14');
    expect(body.params.windows).toBe(6);
    expect(body.params.folds).toBe(4);
    expect(body.params.cost_stress_multiplier).toBeUndefined();
    expect(body.params.dsr_cutoff).toBeUndefined();
    expect(body.params.evolved_pool).toBeUndefined();
  });

  it('requires a seed and never advances / POSTs without one', async () => {
    const user = userEvent.setup();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewFlow onCreated={() => {}} onCancel={() => {}} />);
    await user.type(screen.getByLabelText('Start'), '2021-01-01');
    await user.type(screen.getByLabelText('End'), '2021-06-01');
    // No seed entered.
    await user.click(screen.getByRole('button', { name: /next: review/i }));

    expect(await screen.findByText(/requires a seed/i)).toBeInTheDocument();
    // Still on the configure step (the review request-body preview is not shown) and no POST fired.
    expect(screen.queryByLabelText('Flow request body')).not.toBeInTheDocument();
    expect(
      fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST'),
    ).toBe(false);
  });

  it('blocks a windows value below the compiled floor before advancing to review', async () => {
    const user = userEvent.setup();
    const fetchMock = mockApi(() => json({ id: 'x' }, 201));
    vi.stubGlobal('fetch', fetchMock);

    render(<NewFlow onCreated={() => {}} onCancel={() => {}} />);
    await user.type(screen.getByLabelText('Start'), '2021-01-01');
    await user.type(screen.getByLabelText('End'), '2021-06-01');
    await user.type(screen.getByLabelText(/Seed/), '9');
    await user.type(screen.getByLabelText(/WFO windows/), '2'); // below the ≥4 floor

    await user.click(screen.getByRole('button', { name: /next: review/i }));
    expect(await screen.findByText(/compiled floor 4/i)).toBeInTheDocument();
    expect(screen.queryByLabelText('Flow request body')).not.toBeInTheDocument();
  });

  it('renders the blocklisted thresholds as disabled guardrail chips with no enabled control that can set them', () => {
    render(<NewFlow onCreated={() => {}} onCancel={() => {}} />);

    const floorGroup = screen.getByRole('group', { name: /compiled gate floors/i });
    const chips = floorGroup.querySelectorAll('button');
    expect(chips.length).toBeGreaterThan(0);
    chips.forEach((c) => expect(c).toBeDisabled());

    for (const bad of [/cost.?stress/i, /turnover/i, /capacity/i, /dsr/i, /pbo/i, /ic.?fdr/i]) {
      expect(screen.queryByRole('textbox', { name: bad })).toBeNull();
      expect(screen.queryByRole('spinbutton', { name: bad })).toBeNull();
    }

    const evolvedGroup = screen.getByRole('group', { name: /evolved-pool formulas/i });
    evolvedGroup.querySelectorAll('button').forEach((b) => expect(b).toBeDisabled());
  });

  it('surfaces a server 400 inline on launch and does not navigate', async () => {
    const user = userEvent.setup();
    const onCreated = vi.fn();
    vi.stubGlobal(
      'fetch',
      mockApi(() => json({ error: 'no bars for the flow window' }, 400)),
    );

    render(<NewFlow onCreated={onCreated} onCancel={() => {}} />);
    await configureAndReview(user);
    await user.click(screen.getByRole('button', { name: /launch flow/i }));

    expect(await screen.findByText(/no bars for the flow window/i)).toBeInTheDocument();
    expect(onCreated).not.toHaveBeenCalled();
  });
});
