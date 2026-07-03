//! QE-254 async integration test: boot the router and drive it in-process with
//! `tower::ServiceExt::oneshot` (no real network bind) to assert the health endpoint and static-index
//! serving behave, and that the reserved `/api` namespace returns `404` for unknown routes.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use qe_server::build_router;
use tower::ServiceExt; // for `oneshot`

const INDEX_HTML: &str = "<!doctype html><title>test-spa</title><h1>test index</h1>";

/// A temp static dir containing a known `index.html`, so `/` serving is deterministic and independent
/// of the committed placeholder.
fn temp_static_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");
    std::fs::write(dir.path().join("index.html"), INDEX_HTML).expect("write index.html");
    dir
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

#[tokio::test]
async fn health_endpoint_returns_ok_json() {
    let dir = temp_static_dir();
    let app = build_router(dir.path());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    let json: serde_json::Value = serde_json::from_str(&body).expect("health body is JSON");
    assert_eq!(json, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn root_serves_static_index() {
    let dir = temp_static_dir();
    let app = build_router(dir.path());

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert_eq!(body, INDEX_HTML);
}

#[tokio::test]
async fn unknown_client_route_falls_back_to_index() {
    // SPA client-side routing: a deep link the server has no file for still returns the app shell,
    // not a 404 — so the browser can hydrate and route client-side.
    let dir = temp_static_dir();
    let app = build_router(dir.path());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/backtests/abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, INDEX_HTML);
}

#[tokio::test]
async fn unknown_api_route_is_reserved_404() {
    // `/api` is a reserved JSON namespace: an unknown `/api/*` path must NOT be swallowed by the SPA
    // fallback — it returns 404 so later tickets can add real routes without ambiguity.
    let dir = temp_static_dir();
    let app = build_router(dir.path());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
