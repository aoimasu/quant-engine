/*
 * App-level auth signalling (QE-409).
 *
 * A single choke point turns "the API client saw an HTTP 401" into an app-level signal: `runs.ts`
 * emits `unauthorized` whenever any request comes back 401 (an expired/cleared session mid-session),
 * and the app shell (`App.tsx`) subscribes to flip `status` back to `'unauth'` and remount `Login` —
 * without a full-page reload. Kept dependency-free (no React, no DOM `CustomEvent`) so `runs.ts` stays
 * clear of app-layer imports and the pub/sub is trivially unit-testable.
 */

type UnauthorizedListener = () => void;

const listeners = new Set<UnauthorizedListener>();

/**
 * Subscribe to the "session expired / 401" signal. Returns an unsubscribe function (call it from a
 * React effect cleanup). Multiple subscribers are supported; each fires once per emit.
 */
export function onUnauthorized(listener: UnauthorizedListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Emit the `unauthorized` signal — the API client observed a 401. Iterates a snapshot so a listener
 * that unsubscribes itself during dispatch cannot perturb the in-progress iteration.
 */
export function emitUnauthorized(): void {
  for (const listener of [...listeners]) listener();
}
