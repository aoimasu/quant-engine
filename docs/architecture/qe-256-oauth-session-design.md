# QE-256 — Google OAuth + email allowlist + signed session — design / evidence note

`Phase: PreP3` · `Area: backend / auth` · `Depends on: QE-254 (scaffold), QE-255 (run API)`
Spec: admin-ui design `§6.3` (Auth / D4d), `§6.4` (Config); ADR **D4d**.

## 1. Goal

Gate the admin-UI backend behind Google sign-in restricted to an env allowlist. Authorization-Code
flow → verify the Google ID token → allowlist check → set a **cryptographically signed** session
cookie, verified on **every** protected `/api` call. `GET /api/me` returns the email or `401`.

## 2. Current-state evidence (what QE-254/255 give us)

- **Router shape** (`crates/server/src/lib.rs::build_router`): outer `Router` nests everything under
  `/api` (`Router::nest("/api", api)`), with a SPA `fallback_service` for everything else. The `/api`
  sub-router today = `.route("/health", …)` `.merge(runs::api::routes())` `.fallback(api_not_found)`
  `.with_state(Arc<RunManager>)`. Unknown `/api/*` → `404` (reserved namespace).
- **Runs routes** (`crates/server/src/runs/api.rs`): `POST/GET /api/runs`, `GET /api/runs/{id}`,
  `GET /api/runs/{id}/result`. Handlers extract `State<Arc<RunManager>>`. `routes()` returns
  `Router<Arc<RunManager>>`. Comment already anticipates: *"QE-256 layers session auth over the whole
  `/api` nest without touching them."*
- **Tests**: `crates/server/tests/http.rs` (QE-254: health/static/404) and
  `crates/server/tests/runs.rs` (QE-255: run lifecycle, `#![cfg(unix)]`) both call
  `build_router(&static_dir, manager)` and drive it with `tower::ServiceExt::oneshot` — no network.
- **Config**: `ServerConfig::from_env` reads `QE_SERVER_*` vars; relative defaults; loopback bind.
- **Firewall**: `crates/architecture/tests/firewall.rs` forbids any `qe-server → qe-runtime/qe-venue`
  edge and asserts the `qe-server → qe-telemetry` edge is real (anti-vacuity). Nothing we add here
  touches the live-trading tree.
- **Dependency policy precedent (QE-253)**: live network parked behind a **default-off `http`
  feature** using `ureq { default-features = false, features = ["native-tls"] }` — chosen because
  native-tls uses the **system** TLS and *"avoids ring's non-allowlisted OpenSSL licence"*. `ureq`,
  `base64`, `url` are already in `Cargo.lock`.

## 3. Decisions

### 3.1 Verifier seam (mockable, no network in tests)

```rust
pub struct GoogleClaims { pub email, email_verified: bool, aud, iss: String, exp: u64 }
pub trait IdTokenVerifier: Send + Sync + 'static {
    /// Exchange the authorization `code` and return the signature-verified Google ID-token claims.
    fn verify(&self, code: &str) -> Result<GoogleClaims, VerifyError>;
}
```

- **Sync** trait (object-safe, no `async-trait` dep). The real impl does blocking I/O (`ureq`); the
  callback handler runs it inside `tokio::task::spawn_blocking`, so async stays clean and confined.
- The trait returns the **raw** claims; the **claim-policy checks live handler-side** in a pure
  `check_claims(&claims, expected_aud, now)` function so they are exercised by the mock (acceptance).
- Mock: tests implement `IdTokenVerifier` themselves (public trait + public `GoogleClaims`) returning a
  chosen claim set — no network, no real Google keys.

### 3.2 Real Google verifier (`http` feature, default-off, NOT acceptance-tested)

`GoogleOidcVerifier` behind `#[cfg(feature = "http")]`: `ureq` POSTs the token endpoint
(`code`→`id_token`) then validates the ID token and extracts claims.

- **Signature verification is delegated to Google's `tokeninfo` endpoint** rather than a local
  JWKS/RS256 verify. **Rationale (dependency policy):** a local RS256 verify needs either
  `jsonwebtoken` (pulls **`ring`**, whose license is *not* on our allowlist — the QE-253 note calls
  this out explicitly) or the pure-Rust **`rsa`** crate (carries **RUSTSEC-2023-0071**, the Marvin
  timing side-channel, an *unsound* advisory). The ticket forbids weakening advisory/ban checks and
  license gates, so both are off the table. `tokeninfo` gives a real, working, license-/advisory-clean
  verifier using only `ureq` (already blessed). **Follow-up:** local JWKS RS256 once an
  advisory-/license-clean crypto path exists. This mirrors QE-253 parking live network behind `http`.
- The `http` code is compiled only under the feature; the default green gate does not build it. We run
  `cargo clippy -p qe-server --features http` locally as diligence.

### 3.3 Session cookie — format, signing, constant-time compare

- Cookie `qe_session` = `b64url(payload) "." b64url(HMAC-SHA256(payload_b64))`, where
  `payload = "v1|<email>|<exp_epoch_secs>"`.
- Signed with **HMAC-SHA256** keyed by `QE_SESSION_SECRET` (`hmac` + `sha2`).
- **Verify uses `hmac`'s `Mac::verify_slice`, which is a constant-time comparison** — never `==` on the
  MAC (timing-attack safe). After MAC passes, the `exp` is checked against now.
- Attributes: `HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=<ttl>`. HttpOnly blocks JS theft;
  Secure keeps it off plaintext; SameSite=Lax is the OAuth-friendly CSRF default.

### 3.4 Allowlist semantics (fail-closed)

- `QE_ADMIN_ALLOWED_EMAILS` = comma-separated. Each entry **trimmed + lowercased**; the candidate
  email is trimmed + lowercased before an exact-set membership test (case-insensitive).
- **Empty/unset ⇒ empty set ⇒ nobody is allowed (fail closed).** A misconfiguration denies access
  rather than opening it. Documented + tested.

### 3.5 Which routes are public vs gated

| Route | Access | Why |
|---|---|---|
| `GET /api/health` | **public** | liveness probe; no secrets (spec §6 keeps health open) |
| `GET /api/auth/login` | **public** | you cannot have a session before logging in |
| `GET /api/auth/callback` | **public** | OAuth redirect target, pre-session |
| `GET /api/me` | **gated** | returns the session email; `401` without a valid session |
| `POST/GET /api/runs*` | **gated** | QE-255 routes, now behind the session |
| unknown `/api/*` | public `404` | reserved-namespace behavior unchanged from QE-254 |

Implementation: a single `AppState { manager, auth }` with `FromRef<AppState>` for both `Arc<RunManager>`
and `Arc<AuthContext>` — so the QE-255 handlers keep `State<Arc<RunManager>>` **unchanged**. The
session middleware is attached with `route_layer` to a **protected sub-router** (`/me` + runs); the
public sub-router (`/health`, `/auth/*`) is merged without it.

### 3.6 CSRF `state`

`/api/auth/login` mints `state = uuid v4`, sets it in a short-lived `qe_oauth_state` cookie
(HttpOnly/Secure/SameSite=Lax) AND embeds it in the Google auth URL. `/api/auth/callback` requires the
`state` query param to equal the cookie (double-submit) — mismatch/absent ⇒ `400`. The state cookie is
cleared on success.

### 3.7 Env vars (spec §6.4 canonical, ticket aliases accepted)

The design spec §6.4 and the backlog ticket text disagree on names. We accept **both**, preferring the
spec §6.4 name, falling back to the ticket alias:

| Purpose | Canonical (spec §6.4) | Alias (ticket) |
|---|---|---|
| client id | `QE_OAUTH_GOOGLE_CLIENT_ID` | `QE_GOOGLE_CLIENT_ID` |
| client secret | `QE_OAUTH_GOOGLE_CLIENT_SECRET` | `QE_GOOGLE_CLIENT_SECRET` |
| redirect uri | `QE_OAUTH_REDIRECT_URI` | `QE_GOOGLE_REDIRECT_URI` |
| session secret | `QE_SESSION_SECRET` | — |
| allowlist | `QE_ADMIN_ALLOWED_EMAILS` | — |

`QE_SESSION_SECRET` unset ⇒ a random ephemeral secret is generated at boot with a warning (sessions do
not survive a restart; the server still boots but nobody is signed in — safe). Missing client
id/secret leave login unable to complete (Google rejects an empty `client_id`) — also safe.

### 3.8 New dependencies + license justification

| Crate | Where | License | Justification |
|---|---|---|---|
| `hmac` 0.12 | always | MIT/Apache-2.0 | HMAC-SHA256 session signing + constant-time verify |
| `sha2` 0.10 | always | MIT/Apache-2.0 | already a workspace dep; SHA-256 for the HMAC |
| `base64` 0.22 | always | MIT/Apache-2.0 | already in lock; cookie payload/MAC encoding |
| `url` 2 | always | MIT/Apache-2.0 | already in lock; build the Google auth redirect URL |
| `ureq` 2 (opt) | `http` feature | MIT/Apache-2.0 | already in lock (QE-253); token exchange + tokeninfo |

No `deny.toml` change is expected: all licenses are already allow-listed, and we deliberately avoid
`ring`/`rsa`. `Cargo.lock` is committed. `getrandom` (via `uuid` v4, already present) supplies the
CSRF `state` and the ephemeral session secret — no new RNG dep.

## 4. Threat / security review

- **ID-token substitution / forgery** — a token not signed by Google. Real impl: signature validated
  (via tokeninfo) before claims are trusted; `aud` must equal our client id (a token minted for another
  app is rejected), `iss ∈ {accounts.google.com, https://accounts.google.com}`.
- **Expired token replay** — `exp` checked against `now`; expired ⇒ rejected.
- **Unverified email** — `email_verified == true` required; else rejected (prevents a Google account
  with an unproven email from matching an allowlisted address).
- **Non-allowlisted valid login** — a genuine Google user not on the list ⇒ `403` (fail-closed empty
  set covers misconfig).
- **Session cookie tampering / forgery** — HMAC-SHA256 over the payload; any bit flip fails
  `verify_slice`. **Constant-time** compare defeats MAC-forgery timing oracles. Secret never leaves the
  server; cookie is HttpOnly (no JS exfiltration) + Secure (no plaintext).
- **CSRF on callback** — `state` double-submit (cookie vs query) blocks a forged callback; SameSite=Lax
  on the session cookie blocks cross-site state-changing requests.
- **Timing** — MAC compare is constant-time; email/allowlist compares are not secret-dependent.
- **Open redirect** — the callback only ever redirects to a fixed local `/`, never to a
  request-controlled URL.

## 5. Test plan (all hermetic: tokio + `tower::oneshot`, mocked verifier)

Unit:
- `check_claims`: good; wrong `aud`; wrong `iss`; expired `exp`; `email_verified=false`.
- allowlist: trim/case-insensitive match; non-member; **empty set ⇒ deny**.
- cookie: sign→verify round-trip; tampered payload ⇒ reject; tampered MAC ⇒ reject; expired ⇒ reject.

Integration (`tests/auth.rs`):
- No session ⇒ `401` on `/api/me` **and** a protected `/api/runs` route.
- Allowlisted login (mock claims) ⇒ callback `302` + `Set-Cookie`; `/api/me` with it ⇒ `200` + email.
- Valid login **not** allowlisted ⇒ `403`.
- Negative: forged/tampered session cookie ⇒ `401`; wrong `aud`/`iss`/expired/`email_verified=false`
  ⇒ not logged in; CSRF `state` mismatch ⇒ rejected.
- Login endpoint ⇒ `302` to Google with `client_id/redirect_uri/scope/state` + a `qe_oauth_state` cookie.

Existing suites kept green:
- `tests/runs.rs` — a test helper mints a valid session cookie via the **same** public signing code
  (`qe_server::auth::mint_session_cookie`) and attaches `Cookie: qe_session=…` to every request. No
  production auth weakened; tests authenticate legitimately.
- `tests/http.rs` — updated to `build_router(&dir, AppState)`; public routes need no session.
- `cargo test -p qe-architecture --test firewall` — unaffected (no live-tree edge).

## 6. Risks

- **Real verifier is `http`-only + not acceptance-tested** (network). Mitigated: the security-relevant
  *logic* (claims, allowlist, signing) is fully tested with the mock; `http` code compiles under
  `cargo clippy --features http`.
- **tokeninfo vs local JWKS** — a documented, deliberate deviation forced by the dependency policy;
  local JWKS RS256 is a follow-up.
- **Ephemeral session secret when unset** — restart invalidates sessions; acceptable for admin scale,
  documented; production sets `QE_SESSION_SECRET`.
