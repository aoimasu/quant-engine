import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { App } from './App';

function mockMe(status: number, body?: unknown) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === 'string' ? input : input.toString();
    if (url.endsWith('/api/me')) {
      return new Response(body ? JSON.stringify(body) : null, {
        status,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    return new Response(null, { status: 404 });
  });
}

describe('App session gate', () => {
  beforeEach(() => {
    window.history.replaceState({}, '', '/');
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('shows Login when unauthenticated (GET /api/me => 401)', async () => {
    vi.stubGlobal('fetch', mockMe(401));
    render(<App />);
    expect(await screen.findByRole('button', { name: /sign in with google/i })).toBeInTheDocument();
    expect(screen.getByText(/sign in to quant engine/i)).toBeInTheDocument();
    // The shell must NOT be present.
    expect(screen.queryByRole('navigation', { name: /primary/i })).not.toBeInTheDocument();
  });

  it('renders the AppShell with the Research nav after a mocked session (200 {email})', async () => {
    vi.stubGlobal('fetch', mockMe(200, { email: 'ada@quant.example' }));
    render(<App />);

    // Research group is active/enabled.
    const nav = await screen.findByRole('navigation', { name: /primary/i });
    expect(nav).toBeInTheDocument();
    for (const label of ['Strategies', 'Backtests', 'Market data']) {
      const item = screen.getByRole('button', { name: new RegExp(label, 'i') });
      expect(item).toBeEnabled();
    }

    // The signed-in email surfaces in the shell footer.
    expect(screen.getByText('ada@quant.example')).toBeInTheDocument();

    // Login is gone.
    expect(screen.queryByRole('button', { name: /sign in with google/i })).not.toBeInTheDocument();
  });

  it('renders Trade and Risk nav items present-but-disabled', async () => {
    vi.stubGlobal('fetch', mockMe(200, { email: 'ada@quant.example' }));
    render(<App />);
    await screen.findByRole('navigation', { name: /primary/i });

    for (const label of ['Dashboard', 'Positions', 'Orders', 'Risk', 'API & docs']) {
      const item = screen.getByRole('button', { name: new RegExp(label, 'i') });
      expect(item).toBeDisabled();
    }
  });

  it('shows the allowlist-rejection state when loaded with ?error=forbidden', async () => {
    window.history.replaceState({}, '', '/?error=forbidden');
    vi.stubGlobal('fetch', mockMe(401));
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText(/access denied/i)).toBeInTheDocument();
    });
    expect(screen.getByText(/isn't on the admin allowlist/i)).toBeInTheDocument();
    // Still offers a retry sign-in.
    expect(screen.getByRole('button', { name: /sign in with google/i })).toBeInTheDocument();
  });
});
