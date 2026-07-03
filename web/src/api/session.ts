/*
 * Session API — integrates with the qe-server auth contract (QE-256):
 *   GET  /api/me            → 200 { email } | 401
 *   GET  /api/auth/login    → 302 to Google (top-level navigation)
 *   POST /api/auth/logout   → clears the session cookie
 */

export interface Me {
  email: string;
}

/** Fetch the current user, or null when unauthenticated (401). */
export async function fetchMe(): Promise<Me | null> {
  const res = await fetch('/api/me', {
    credentials: 'same-origin',
    headers: { Accept: 'application/json' },
  });
  if (res.status === 401) return null;
  if (!res.ok) {
    throw new Error(`GET /api/me failed: ${res.status}`);
  }
  return (await res.json()) as Me;
}

/** Begin Google OAuth. Full-page navigation — the OAuth dance needs top-level. */
export function startLogin(): void {
  window.location.assign('/api/auth/login');
}

/** Clear the session, then return to the app root (which re-renders Login). */
export async function logout(): Promise<void> {
  try {
    await fetch('/api/auth/logout', { method: 'POST', credentials: 'same-origin' });
  } finally {
    window.location.assign('/');
  }
}

/**
 * Rejection detection. The server's callback returns 403 for a valid Google
 * login that is not on QE_ADMIN_ALLOWED_EMAILS. The SPA-facing signal is read
 * from the URL (`?error=forbidden|rejected|403|not_allowed`). See the design
 * note §1/§4 — the exact wiring is finalised with QE-259.
 */
const REJECTION_CODES = new Set(['forbidden', 'rejected', '403', 'not_allowed', 'unauthorized']);

export function detectRejection(search: string = window.location.search): boolean {
  const err = new URLSearchParams(search).get('error');
  return err != null && REJECTION_CODES.has(err.toLowerCase());
}
