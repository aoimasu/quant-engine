import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { PoolBrowser } from './PoolBrowser';

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const POOLS = [
  {
    id: 'campaign-abc',
    mode: 'sandbox',
    content_hash: 'a'.repeat(64),
    pool_hash: 'c'.repeat(64),
    formula_count: 8,
    gp_aware: true,
    distinct_evaluations: 200,
    lifecycle: 'sealed',
  },
  {
    id: 'campaign-xyz',
    mode: 'production',
    content_hash: 'd'.repeat(64),
    pool_hash: 'e'.repeat(64),
    formula_count: 12,
    gp_aware: false,
    distinct_evaluations: 90,
    lifecycle: 'approved',
  },
];

describe('PoolBrowser', () => {
  afterEach(() => vi.restoreAllMocks());

  it('lists pools with their lifecycle + mode and opens PoolReview on row-click', async () => {
    const onOpen = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input.toString();
        if (url.endsWith('/api/formula-pools')) return json(POOLS);
        return new Response(null, { status: 404 });
      }),
    );

    render(<PoolBrowser onBack={() => {}} onOpen={onOpen} />);

    // Both pools render with their lifecycle badges.
    expect(await screen.findByText('SEALED')).toBeInTheDocument();
    expect(screen.getByText('APPROVED')).toBeInTheDocument();
    // The non-GP-aware pool is flagged FLOOR.
    expect(screen.getByText('FLOOR')).toBeInTheDocument();

    // Row-click opens the review gate for that pool.
    await userEvent.click(screen.getByText('SEALED'));
    expect(onOpen).toHaveBeenCalledWith('campaign-abc');
  });
});
