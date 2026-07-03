//! QE-256 auth acceptance + negative-security tests (`#[tokio::test]`, `tower::oneshot`, no network).
//!
//! The OAuth verifier is **mocked** via `common::MockVerifier`, so every path — a valid allowlisted
//! login, a non-allowlisted login, a rejected token, a tampered session, a CSRF mismatch — is
//! exercised deterministically without talking to Google.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use qe_server::auth::GoogleClaims;
use qe_server::{build_router, CliJobSpawner, RunManager};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

/// Build the app with a given allowlist + mocked verifier outcome. The run manager is present but the
/// auth tests never spawn a job. Returns the owning [`TempDir`] alongside the router: all throwaway
/// state (the runs dir + the read-state store) lives under it, so it is cleaned up when the caller
/// drops the guard — nothing leaks into the temp dir across the suite.
fn app(allowlist: &str, verifier_outcome: Option<GoogleClaims>) -> (Router, TempDir) {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let spawner = Arc::new(CliJobSpawner::new(PathBuf::from("qe")));
    let manager = Arc::new(RunManager::new(tmp.path().join("runs"), spawner, 2));
    let auth = common::auth_context(allowlist, verifier_outcome);
    // Static dir irrelevant (no `/` requests here).
    let router = build_router(
        &tmp.path().join("static"),
        common::app_state_under(manager, auth, tmp.path()),
    );
    (router, tmp)
}

async fn send(app: &Router, req: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(req).await.expect("router responds")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap()
}

/// A callback request with matching CSRF `state` in both the query and the state cookie (double-submit).
fn callback(code: &str, state: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/api/auth/callback?code={code}&state={state}"))
        .header("cookie", format!("qe_oauth_state={state}"))
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Pull the `qe_session=…` cookie value from a response's `Set-Cookie` headers.
fn session_cookie_from(resp: &axum::response::Response) -> Option<String> {
    for hv in resp.headers().get_all(header::SET_COOKIE) {
        let s = hv.to_str().ok()?;
        if let Some(rest) = s.strip_prefix("qe_session=") {
            let value = rest.split(';').next().unwrap_or("");
            return Some(format!("qe_session={value}"));
        }
    }
    None
}

// ---- acceptance criteria ------------------------------------------------------------------------

#[tokio::test]
async fn no_session_is_401_on_me_and_on_a_protected_run_route() {
    let (app, _tmp) = app("admin@example.com", None);

    let me = send(&app, get("/api/me")).await;
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);

    let runs = send(&app, get("/api/runs")).await;
    assert_eq!(
        runs.status(),
        StatusCode::UNAUTHORIZED,
        "QE-255 runs routes are now gated"
    );
}

#[tokio::test]
async fn allowlisted_login_sets_a_session_and_me_returns_the_email() {
    let email = "admin@example.com";
    let (app, _tmp) = app(email, Some(common::valid_claims(email)));

    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(
        resp.status(),
        StatusCode::FOUND,
        "callback redirects on success"
    );
    let cookie = session_cookie_from(&resp).expect("a session cookie is set");

    let me = send(&app, get_with_cookie("/api/me", &cookie)).await;
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(json_body(me).await, serde_json::json!({ "email": email }));
}

#[tokio::test]
async fn valid_login_not_on_allowlist_redirects_to_spa_with_error() {
    // A genuine Google login (verifier succeeds, claims pass policy) but the email isn't allowlisted.
    // QE-259: instead of a raw JSON 403, the browser is redirected back to the SPA with
    // `?error=forbidden` so the styled allowlist-rejection Callout fires. Still no session cookie.
    let (app, _tmp) = app(
        "admin@example.com",
        Some(common::valid_claims("intruder@example.com")),
    );
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(
        resp.status(),
        StatusCode::FOUND,
        "rejection redirects the browser back to the SPA"
    );
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert_eq!(location, "/?error=forbidden");
    assert!(
        session_cookie_from(&resp).is_none(),
        "no session on rejection"
    );
}

// ---- negative security --------------------------------------------------------------------------

#[tokio::test]
async fn tampered_session_cookie_is_401() {
    let (app, _tmp) = app("admin@example.com", None);

    // A structurally-plausible but unsigned/forged token.
    let forged = "qe_session=djF8YWRtaW5AZXhhbXBsZS5jb218OTk5OTk5OTk5OQ.not-a-valid-mac";
    let resp = send(&app, get_with_cookie("/api/me", forged)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A valid token whose payload has been altered but the signature kept.
    let good = common::valid_session_token("admin@example.com");
    let sig = good.split_once('.').unwrap().1;
    let forged2 = format!("qe_session=dGFtcGVyZWQ.{sig}");
    let resp2 = send(&app, get_with_cookie("/api/me", &forged2)).await;
    assert_eq!(resp2.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_aud_token_is_rejected() {
    let mut claims = common::valid_claims("admin@example.com");
    claims.aud = "some-other-client".to_owned();
    let (app, _tmp) = app("admin@example.com", Some(claims));
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(session_cookie_from(&resp).is_none());
}

#[tokio::test]
async fn wrong_iss_token_is_rejected() {
    let mut claims = common::valid_claims("admin@example.com");
    claims.iss = "https://evil.example.com".to_owned();
    let (app, _tmp) = app("admin@example.com", Some(claims));
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let mut claims = common::valid_claims("admin@example.com");
    claims.exp = 1; // long in the past
    let (app, _tmp) = app("admin@example.com", Some(claims));
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unverified_email_token_is_rejected() {
    let mut claims = common::valid_claims("admin@example.com");
    claims.email_verified = false;
    let (app, _tmp) = app("admin@example.com", Some(claims));
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verifier_failure_is_rejected() {
    // Verifier returns an error (e.g. token exchange failed) ⇒ not signed in.
    let (app, _tmp) = app("admin@example.com", None);
    let resp = send(&app, callback("auth-code", "state-1")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn csrf_state_mismatch_is_rejected() {
    let (app, _tmp) = app(
        "admin@example.com",
        Some(common::valid_claims("admin@example.com")),
    );

    // Query `state` differs from the state cookie.
    let mismatched = Request::builder()
        .uri("/api/auth/callback?code=auth-code&state=attacker-state")
        .header("cookie", "qe_oauth_state=the-real-state")
        .body(Body::empty())
        .unwrap();
    let resp = send(&app, mismatched).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Missing state cookie entirely.
    let no_cookie = Request::builder()
        .uri("/api/auth/callback?code=auth-code&state=s")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, no_cookie).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn missing_code_is_rejected() {
    let (app, _tmp) = app(
        "admin@example.com",
        Some(common::valid_claims("admin@example.com")),
    );
    let no_code = Request::builder()
        .uri("/api/auth/callback?state=s")
        .header("cookie", "qe_oauth_state=s")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, no_code).await.status(), StatusCode::BAD_REQUEST);
}

// ---- login entry ---------------------------------------------------------------------------------

#[tokio::test]
async fn login_redirects_to_google_with_state_cookie() {
    let (app, _tmp) = app("admin@example.com", None);
    let resp = send(&app, get("/api/auth/login")).await;
    assert_eq!(resp.status(), StatusCode::FOUND);

    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert!(location.starts_with("https://accounts.google.com/o/oauth2/v2/auth"));
    assert!(location.contains("client_id="));
    assert!(location.contains("response_type=code"));
    assert!(location.contains("state="));

    let set_cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|s| s.starts_with("qe_oauth_state=") && s.contains("HttpOnly"));
    assert!(set_cookie, "a HttpOnly state cookie is set");
}

#[tokio::test]
async fn health_stays_public() {
    let (app, _tmp) = app("admin@example.com", None);
    let resp = send(&app, get("/api/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
