//! Shared test helpers (QE-256): a mock ID-token verifier, a test [`AppState`] builder, and a
//! session-cookie minter that authenticates through the **same** production signing code
//! (`qe_server::mint_session_cookie`) — so tests never fork a parallel signing implementation.
#![allow(dead_code)] // Each integration-test binary uses a different subset of these helpers.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use qe_server::auth::{
    cookie_secure_for, parse_allowlist, AuthConfig, AuthContext, GoogleClaims, IdTokenVerifier,
    VerifyError, DEFAULT_AUTH_ENDPOINT, DEFAULT_SESSION_TTL_SECS, DEFAULT_TOKENINFO_ENDPOINT,
    DEFAULT_TOKEN_ENDPOINT, SESSION_COOKIE_NAME,
};
use qe_server::{mint_session_cookie, AppState, ReadState, RunManager};
use qe_storage::{MarketStore, DEFAULT_MAP_SIZE};
use qe_vintage::VintageRepository;

/// Fixed session HMAC key for tests.
pub const SESSION_SECRET: &[u8] = b"integration-test-session-secret-0123456789";
/// The OAuth client id tests expect as the ID-token `aud`.
pub const CLIENT_ID: &str = "test-client-id.apps.googleusercontent.com";

/// A verifier that returns a pre-set outcome regardless of the `code` — the injected seam for
/// hermetic auth tests (no network, no real Google keys).
pub struct MockVerifier {
    /// `Some(claims)` ⇒ the verifier "succeeds" with these raw claims; `None` ⇒ it fails.
    pub outcome: Option<GoogleClaims>,
}

impl IdTokenVerifier for MockVerifier {
    fn verify(&self, _code: &str) -> Result<GoogleClaims, VerifyError> {
        self.outcome
            .clone()
            .ok_or_else(|| VerifyError::Upstream("mock verifier: forced failure".to_owned()))
    }
}

/// Epoch seconds, now.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A well-formed, policy-passing claim set for `email` (correct `aud`/`iss`, verified, not expired).
pub fn valid_claims(email: &str) -> GoogleClaims {
    GoogleClaims {
        email: email.to_owned(),
        email_verified: true,
        aud: CLIENT_ID.to_owned(),
        iss: "https://accounts.google.com".to_owned(),
        exp: now_secs() + 3600,
    }
}

/// Build an [`AuthConfig`] with the fixed test secret, the given comma-separated allowlist, and the
/// test client id. The `redirect_uri` is an `https` URL, so `cookie_secure` is `true` (production-like).
pub fn auth_config(allowlist: &str) -> AuthConfig {
    auth_config_with_redirect(allowlist, "https://app.test/api/auth/callback")
}

/// Like [`auth_config`] but with an explicit `redirect_uri`, so a test can exercise the QE-409 cookie
/// `Secure` conditionality (https ⇒ `Secure`; `http://127.0.0.1` loopback dev ⇒ no `Secure`).
pub fn auth_config_with_redirect(allowlist: &str, redirect_uri: &str) -> AuthConfig {
    AuthConfig {
        client_id: CLIENT_ID.to_owned(),
        client_secret: "test-secret".to_owned(),
        redirect_uri: redirect_uri.to_owned(),
        auth_endpoint: DEFAULT_AUTH_ENDPOINT.to_owned(),
        token_endpoint: DEFAULT_TOKEN_ENDPOINT.to_owned(),
        tokeninfo_endpoint: DEFAULT_TOKENINFO_ENDPOINT.to_owned(),
        allowed_emails: parse_allowlist(allowlist),
        session_secret: SESSION_SECRET.to_vec(),
        session_secret_is_ephemeral: false,
        session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
        cookie_secure: cookie_secure_for(redirect_uri),
    }
}

/// Build an [`AuthContext`] from an allowlist + a mock verifier outcome.
pub fn auth_context(allowlist: &str, verifier_outcome: Option<GoogleClaims>) -> Arc<AuthContext> {
    auth_context_with_redirect(
        allowlist,
        "https://app.test/api/auth/callback",
        verifier_outcome,
    )
}

/// Build an [`AuthContext`] with an explicit `redirect_uri` (drives the cookie-`Secure` rule).
pub fn auth_context_with_redirect(
    allowlist: &str,
    redirect_uri: &str,
    verifier_outcome: Option<GoogleClaims>,
) -> Arc<AuthContext> {
    Arc::new(AuthContext::new(
        auth_config_with_redirect(allowlist, redirect_uri),
        Arc::new(MockVerifier {
            outcome: verifier_outcome,
        }),
    ))
}

/// A throwaway [`ReadState`] (empty market store + empty artifacts dir) rooted **under `base`** — a
/// directory the caller already owns (typically the test's [`tempfile::TempDir`]). Keeping the store
/// under a caller-owned dir means it is cleaned up when that dir is dropped, so no temp dir leaks, and
/// distinct callers never collide (each passes its own unique base ⇒ no double-open of one LMDB path).
/// For tests that don't exercise the QE-257 read endpoints (auth / runs / http).
pub fn empty_read_state_under(base: &Path) -> Arc<ReadState> {
    let market = base.join("read-market");
    std::fs::create_dir_all(&market).expect("create market dir");
    let store = Arc::new(MarketStore::open(&market, DEFAULT_MAP_SIZE).expect("open market store"));
    let vintages = VintageRepository::new(base.join("read-artifacts"));
    Arc::new(ReadState::new(vintages, store))
}

/// Assemble an [`AppState`] from a run manager + an auth context, with a throwaway empty read state
/// rooted under `base` (see [`empty_read_state_under`]).
pub fn app_state_under(manager: Arc<RunManager>, auth: Arc<AuthContext>, base: &Path) -> AppState {
    AppState::new(manager, auth, empty_read_state_under(base))
}

/// Assemble an [`AppState`] with an explicit [`ReadState`] — for the QE-257 read-endpoint tests.
pub fn app_state_with_read(
    manager: Arc<RunManager>,
    auth: Arc<AuthContext>,
    read: Arc<ReadState>,
) -> AppState {
    AppState::new(manager, auth, read)
}

/// Mint a valid session cookie **value** for `email`, expiring an hour out, via the production signer.
pub fn valid_session_token(email: &str) -> String {
    mint_session_cookie(SESSION_SECRET, email, now_secs() + 3600)
}

/// A `Cookie` header value carrying a valid session for `email`.
pub fn session_cookie_header(email: &str) -> String {
    format!("{SESSION_COOKIE_NAME}={}", valid_session_token(email))
}
