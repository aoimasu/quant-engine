//! QE-425 router-hardening integration tests: the explicit body cap on `POST /api/runs` (413), and a
//! confirmation the added transport layers leave the happy path (health + a normal `/api` GET) intact.
//!
//! Driven in-process with `tower::ServiceExt::oneshot` (no network bind), through the **production**
//! [`build_router`] so the real layer wiring is exercised. The per-request-timeout `408` is covered by
//! the `lib.rs` unit test `api_timeout_layer_returns_408_for_a_slow_handler` (the exact `TimeoutLayer`
//! type, with a tiny deadline — a real 30s end-to-end deadline is untestable in-process).

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::Path;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use qe_server::{build_router, AppState, CliJobSpawner, RunManager};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`

mod common;

/// The allowlisted email these tests authenticate as (QE-256 gates all `/api/runs*` behind a session).
const TEST_EMAIL: &str = "runner@example.com";

/// Exceeds the 256 KiB `RUN_SPEC_BODY_LIMIT` cap on `POST /api/runs`.
const OVER_LIMIT_BYTES: usize = 300 * 1024;

/// A [`RunManager`] rooted under `base` (a caller-owned temp dir). The body-cap tests never create a
/// run (an over-limit body is rejected before the handler; the within-limit body is intentionally
/// invalid), so the spawner binary is never invoked.
fn test_manager(base: &Path) -> Arc<RunManager> {
    let spawner = Arc::new(CliJobSpawner::new(std::path::PathBuf::from("qe")));
    Arc::new(RunManager::new(base.join("runs"), spawner, 2))
}

/// An [`AppState`] whose auth context authenticates the `TEST_EMAIL` session cookie, rooted under
/// `base` so everything is cleaned up on drop.
fn test_state(base: &Path) -> AppState {
    common::app_state_under(
        test_manager(base),
        common::auth_context(TEST_EMAIL, None),
        base,
    )
}

async fn read_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Authenticated `POST /api/runs` carrying `body` (raw bytes).
fn post_runs(body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/runs")
        .header("content-type", "application/json")
        .header("cookie", common::session_cookie_header(TEST_EMAIL))
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn over_limit_body_on_post_runs_is_413() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(&dir.path().join("static"), test_state(dir.path()));

    // A syntactically-valid run spec whose `label` field is padded past the 256 KiB cap. Without the
    // cap this body would deserialize + reach `create_run`; with it, the request is short-circuited to
    // 413 before the handler runs.
    let padding = "x".repeat(OVER_LIMIT_BYTES);
    let body = json!({
        "type": "backtest",
        "params": {
            "vintage": "sample_vintage",
            "start": "2021-01-01",
            "end": "2021-02-01",
            "resolution": "1h",
            "universe": ["BTCUSDT"],
            "pad": padding
        }
    })
    .to_string()
    .into_bytes();
    assert!(
        body.len() > 256 * 1024,
        "the test body must exceed the cap to be non-vacuous (len={})",
        body.len()
    );

    let response = app.oneshot(post_runs(body)).await.expect("router responds");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn within_limit_body_on_post_runs_reaches_the_handler() {
    // Non-vacuous companion to the 413 test: a normal-sized body is NOT rejected by the cap — it
    // reaches `create_run`, which rejects the unknown run type with a `400` validation error (proving
    // the request passed the body-limit layer rather than being short-circuited to 413).
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(&dir.path().join("static"), test_state(dir.path()));

    let body = json!({ "type": "not-a-real-type", "params": {} })
        .to_string()
        .into_bytes();
    assert!(
        body.len() < 256 * 1024,
        "control body must be within the cap"
    );

    let (status, value) =
        read_json(app.oneshot(post_runs(body)).await.expect("router responds")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "reached the handler, not a 413"
    );
    assert!(
        value.get("error").is_some(),
        "expected a validation error body, got {value}"
    );
}

#[tokio::test]
async fn happy_path_is_intact_health_and_normal_get() {
    // The added layers must not break the happy path. `GET /api/health` (routed OUTSIDE the timeout
    // group) and a normal authenticated `GET /api/runs` (an empty store) both still return 200.
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(&dir.path().join("static"), test_state(dir.path()));

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    assert_eq!(health.status(), StatusCode::OK);

    let (status, value) = read_json(
        app.oneshot(
            Request::builder()
                .uri("/api/runs")
                .header("cookie", common::session_cookie_header(TEST_EMAIL))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value["runs"],
        json!([]),
        "empty store lists no runs, got {value}"
    );
}
