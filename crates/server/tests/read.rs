//! QE-257 read-API acceptance tests (`#[tokio::test]`, `tower::oneshot`, no network).
//!
//! Both endpoints are session-gated (QE-256): a valid session ⇒ the fixture data; no session ⇒ `401`.
//!
//! Fixtures are the committed QE-251 samples copied into `tests/fixtures/` (`qe-server` can't depend on
//! `qe-cli`, so the fixtures are duplicated here rather than reached across the crate boundary; a
//! sealed vintage is expensive to construct in-code, so copying is the cleanest hermetic option):
//! - `sample_store/` — BTCUSDT / 1h / 120 bars (copied into a tempdir before opening, so the read-only
//!   fixture is never mutated by the store's schema-init write txn);
//! - `sample_vintage.json` — one sealed vintage (`vintage_id = "sample_vintage"`), read in place.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use qe_server::{build_router, CliJobSpawner, ReadState, RunManager};
use qe_storage::{MarketStore, DEFAULT_MAP_SIZE};
use qe_vintage::VintageRepository;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

/// The allowlisted email these tests authenticate as.
const TEST_EMAIL: &str = "reader@example.com";

/// 2021-01-01T00:00:00Z in epoch-ms — the sample store's first bar (matches `cli/tests/ingest_job.rs`).
const START_MS: i64 = 18_628 * 86_400_000;
const HOUR_MS: i64 = 3_600_000;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the committed sample store into `tmp` so opening it (a schema-init write txn) never mutates the
/// read-only fixture. Mirrors `cli/tests/ingest_job.rs::copy_store_to`.
fn copy_store_to(tmp: &Path) -> PathBuf {
    let src = fixtures_dir().join("sample_store");
    let dst = tmp.join("sample_store");
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    dst
}

/// Build the router over the fixtures: the artifacts dir is the read-only `tests/fixtures/` (holding
/// `sample_vintage.json`); the market store is a private copy of `sample_store/` under `tmp`.
fn build_app(tmp: &TempDir) -> Router {
    let store_path = copy_store_to(tmp.path());
    let market_store = Arc::new(MarketStore::open(&store_path, DEFAULT_MAP_SIZE).unwrap());
    let vintages = VintageRepository::new(fixtures_dir());
    let read = Arc::new(ReadState::new(vintages, market_store));

    // A run manager is required to assemble AppState but is never exercised here.
    let spawner = Arc::new(CliJobSpawner::new(PathBuf::from("qe")));
    let manager = Arc::new(RunManager::new(tmp.path().join("runs"), spawner, 2));
    let auth = common::auth_context(TEST_EMAIL, None);

    build_router(
        &tmp.path().join("static"),
        common::app_state_with_read(manager, auth, read),
    )
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get_authed(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", common::session_cookie_header(TEST_EMAIL))
        .body(Body::empty())
        .unwrap()
}

fn get_no_session(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn vintages_returns_the_fixture_vintage_with_a_session() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, body) = send(&app, get_authed("/api/vintages")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    let arr = body.as_array().expect("vintages is an array");
    assert_eq!(arr.len(), 1, "exactly the one fixture vintage: {body}");
    let v = &arr[0];
    assert_eq!(v["id"], "sample_vintage");
    assert_eq!(v["label"], "sample_vintage");
    assert!(
        v["summary"]["chromosomes"].as_u64().unwrap_or(0) >= 1,
        "summary reports at least one chromosome: {v}"
    );
    assert!(
        v["summary"]["content_hash"]
            .as_str()
            .is_some_and(|h| h.len() == 64),
        "summary carries the 64-hex content hash: {v}"
    );
    assert!(
        v["summary"]["format_version"].is_u64(),
        "summary carries the format version: {v}"
    );
}

#[tokio::test]
async fn coverage_returns_the_sample_store_rows_with_a_session() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, body) = send(&app, get_authed("/api/market-data/coverage")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    assert_eq!(
        body,
        serde_json::json!([{
            "symbol": "BTCUSDT",
            "resolution": "1h",
            "from": START_MS,
            "to": START_MS + 119 * HOUR_MS,
            "bars": 120,
        }]),
        "coverage over the committed sample store diverged"
    );
}

#[tokio::test]
async fn both_read_endpoints_require_a_session() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (vintages, _) = send(&app, get_no_session("/api/vintages")).await;
    assert_eq!(vintages, StatusCode::UNAUTHORIZED, "no session ⇒ 401");

    let (coverage, _) = send(&app, get_no_session("/api/market-data/coverage")).await;
    assert_eq!(coverage, StatusCode::UNAUTHORIZED, "no session ⇒ 401");
}
