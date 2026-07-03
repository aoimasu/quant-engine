//! QE-255 run-lifecycle integration tests (`#[tokio::test]`), hermetic and deterministic.
//!
//! Strategy (ADR D4c spawn seam): the production [`CliJobSpawner`] builds the real
//! `backtest … --run-dir <dir> --json` argv and spawns it; these tests only swap the *binary* for a
//! generated `/bin/sh` fake job. So the real arg-building + real subprocess supervision (tailing
//! stdout progress into `meta.json`, capturing a stderr tail, the bounded worker pool) are exercised
//! end-to-end — no globally-installed binary, no building `qe-cli`. Status is polled with a bounded
//! timeout (no fixed sleeps) to avoid flakes.
//!
//! Unix-only: the fake job is a POSIX shell script (dev + CI are macOS/Linux).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use qe_server::{build_router, CliJobSpawner, RunManager};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::time::Instant;
use tower::ServiceExt;

mod common;

/// The allowlisted email these tests authenticate as (QE-256 gates all `/api/runs*` behind a session).
const TEST_EMAIL: &str = "runner@example.com";

/// Write `body` as an executable `/bin/sh` script at `path`.
fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    let mut perms = std::fs::metadata(path).expect("stat script").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod script");
}

/// Build the router over `data_dir/runs` with a [`CliJobSpawner`] pointed at `script`, and a pool of
/// `max_concurrency`.
fn app_with_script(data_dir: &Path, script: &Path, max_concurrency: usize) -> Router {
    let spawner = Arc::new(CliJobSpawner::new(script.to_path_buf()));
    let manager = Arc::new(RunManager::new(
        data_dir.join("runs"),
        spawner,
        max_concurrency,
    ));
    // QE-256 gates `/api/runs*` behind a session; wire an auth context whose session secret matches
    // the one the test's `Cookie` header is signed with (both from `common`).
    let auth = common::auth_context(TEST_EMAIL, None);
    // The static dir is irrelevant here (no `/` requests); an absent path is fine.
    build_router(
        &data_dir.join("static"),
        common::app_state_under(manager, auth, data_dir),
    )
}

/// A create-run request body for a minimal valid backtest.
fn create_body() -> Value {
    json!({
        "type": "backtest",
        "params": {
            "vintage": "sample_vintage",
            "start": "2021-01-01",
            "end": "2021-02-01",
            "resolution": "1h",
            "universe": ["BTCUSDT"]
        }
    })
}

async fn read_json(resp: axum::response::Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", common::session_cookie_header(TEST_EMAIL))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

async fn post_run(app: &Router, body: &Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("content-type", "application/json")
                .header("cookie", common::session_cookie_header(TEST_EMAIL))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

/// Poll `GET /api/runs/{id}` until `status == want` or the timeout elapses; returns the final meta.
async fn poll_status(app: &Router, id: &str, want: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let (_, meta) = get(app, &format!("/api/runs/{id}")).await;
        if meta["status"] == want {
            return meta;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for status `{want}`; last meta = {meta}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// Full AC transition `queued → running → succeeded`, with an **observed** `running` snapshot.
///
/// The fake job emits a progress line then **blocks until a release sentinel appears**, so `running`
/// is deterministically observable — the job cannot finish before we poll it (no race). We then
/// release it, confirm `succeeded`, and confirm `/result` serves the artefact.
#[tokio::test]
async fn run_transitions_running_then_succeeds_and_serves_result() {
    let tmp = TempDir::new().unwrap();
    let release = tmp.path().join("release");
    let script = tmp.path().join("job_ok.sh");
    write_script(
        &script,
        &format!(
            r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{{"t":"progress","pct":10,"stage":"load","msg":"loading"}}\n'
# Block in `running` until released, so the test can observe the running snapshot deterministically.
while [ ! -f "{release}" ]; do sleep 0.02; done
printf '{{"t":"progress","pct":80,"stage":"simulate","msg":"simulating"}}\n'
printf '{{"ok":true}}' > "$run_dir/result.json"
printf '{{"t":"done","result":"result.json"}}\n'
exit 0
"#,
            release = release.display()
        ),
    );
    let app = app_with_script(tmp.path(), &script, 2);

    let (status, body) = post_run(&app, &create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create returned {body}");
    let id = body["id"].as_str().expect("id in response").to_owned();

    // Observe the `running` snapshot while the job is blocked (deterministic — it can't finish yet).
    let running = poll_status(&app, &id, "running", TIMEOUT).await;
    assert_eq!(running["status"], json!("running"));
    assert!(
        running["started_ms"].is_u64(),
        "started while running: {running}"
    );
    assert!(
        running["finished_ms"].is_null(),
        "not yet finished while running: {running}"
    );

    // Release the job → it finishes.
    std::fs::write(&release, b"go").unwrap();

    let meta = poll_status(&app, &id, "succeeded", TIMEOUT).await;
    assert!(meta["started_ms"].is_u64(), "started_ms set: {meta}");
    assert!(meta["finished_ms"].is_u64(), "finished_ms set: {meta}");
    assert_eq!(meta["exit"], json!(0));
    // Progress was tailed from the subprocess stdout (last line before `done`).
    assert_eq!(meta["progress"]["pct"], json!(80));
    assert_eq!(meta["progress"]["stage"], json!("simulate"));
    assert_eq!(meta["artifacts"], json!(["result.json"]));

    // Result endpoint serves the exact bytes the job wrote.
    let (rstatus, rbody) = get(&app, &format!("/api/runs/{id}/result")).await;
    assert_eq!(rstatus, StatusCode::OK);
    assert_eq!(rbody, json!({ "ok": true }));

    // List shows the run.
    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(lstatus, StatusCode::OK);
    let arr = list.as_array().expect("list is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], json!(id));
    assert_eq!(arr[0]["status"], json!("succeeded"));
}

/// Failure path: a non-zero exit ⇒ `failed`, with the captured stderr tail as the error; the result
/// endpoint reports `409` (no result available).
#[tokio::test]
async fn run_fails_with_stderr_tail() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_fail.sh");
    write_script(
        &script,
        r#"#!/bin/sh
printf '{"t":"progress","pct":10,"stage":"load","msg":"loading"}\n'
echo "fatal: vintage not found in repository" 1>&2
exit 3
"#,
    );
    let app = app_with_script(tmp.path(), &script, 2);

    let (status, body) = post_run(&app, &create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create returned {body}");
    let id = body["id"].as_str().unwrap().to_owned();

    let meta = poll_status(&app, &id, "failed", TIMEOUT).await;
    assert_eq!(meta["exit"], json!(3));
    let err = meta["error"].as_str().expect("error message present");
    assert!(
        err.contains("vintage not found"),
        "stderr tail captured: {err:?}"
    );

    let (rstatus, _) = get(&app, &format!("/api/runs/{id}/result")).await;
    assert_eq!(rstatus, StatusCode::CONFLICT);
}

/// Bounded worker pool: with `max_concurrency = 2`, submitting 3 blocking jobs leaves exactly one
/// `queued` until a slot frees. Releasing the sentinel lets all three finish.
#[tokio::test]
async fn bounded_pool_queues_excess_runs() {
    let tmp = TempDir::new().unwrap();
    let release = tmp.path().join("release");
    let script = tmp.path().join("job_block.sh");
    // The job blocks until the release sentinel appears (path baked in at generation time).
    write_script(
        &script,
        &format!(
            r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{{"t":"progress","pct":5,"stage":"load","msg":"waiting"}}\n'
while [ ! -f "{release}" ]; do sleep 0.02; done
printf '{{"ok":true}}' > "$run_dir/result.json"
printf '{{"t":"done","result":"result.json"}}\n'
exit 0
"#,
            release = release.display()
        ),
    );
    let app = app_with_script(tmp.path(), &script, 2);

    // Submit 3 runs.
    let mut ids = Vec::new();
    for _ in 0..3 {
        let (status, body) = post_run(&app, &create_body()).await;
        assert_eq!(status, StatusCode::CREATED);
        ids.push(body["id"].as_str().unwrap().to_owned());
    }

    // Poll until exactly 2 are running and 1 is queued (bounded pool = 2).
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (_, list) = get(&app, "/api/runs").await;
        let arr = list.as_array().unwrap();
        let running = arr.iter().filter(|m| m["status"] == "running").count();
        let queued = arr.iter().filter(|m| m["status"] == "queued").count();
        if running == 2 && queued == 1 {
            break;
        }
        if Instant::now() >= deadline {
            panic!("pool never reached 2 running / 1 queued; got {list}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Release: all three should now succeed.
    std::fs::write(&release, b"go").unwrap();
    for id in &ids {
        poll_status(&app, id, "succeeded", TIMEOUT).await;
    }
}

/// Validation is uniform `400` (never a serde `422`) for every invalid/missing param — a missing
/// required **string** field parses leniently and is rejected in one place, same as an empty one; and
/// no run is created.
#[tokio::test]
async fn create_rejects_invalid_request_uniform_400() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_ok.sh");
    write_script(&script, "#!/bin/sh\nexit 0\n");
    let app = app_with_script(tmp.path(), &script, 2);

    // A missing required string field (`vintage` absent) ⇒ 400, NOT 422.
    let missing_vintage = json!({
        "type": "backtest",
        "params": { "start": "2021-01-01", "end": "2021-02-01", "resolution": "1h", "universe": ["BTCUSDT"] }
    });
    let (status, body) = post_run(&app, &missing_vintage).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing vintage: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("vintage"),
        "clear error naming the field: {body}"
    );

    // A missing `universe` ⇒ 400 too.
    let missing_universe = json!({
        "type": "backtest",
        "params": { "vintage": "v", "start": "2021-01-01", "end": "2021-02-01", "resolution": "1h" }
    });
    let (status, _) = post_run(&app, &missing_universe).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An entirely empty body ⇒ 400 (lenient parse → validation), not a serde 422.
    let (status, _) = post_run(&app, &json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(lstatus, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0, "no run created");
}

/// Unknown ids: `GET /api/runs/{unknown}` and `.../result` both 404.
#[tokio::test]
async fn unknown_run_is_404() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_ok.sh");
    write_script(&script, "#!/bin/sh\nexit 0\n");
    let app = app_with_script(tmp.path(), &script, 2);

    let (s1, _) = get(&app, "/api/runs/does-not-exist").await;
    assert_eq!(s1, StatusCode::NOT_FOUND);
    let (s2, _) = get(&app, "/api/runs/does-not-exist/result").await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
}
