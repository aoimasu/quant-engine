# QE-409 — Auth completeness design note

Ticket: **QE-409 — Auth completeness: 401 → re-auth in the SPA, logout endpoint, dev-safe cookies,
fail-closed secret** (spec of record: `### QE-409` in
`docs/reviews/2026-07-15-team-improvement-review.md`, lines 284–308).
Area: frontend (`web/`) + backend (`crates/server`) / auth. Adjacent to QE-265 — OIDC nonce / local
JWKS / RS256 are **strictly out of scope** here.

## Current state (read from HEAD)

### Frontend
- `web/src/api/runs.ts` — `getJson`/`postRun` throw a generic `ApiError(status, msg)` for **any**
  non-OK, so a 401 (expired cookie mid-session) is indistinguishable from a 500. There is **no 401
  branch**.
- `web/src/api/usePollingRun.ts` (QE-410) — the shared run poller. `catch` increments a bounded
  `failures` streak (`MAX_POLL_FAILURES = 4`), sets `retrying`, and reschedules; past the cap it sets
  a **fatal** `error` (the "Backtest failed" surface). A 401 currently consumes this retry budget and
  eventually renders a fatal error.
- `web/src/api/useRunListPolling.ts` (QE-410) — the shared list poller, same bounded-retry shape.
- `web/src/app/App.tsx` — the app shell. `status: 'loading' | 'unauth' | 'auth'`; `unauth` renders
  `<Login/>`. `status` is set **once** from the initial `fetchMe()`; nothing flips it back to
  `'unauth'` after mount. The "Sign out" control already exists in `AppShell` → `onSignOut` →
  `logout()` (`web/src/api/session.ts`).
- `web/src/api/session.ts` — `logout()` already `POST`s `/api/auth/logout` then `assign('/')`. The
  server endpoint did **not** exist yet.

### Backend
- `crates/server/src/auth/mod.rs` — session + OAuth-state cookies are minted with a **hard-coded
  `Secure`** attribute (`login` state cookie L253; `callback` session + clear-state cookies L318/324).
  No `/auth/logout` route. `/api/me` behind `require_session`.
- `AuthConfig::from_env` — falls back to a **random ephemeral** session secret when
  `QE_SESSION_SECRET` is unset (warns only). No record of whether the fallback was taken.
- `crates/server/src/main.rs` — boots unconditionally; binds `cfg.addr` (default
  `127.0.0.1:8080`). No non-loopback/secret gate.
- Default bind is loopback `http://127.0.0.1:8080`, so an unconditional `Secure` cookie is silently
  dropped by the browser on the default dev address — login "succeeds" but the session never persists.

## Approach

### FE — 401 → re-auth (no reload)
1. `runs.ts`: add `class UnauthorizedError extends ApiError` (status fixed to 401). Route both
   `getJson` and `postRun` through one `throwForResponse(res)` helper that, on **401**, emits an
   app-level `unauthorized` signal and throws `UnauthorizedError`; otherwise throws `ApiError`.
   `UnauthorizedError extends ApiError` so every existing `instanceof ApiError` consumer keeps working.
2. New tiny pub/sub module `web/src/api/authEvents.ts` (`onUnauthorized`/`emitUnauthorized`) — no DOM
   events, no React import, so `runs.ts` stays free of app-layer deps and it is unit-testable.
3. `App.tsx`: `useEffect` subscribes to `onUnauthorized`; on fire it `setStatus('unauth')` +
   `setMe(null)`. React remounts `<Login/>` — **no `window.location` reload**.

### FE — 401 classification in the pollers (terminal-auth, distinct from transient/terminal-status)
In both `usePollingRun` and `useRunListPolling`, the `catch` checks `e instanceof UnauthorizedError`
**first**, before the transient-retry logic:
- **terminal-auth**: `return` immediately — do **not** increment `failures`, do **not** set
  `retrying`, do **not** set the fatal `error` (so no "Backtest failed"), do **not** reschedule.
  Stops the poll instantly and never burns the retry budget.
- transient error → existing bounded retry; terminal run status → existing stop.
The `emitUnauthorized()` already fired inside `getJson`, so the app-level flip happens in parallel;
the poller's only job is to stop cleanly.

### BE — logout endpoint
`GET|POST /api/auth/logout` in `public_routes` (reachable without a valid session — you must be able
to clear a broken/expired cookie). Responds `200` and `Set-Cookie: qe_session=; …; Max-Age=0` (same
name/path/attrs, `Secure` conditional). A subsequent `/api/me` with no valid cookie ⇒ 401.

### BE — cookie `Secure` conditionality rule
`Secure` is emitted **iff the deployment is served over https**, derived from the OAuth
`redirect_uri` scheme (the external-URL signal already in `AuthConfig`):

> `cookie_secure = redirect_uri scheme is "https"`.

- default dev (`redirect_uri` unset/empty → not https) ⇒ **no `Secure`** ⇒ the cookie is sent over
  `http://127.0.0.1` ⇒ default-loopback dev login persists.
- https deploy (`redirect_uri = https://…`) ⇒ **`Secure`** present.
- `HttpOnly` + `SameSite=Lax` are **always** kept.

Computed once in `from_env` into `AuthConfig.cookie_secure`; a pure `cookie_secure_for(&str)` helper
is unit-tested. A single `set_cookie(name, value, secure, max_age)` builder is used for the session,
state, clear-state, and logout cookies so the attribute set can never drift between them.

### BE — fail-closed session secret
`AuthConfig` gains `session_secret_is_ephemeral: bool` (true when the random fallback was taken).
Pure decision fn `check_session_secret_policy(&SocketAddr, is_ephemeral) -> Result<(), EphemeralSecretRefused>`:
refuse (`Err`) when `is_ephemeral && !addr.ip().is_loopback()`; `Ok` otherwise. `main.rs` calls it
after resolving config and **before binding**; `Err` ⇒ structured error + `ExitCode::FAILURE`.
Loopback keeps the ephemeral fallback (AR-9); `0.0.0.0` / any routable IP without a secret refuses.

## Test plan (each AC covered, non-vacuous)
- **FE** `usePollingRun.test.tsx`: a 401 during polling stops the poll (fetch called once, no retry),
  sets no fatal `error`, no `retrying`, and fires the `onUnauthorized` listener.
- **FE** `useRunListPolling.test.tsx`: same terminal-auth behaviour for the list poller.
- **FE** `App.test.tsx`: authed shell + a subsequent `/api/runs` 401 flips the app back to `<Login/>`
  without a reload (`window.location` untouched).
- **BE** `auth/mod.rs` unit: `cookie_secure_for` — http/loopback/empty ⇒ false, https ⇒ true;
  `check_session_secret_policy` — non-loopback+ephemeral ⇒ Err, loopback+ephemeral ⇒ Ok,
  non-loopback+explicit ⇒ Ok.
- **BE** `tests/auth.rs`: login → `/api/me` 200 → `POST /api/auth/logout` clears the cookie
  (`Max-Age=0`) → `/api/me` with the cleared cookie ⇒ 401; https config ⇒ session cookie has
  `Secure`+`HttpOnly`+`SameSite=Lax`; http-loopback config ⇒ **no** `Secure` (keeps HttpOnly/Lax).

## Risks / blast radius
- `UnauthorizedError extends ApiError` — a 401 on `POST /api/runs` is still caught by the existing
  `instanceof ApiError` handlers (NewBacktest / NewTraining / rerun); they set a transient message,
  but the app-level flip unmounts them, so the message is never seen. No behavioural regression.
- Emitting `unauthorized` from the API client is a deliberate single choke point so **any** 401
  (list, run, coverage, vintages, create) flips the shell exactly once.
- Cookie `Secure` keyed off `redirect_uri` scheme: a misconfigured https-behind-proxy deploy that
  leaves `redirect_uri` http would not set `Secure` — an existing-style misconfiguration, not a new
  risk; documented.
- No new crate dependencies (std::net + existing `thiserror`), so the QE-132 firewall / QE-001
  decoupling guards stay green. Changes are confined to `auth/**` + `main.rs` boot — off the QE-406/
  407/408/411 run-handler hot path.
- No golden/vintage bytes touched.
