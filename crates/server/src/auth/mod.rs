//! Google OAuth + email allowlist + signed session (QE-256, spec §6.3/§6.4, ADR D4d).
//!
//! Layout:
//! - [`session`] — the HMAC-signed session cookie (sign / constant-time verify).
//! - [`google`] — the real network verifier (`ureq` + Google `tokeninfo`), behind the default-off
//!   `http` feature. The claim-verification *logic* lives here in [`check_claims`] and is always
//!   compiled + tested; only the network fetch is feature-gated.
//!
//! The [`IdTokenVerifier`] seam turns "authorization code → verified Google claims" into an injectable
//! trait so tests substitute a mock (chosen claims, no network) while production wires the real Google
//! verifier. The trait is **synchronous**; the callback handler runs it inside
//! [`tokio::task::spawn_blocking`], keeping async confined and avoiding an `async-trait` dependency.

pub mod session;

#[cfg(feature = "http")]
pub mod google;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::json;
use url::Url;
use uuid::Uuid;

use crate::AppState;

pub use session::{mint_session_cookie, verify_session_cookie, Session, SESSION_COOKIE_NAME};

/// Google's default OAuth 2.0 authorization endpoint.
pub const DEFAULT_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's default OAuth 2.0 token endpoint (authorization-code exchange).
pub const DEFAULT_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Google's ID-token validation endpoint (server-side signature + expiry check).
pub const DEFAULT_TOKENINFO_ENDPOINT: &str = "https://oauth2.googleapis.com/tokeninfo";

/// The two accepted `iss` values for a Google ID token.
const ALLOWED_ISS: [&str; 2] = ["accounts.google.com", "https://accounts.google.com"];

/// Short-lived cookie holding the CSRF `state` nonce between `/auth/login` and `/auth/callback`.
const OAUTH_STATE_COOKIE: &str = "qe_oauth_state";

/// Default session lifetime: 12 hours.
pub const DEFAULT_SESSION_TTL_SECS: u64 = 12 * 60 * 60;

// ---- env var names (spec §6.4 canonical, with the backlog-ticket aliases accepted) --------------

const ENV_CLIENT_ID: [&str; 2] = ["QE_OAUTH_GOOGLE_CLIENT_ID", "QE_GOOGLE_CLIENT_ID"];
const ENV_CLIENT_SECRET: [&str; 2] = ["QE_OAUTH_GOOGLE_CLIENT_SECRET", "QE_GOOGLE_CLIENT_SECRET"];
const ENV_REDIRECT_URI: [&str; 2] = ["QE_OAUTH_REDIRECT_URI", "QE_GOOGLE_REDIRECT_URI"];
const ENV_SESSION_SECRET: &str = "QE_SESSION_SECRET";
const ENV_ALLOWED_EMAILS: &str = "QE_ADMIN_ALLOWED_EMAILS";

/// The verified subset of Google ID-token claims the app cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleClaims {
    /// The user's email.
    pub email: String,
    /// Whether Google has verified ownership of `email`.
    pub email_verified: bool,
    /// Audience — must equal our OAuth client id.
    pub aud: String,
    /// Issuer — must be a Google issuer.
    pub iss: String,
    /// Expiry, epoch seconds.
    pub exp: u64,
}

/// Failure exchanging/verifying the authorization code (the network step behind [`IdTokenVerifier`]).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// Live Google verification was attempted without the `http` feature.
    #[error("live Google verification requires the `http` feature")]
    Unsupported,
    /// The token endpoint / tokeninfo call failed or rejected the token.
    #[error("token exchange/verification failed: {0}")]
    Upstream(String),
    /// The upstream response could not be parsed into claims.
    #[error("malformed token response: {0}")]
    Malformed(String),
}

/// Injectable "authorization code → signature-verified Google claims" seam.
///
/// Synchronous by design (see the module docs). The returned claims are **not** yet policy-checked:
/// [`check_claims`] (`aud`/`iss`/`exp`/`email_verified`) runs handler-side so the policy is exercised
/// by the mock in tests.
pub trait IdTokenVerifier: Send + Sync + 'static {
    /// Exchange `code` at Google and return the signature-verified ID-token claims.
    fn verify(&self, code: &str) -> Result<GoogleClaims, VerifyError>;
}

/// A verifier that always fails — wired when the `http` feature is off so the server still boots
/// (health/static work) but a login cannot complete. Mirrors QE-253's honest "enable `http`" error.
pub struct DisabledVerifier;

impl IdTokenVerifier for DisabledVerifier {
    fn verify(&self, _code: &str) -> Result<GoogleClaims, VerifyError> {
        Err(VerifyError::Unsupported)
    }
}

/// Why a claim set was rejected (maps to a `401` — not signed in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRejection {
    /// `aud` did not equal our client id (token minted for another app).
    Audience,
    /// `iss` was not a Google issuer.
    Issuer,
    /// The token had already expired.
    Expired,
    /// Google had not verified the email.
    EmailUnverified,
}

/// Policy-check verified Google claims against our expectations at wall-clock `now` (epoch seconds).
///
/// This is the security core the acceptance tests exercise through the mock verifier.
pub fn check_claims(
    claims: &GoogleClaims,
    expected_aud: &str,
    now: u64,
) -> Result<(), ClaimRejection> {
    if claims.aud != expected_aud {
        return Err(ClaimRejection::Audience);
    }
    if !ALLOWED_ISS.contains(&claims.iss.as_str()) {
        return Err(ClaimRejection::Issuer);
    }
    if now >= claims.exp {
        return Err(ClaimRejection::Expired);
    }
    if !claims.email_verified {
        return Err(ClaimRejection::EmailUnverified);
    }
    Ok(())
}

/// Parse `QE_ADMIN_ALLOWED_EMAILS` into a normalized (trimmed, lowercased, non-empty) set.
///
/// **Fail-closed:** an empty/blank string yields an empty list, so nobody is allowed.
pub fn parse_allowlist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Whether cookies should carry the `Secure` attribute for a deployment whose external URL is
/// `redirect_uri` (QE-409).
///
/// `Secure` is emitted **iff the deployment is served over https** (the `redirect_uri` scheme). A
/// loopback/`http` dev address (including the empty default, before OAuth is configured) yields
/// `false`, because a browser will not send a `Secure` cookie over `http://127.0.0.1` — an
/// unconditional `Secure` would silently break default-address dev login. The scheme match is
/// case-insensitive and anchored so only the URL scheme is inspected.
pub fn cookie_secure_for(redirect_uri: &str) -> bool {
    redirect_uri
        .split_once("://")
        .map(|(scheme, _)| scheme.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

/// Build a `Set-Cookie` header value with the QE-409 attribute policy: always `HttpOnly` +
/// `SameSite=Lax` + `Path=/`, and `Secure` only when `secure` (see [`cookie_secure_for`]). One builder
/// for the session, OAuth-state, clear-state, and logout cookies so the attribute set can never drift.
fn set_cookie(name: &str, value: &str, secure: bool, max_age: u64) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{name}={value}; HttpOnly{secure_attr}; SameSite=Lax; Path=/; Max-Age={max_age}")
}

/// The boot was refused because an ephemeral session secret would guard a network-exposed bind
/// (QE-409 / AR-9). The ephemeral fallback is safe only on loopback.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to boot: bound to a non-loopback address ({addr}) with an ephemeral session secret — \
     set {ENV_SESSION_SECRET} to a persistent value for a network-exposed deployment"
)]
pub struct EphemeralSecretRefused {
    /// The offending (non-loopback) bind address.
    pub addr: SocketAddr,
}

/// Fail-closed session-secret boot policy (QE-409 / AR-9). The random ephemeral session-secret
/// fallback is safe **only** on loopback (a restart-invalidated, per-process secret must never guard
/// a network-reachable deployment). Refuse when the bind is non-loopback and the secret is ephemeral;
/// loopback keeps the ephemeral fallback, and any bind with an explicit secret is fine.
///
/// # Errors
/// [`EphemeralSecretRefused`] when `addr` is non-loopback and `secret_is_ephemeral` is `true`.
pub fn check_session_secret_policy(
    addr: &SocketAddr,
    secret_is_ephemeral: bool,
) -> Result<(), EphemeralSecretRefused> {
    if secret_is_ephemeral && !addr.ip().is_loopback() {
        return Err(EphemeralSecretRefused { addr: *addr });
    }
    Ok(())
}

/// Whether boot should emit an **advisory** warning about insecure cookies (QE-409): the server is
/// bound to a non-loopback address while cookies are minted without `Secure` (the `redirect_uri` is
/// not https), so a session cookie could traverse the network unprotected — a likely misconfiguration
/// (wrong `redirect_uri` scheme / TLS-terminating proxy). Advisory only: unlike
/// [`check_session_secret_policy`], this never refuses boot.
pub fn should_warn_insecure_cookies(addr: &SocketAddr, cookie_secure: bool) -> bool {
    !addr.ip().is_loopback() && !cookie_secure
}

/// Server-side OAuth + session configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// OAuth client id (also the expected ID-token `aud`).
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: String,
    /// The registered redirect URI (our `/api/auth/callback`).
    pub redirect_uri: String,
    /// Google authorization endpoint.
    pub auth_endpoint: String,
    /// Google token endpoint.
    pub token_endpoint: String,
    /// Google tokeninfo endpoint.
    pub tokeninfo_endpoint: String,
    /// Allowed emails, already normalized (trimmed + lowercased). Empty ⇒ nobody allowed.
    pub allowed_emails: Vec<String>,
    /// HMAC key for the session cookie.
    pub session_secret: Vec<u8>,
    /// Whether [`session_secret`](Self::session_secret) is a **random ephemeral** fallback (no
    /// `QE_SESSION_SECRET` was set). Safe only on loopback (AR-9); the boot check
    /// [`check_session_secret_policy`] refuses a non-loopback bind while this is `true`.
    pub session_secret_is_ephemeral: bool,
    /// Session lifetime, seconds.
    pub session_ttl_secs: u64,
    /// Whether cookies are minted with the `Secure` attribute (QE-409). Derived from the deployment
    /// scheme (the OAuth `redirect_uri`): `true` for an `https` deployment, `false` for loopback/`http`
    /// dev — because a browser drops a `Secure` cookie over `http://127.0.0.1`, so an unconditional
    /// `Secure` silently breaks default-address dev login. `HttpOnly` + `SameSite=Lax` are always kept.
    pub cookie_secure: bool,
}

impl AuthConfig {
    /// Resolve from the environment (spec §6.4 names, ticket aliases accepted). Never hard-fails:
    /// missing OAuth creds simply prevent a login from completing; a missing `QE_SESSION_SECRET`
    /// falls back to a **random ephemeral** secret (sessions don't survive a restart) — both are
    /// safe (fail-closed) defaults so the server always boots.
    pub fn from_env() -> Self {
        let explicit_secret = std::env::var(ENV_SESSION_SECRET)
            .ok()
            .filter(|s| !s.is_empty());
        let session_secret_is_ephemeral = explicit_secret.is_none();
        let session_secret = explicit_secret.map(String::into_bytes).unwrap_or_else(|| {
            tracing::warn!(
                "{ENV_SESSION_SECRET} unset — using a random ephemeral session secret; \
                     sessions will not survive a restart. Set {ENV_SESSION_SECRET} in production."
            );
            // 256 bits from two v4 UUIDs (getrandom-backed).
            let mut key = Uuid::new_v4().as_bytes().to_vec();
            key.extend_from_slice(Uuid::new_v4().as_bytes());
            key
        });

        let redirect_uri = env_first(&ENV_REDIRECT_URI);
        let cookie_secure = cookie_secure_for(&redirect_uri);

        Self {
            client_id: env_first(&ENV_CLIENT_ID),
            client_secret: env_first(&ENV_CLIENT_SECRET),
            redirect_uri,
            auth_endpoint: DEFAULT_AUTH_ENDPOINT.to_owned(),
            token_endpoint: DEFAULT_TOKEN_ENDPOINT.to_owned(),
            tokeninfo_endpoint: DEFAULT_TOKENINFO_ENDPOINT.to_owned(),
            allowed_emails: parse_allowlist(&std::env::var(ENV_ALLOWED_EMAILS).unwrap_or_default()),
            session_secret,
            session_secret_is_ephemeral,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            cookie_secure,
        }
    }

    /// Whether `email` is on the allowlist (case-insensitive, trimmed on both sides).
    pub fn is_allowed(&self, email: &str) -> bool {
        let candidate = email.trim().to_lowercase();
        !candidate.is_empty() && self.allowed_emails.iter().any(|a| a == &candidate)
    }
}

/// The shared auth state: config + the injectable verifier. Carried in [`AppState`].
pub struct AuthContext {
    /// OAuth + session config.
    pub config: AuthConfig,
    /// The ID-token verifier (real Google impl, or a mock in tests).
    pub verifier: Arc<dyn IdTokenVerifier>,
}

impl AuthContext {
    /// Build an auth context from config + a verifier.
    pub fn new(config: AuthConfig, verifier: Arc<dyn IdTokenVerifier>) -> Self {
        Self { config, verifier }
    }
}

/// The authenticated email, injected into request extensions by [`require_session`] for downstream
/// handlers (e.g. [`me`]) and the QE-452 Phase B [`require_role`] seam. Public so governance handlers can
/// read it as the transition **actor** and the role guard can resolve the caller's roles per-request.
#[derive(Debug, Clone)]
pub struct AuthedEmail(pub String);

/// Public (unauthenticated) auth routes: the OAuth entry + redirect target.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        // Logout is public: you must be able to clear a broken/expired cookie without holding a valid
        // session. `GET|POST` so a plain link and the SPA's `fetch(POST)` both reach it (QE-409).
        .route("/auth/logout", get(logout).post(logout))
}

/// `GET /api/auth/login` — mint a CSRF `state`, set it in a short-lived cookie, and `302` to Google's
/// consent screen carrying `client_id`/`redirect_uri`/`scope=openid email`/`state`.
async fn login(State(auth): State<Arc<AuthContext>>) -> Response {
    let state = Uuid::new_v4().to_string();
    let location = build_auth_url(&auth.config, &state);
    let state_cookie = set_cookie(OAUTH_STATE_COOKIE, &state, auth.config.cookie_secure, 600);
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, location),
            (header::SET_COOKIE, state_cookie),
        ],
    )
        .into_response()
}

/// Query params on the OAuth redirect back to `/api/auth/callback`.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// `GET /api/auth/callback` — verify CSRF `state`, exchange+verify the ID token, enforce the
/// allowlist, and set the signed session cookie (302 → `/`).
async fn callback(
    State(auth): State<Arc<AuthContext>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    // CSRF: the `state` query param must match the state cookie (double-submit).
    let cookie_state = cookie_value(&headers, OAUTH_STATE_COOKIE);
    match (q.state.as_deref(), cookie_state.as_deref()) {
        (Some(qs), Some(cs)) if !qs.is_empty() && qs == cs => {}
        _ => return reject(StatusCode::BAD_REQUEST, "invalid or missing CSRF state"),
    }

    let code = match q.code {
        Some(c) if !c.is_empty() => c,
        _ => return reject(StatusCode::BAD_REQUEST, "missing authorization code"),
    };

    // The verifier may block (network); run it off the async worker.
    let verifier = Arc::clone(&auth.verifier);
    let claims = match tokio::task::spawn_blocking(move || verifier.verify(&code)).await {
        Ok(Ok(claims)) => claims,
        Ok(Err(_)) => return reject(StatusCode::UNAUTHORIZED, "could not verify Google identity"),
        Err(_) => {
            return reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "verification task failed",
            )
        }
    };

    let now = now_secs();
    if check_claims(&claims, &auth.config.client_id, now).is_err() {
        return reject(StatusCode::UNAUTHORIZED, "ID token failed verification");
    }

    if !auth.config.is_allowed(&claims.email) {
        // A genuine Google login that isn't allowlisted is rejected — but redirect the *browser*
        // back to the SPA with `?error=forbidden` (not a raw JSON 403) so the styled allowlist
        // rejection Callout fires (QE-258 `detectRejection`). No session cookie is set.
        return redirect_to_spa("/?error=forbidden");
    }

    let exp = now.saturating_add(auth.config.session_ttl_secs);
    let token = mint_session_cookie(&auth.config.session_secret, &claims.email, exp);
    let session_cookie = set_cookie(
        SESSION_COOKIE_NAME,
        &token,
        auth.config.cookie_secure,
        auth.config.session_ttl_secs,
    );
    // Clear the one-shot state cookie.
    let clear_state = set_cookie(OAUTH_STATE_COOKIE, "", auth.config.cookie_secure, 0);

    // `AppendHeaders` so both `Set-Cookie`s survive — an array of headers would `insert` (overwrite)
    // and drop the session cookie.
    (
        StatusCode::FOUND,
        AppendHeaders([
            (header::LOCATION, "/".to_owned()),
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear_state),
        ]),
    )
        .into_response()
}

/// `GET /api/me` — the authenticated email (the session middleware guarantees one is present).
async fn me(Extension(AuthedEmail(email)): Extension<AuthedEmail>) -> Response {
    Json(json!({ "email": email })).into_response()
}

/// `GET|POST /api/auth/logout` — clear the session cookie (QE-409). Emits a `Set-Cookie` with the
/// **same name/path/attributes** and `Max-Age=0` (empty value) so the browser drops it immediately;
/// a subsequent `/api/me` then carries no valid session and is `401`. Idempotent and public (no valid
/// session required — clearing a broken/expired cookie must always succeed).
async fn logout(State(auth): State<Arc<AuthContext>>) -> Response {
    let clear = set_cookie(SESSION_COOKIE_NAME, "", auth.config.cookie_secure, 0);
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear)],
        Json(json!({ "status": "logged_out" })),
    )
        .into_response()
}

/// Session-gate middleware for the protected `/api` subtree: require a valid signed session cookie,
/// injecting the email for downstream handlers; otherwise `401`.
pub async fn require_session(
    State(auth): State<Arc<AuthContext>>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    let session = cookie_value(req.headers(), SESSION_COOKIE_NAME)
        .and_then(|t| verify_session_cookie(&auth.config.session_secret, &t, now_secs()));
    match session {
        Some(Session { email, .. }) => {
            req.extensions_mut().insert(AuthedEmail(email));
            next.run(req).await
        }
        None => reject(StatusCode::UNAUTHORIZED, "authentication required"),
    }
}

/// Assemble the protected sub-router behind [`require_session`]: `/me`, the QE-255 runs routes, the
/// QE-257 read APIs, the QE-452 Phase B pool **read** routes (session-gated), and the QE-452 Phase B
/// **governance** routes (each additionally behind a [`require_role`] seam — `roles` supplies that seam's
/// allowlists). Because the governance routes are merged **inside** this sub-router, `require_session`
/// (the outermost `route_layer`) always runs first, so the [`AuthedEmail`] the role guard reads is
/// guaranteed present.
pub fn protected_routes(auth: Arc<AuthContext>, roles: Arc<RoleConfig>) -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .merge(crate::runs::api::routes())
        .merge(crate::read::routes())
        .merge(crate::pools::read_routes())
        .merge(crate::pools::governance_routes(roles))
        .route_layer(axum::middleware::from_fn_with_state(auth, require_session))
}

/// Coarse governance roles (design §13.8). **Phase B placeholder** — a minimal, boot-resolved allowlist
/// seam. The AUTHORITATIVE enforcement (per-request server-side resolution, separation of duties — an
/// approver ≠ the launcher — dual sign-off, and the tamper-evident audit binding) is **QE-454**; this
/// only proves the seam is on the governance-route path so QE-454 can harden it in place.
#[derive(Debug, Clone, Default)]
pub struct RoleConfig {
    /// Emails allowed to launch/monitor/halt campaigns (`QE_ROLE_OPERATORS`).
    pub operators: Vec<String>,
    /// Emails allowed to approve/reject/revoke/seal pools (`QE_ROLE_APPROVERS`).
    pub approvers: Vec<String>,
}

const ENV_ROLE_OPERATORS: &str = "QE_ROLE_OPERATORS";
const ENV_ROLE_APPROVERS: &str = "QE_ROLE_APPROVERS";

impl RoleConfig {
    /// Resolve the role allowlists from the environment (both **fail-closed**: unset/blank ⇒ nobody).
    pub fn from_env() -> Self {
        Self {
            operators: parse_allowlist(&std::env::var(ENV_ROLE_OPERATORS).unwrap_or_default()),
            approvers: parse_allowlist(&std::env::var(ENV_ROLE_APPROVERS).unwrap_or_default()),
        }
    }

    /// Whether `email` holds the operator role (case-insensitive, trimmed).
    #[must_use]
    pub fn is_operator(&self, email: &str) -> bool {
        contains_email(&self.operators, email)
    }

    /// Whether `email` holds the approver role (case-insensitive, trimmed).
    #[must_use]
    pub fn is_approver(&self, email: &str) -> bool {
        contains_email(&self.approvers, email)
    }
}

/// Whether the normalized `email` is in the (already-normalized) `allowlist`.
fn contains_email(allowlist: &[String], email: &str) -> bool {
    let candidate = email.trim().to_lowercase();
    !candidate.is_empty() && allowlist.iter().any(|a| a == &candidate)
}

/// `require_role(Operator)` seam (design §13.8): gate a route on the caller (the [`AuthedEmail`] set by
/// [`require_session`]) holding the operator role, else `403`. **QE-454** replaces this with authoritative
/// RBAC + audit; here it only proves the seam sits on the governance path.
pub async fn require_operator(
    State(roles): State<Arc<RoleConfig>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    role_gate(&roles, Role::Operator, req, next).await
}

/// `require_role(Approver)` seam (design §13.8) — the approve/reject/revoke/seal governance routes. See
/// [`require_operator`]; **QE-454** adds separation-of-duties + dual sign-off + audit.
pub async fn require_approver(
    State(roles): State<Arc<RoleConfig>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    role_gate(&roles, Role::Approver, req, next).await
}

/// The two governance roles the Phase B seam distinguishes.
#[derive(Debug, Clone, Copy)]
enum Role {
    Operator,
    Approver,
}

/// Shared body of the [`require_operator`]/[`require_approver`] guards: read the request's
/// [`AuthedEmail`], check the role, and either forward or `403`. Fail-closed — a missing `AuthedEmail`
/// (should never happen inside `protected_routes`) or a role miss is refused.
async fn role_gate(
    roles: &RoleConfig,
    role: Role,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let held = req
        .extensions()
        .get::<AuthedEmail>()
        .map(|AuthedEmail(email)| match role {
            Role::Operator => roles.is_operator(email),
            Role::Approver => roles.is_approver(email),
        })
        .unwrap_or(false);
    if held {
        next.run(req).await
    } else {
        // QE-454 will replace this placeholder with authoritative RBAC + a tamper-evident audit entry.
        reject(
            StatusCode::FORBIDDEN,
            "role required — this governance action is gated on a server-side role (authoritative \
             RBAC + audit land in QE-454)",
        )
    }
}

/// Build the Google authorization-code consent URL.
fn build_auth_url(cfg: &AuthConfig, state: &str) -> String {
    match Url::parse(&cfg.auth_endpoint) {
        Ok(mut url) => {
            url.query_pairs_mut()
                .append_pair("client_id", &cfg.client_id)
                .append_pair("redirect_uri", &cfg.redirect_uri)
                .append_pair("response_type", "code")
                .append_pair("scope", "openid email")
                .append_pair("state", state)
                .append_pair("access_type", "online")
                .append_pair("prompt", "select_account");
            url.to_string()
        }
        // A misconfigured endpoint shouldn't panic; login just won't reach Google.
        Err(_) => cfg.auth_endpoint.clone(),
    }
}

/// Extract a single cookie value by name from the request `Cookie` header(s).
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for hv in headers.get_all(header::COOKIE) {
        let Ok(s) = hv.to_str() else { continue };
        for part in s.split(';') {
            if let Some(v) = part.trim().strip_prefix(&prefix) {
                return Some(v.to_owned());
            }
        }
    }
    None
}

/// A JSON error response with `status` + message.
fn reject(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

/// A `302` redirect back to the same-origin SPA at `location` (e.g. `/?error=forbidden`). Used for
/// the allowlist-reject path so the browser lands back on the app and the styled rejection UI fires,
/// rather than seeing a raw JSON body.
fn redirect_to_spa(location: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, location.to_owned())]).into_response()
}

/// The first set, non-empty value among `keys`, or empty string.
fn env_first(keys: &[&str]) -> String {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    String::new()
}

/// Wall-clock now, epoch seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> GoogleClaims {
        GoogleClaims {
            email: "admin@example.com".to_owned(),
            email_verified: true,
            aud: "client-123".to_owned(),
            iss: "https://accounts.google.com".to_owned(),
            exp: 2_000,
        }
    }

    #[test]
    fn check_claims_accepts_a_good_token() {
        assert!(check_claims(&claims(), "client-123", 1_000).is_ok());
        // The bare (non-https) issuer is also accepted.
        let mut c = claims();
        c.iss = "accounts.google.com".to_owned();
        assert!(check_claims(&c, "client-123", 1_000).is_ok());
    }

    #[test]
    fn check_claims_rejects_wrong_aud() {
        assert_eq!(
            check_claims(&claims(), "someone-elses-client", 1_000),
            Err(ClaimRejection::Audience)
        );
    }

    #[test]
    fn check_claims_rejects_wrong_iss() {
        let mut c = claims();
        c.iss = "https://evil.example.com".to_owned();
        assert_eq!(
            check_claims(&c, "client-123", 1_000),
            Err(ClaimRejection::Issuer)
        );
    }

    #[test]
    fn check_claims_rejects_expired() {
        assert_eq!(
            check_claims(&claims(), "client-123", 2_000),
            Err(ClaimRejection::Expired)
        );
        assert_eq!(
            check_claims(&claims(), "client-123", 2_001),
            Err(ClaimRejection::Expired)
        );
    }

    #[test]
    fn check_claims_rejects_unverified_email() {
        let mut c = claims();
        c.email_verified = false;
        assert_eq!(
            check_claims(&c, "client-123", 1_000),
            Err(ClaimRejection::EmailUnverified)
        );
    }

    #[test]
    fn allowlist_is_trimmed_and_case_insensitive() {
        let cfg = AuthConfig {
            allowed_emails: parse_allowlist("  Admin@Example.com , other@x.io "),
            ..test_config()
        };
        assert!(cfg.is_allowed("admin@example.com"));
        assert!(cfg.is_allowed("ADMIN@EXAMPLE.COM"));
        assert!(cfg.is_allowed("  other@x.io  "));
        assert!(!cfg.is_allowed("nope@example.com"));
    }

    #[test]
    fn empty_allowlist_fails_closed() {
        let cfg = AuthConfig {
            allowed_emails: parse_allowlist("   ,  "),
            ..test_config()
        };
        assert!(cfg.allowed_emails.is_empty());
        assert!(!cfg.is_allowed("admin@example.com"));
        assert!(!cfg.is_allowed(""));
    }

    fn test_config() -> AuthConfig {
        AuthConfig {
            client_id: "client-123".to_owned(),
            client_secret: String::new(),
            redirect_uri: "https://app.example.com/api/auth/callback".to_owned(),
            auth_endpoint: DEFAULT_AUTH_ENDPOINT.to_owned(),
            token_endpoint: DEFAULT_TOKEN_ENDPOINT.to_owned(),
            tokeninfo_endpoint: DEFAULT_TOKENINFO_ENDPOINT.to_owned(),
            allowed_emails: Vec::new(),
            session_secret: b"secret".to_vec(),
            session_secret_is_ephemeral: false,
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            cookie_secure: true,
        }
    }

    #[test]
    fn build_auth_url_has_the_expected_params() {
        let url = build_auth_url(&test_config(), "state-xyz");
        assert!(url.starts_with(DEFAULT_AUTH_ENDPOINT));
        assert!(url.contains("client_id=client-123"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("scope=openid+email") || url.contains("scope=openid%20email"));
        assert!(url.contains("state=state-xyz"));
        // redirect_uri is percent-encoded.
        assert!(url.contains("redirect_uri=https"));
    }

    #[test]
    fn cookie_value_parses_from_a_multi_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "foo=1; qe_session=abc.def; bar=2".parse().unwrap(),
        );
        assert_eq!(
            cookie_value(&headers, "qe_session"),
            Some("abc.def".to_owned())
        );
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[test]
    fn disabled_verifier_errors() {
        assert!(matches!(
            DisabledVerifier.verify("code"),
            Err(VerifyError::Unsupported)
        ));
    }

    #[test]
    fn cookie_secure_follows_scheme() {
        // https deployment ⇒ Secure.
        assert!(cookie_secure_for(
            "https://app.example.com/api/auth/callback"
        ));
        assert!(cookie_secure_for("HTTPS://APP.EXAMPLE.COM/cb")); // case-insensitive scheme
                                                                  // loopback / http dev, and the empty default (OAuth unconfigured) ⇒ NOT Secure, so the cookie
                                                                  // survives over http://127.0.0.1 and default-address dev login persists.
        assert!(!cookie_secure_for(
            "http://127.0.0.1:8080/api/auth/callback"
        ));
        assert!(!cookie_secure_for("http://localhost:8080/cb"));
        assert!(!cookie_secure_for(""));
        assert!(!cookie_secure_for("not-a-url"));
    }

    #[test]
    fn set_cookie_keeps_httponly_and_samesite_and_conditions_secure() {
        let secure = set_cookie(SESSION_COOKIE_NAME, "tok", true, 100);
        assert!(secure.contains("HttpOnly"));
        assert!(secure.contains("SameSite=Lax"));
        assert!(secure.contains("Path=/"));
        assert!(secure.contains("Secure"));
        assert!(secure.contains("Max-Age=100"));

        let insecure = set_cookie(SESSION_COOKIE_NAME, "tok", false, 0);
        assert!(insecure.contains("HttpOnly"));
        assert!(insecure.contains("SameSite=Lax"));
        assert!(!insecure.contains("Secure"), "no Secure over http dev");
        assert!(insecure.contains("Max-Age=0"));
    }

    #[test]
    fn session_secret_policy_is_fail_closed_off_loopback() {
        let loopback: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let loopback_v6: SocketAddr = "[::1]:8080".parse().unwrap();
        let routable: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let public: SocketAddr = "10.0.0.5:8080".parse().unwrap();

        // Ephemeral secret is safe on loopback (AR-9) …
        assert!(check_session_secret_policy(&loopback, true).is_ok());
        assert!(check_session_secret_policy(&loopback_v6, true).is_ok());
        // … but a non-loopback bind with an ephemeral secret is REFUSED.
        assert!(check_session_secret_policy(&routable, true).is_err());
        assert!(check_session_secret_policy(&public, true).is_err());
        // An explicit (persistent) secret is fine on any bind.
        assert!(check_session_secret_policy(&routable, false).is_ok());
        assert!(check_session_secret_policy(&public, false).is_ok());
        assert!(check_session_secret_policy(&loopback, false).is_ok());
    }

    #[test]
    fn warns_only_for_non_loopback_insecure_cookies() {
        let loopback: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let loopback_v6: SocketAddr = "[::1]:8080".parse().unwrap();
        let routable: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let public: SocketAddr = "10.0.0.5:8080".parse().unwrap();

        // The one case that warrants an advisory warning: off-loopback AND cookies not Secure.
        assert!(should_warn_insecure_cookies(&routable, false));
        assert!(should_warn_insecure_cookies(&public, false));
        // Loopback dev is expected to serve http without Secure — no warning.
        assert!(!should_warn_insecure_cookies(&loopback, false));
        assert!(!should_warn_insecure_cookies(&loopback_v6, false));
        // Secure cookies off-loopback (https deploy) is the correct config — no warning.
        assert!(!should_warn_insecure_cookies(&routable, true));
        assert!(!should_warn_insecure_cookies(&public, true));
        assert!(!should_warn_insecure_cookies(&loopback, true));
    }
}
