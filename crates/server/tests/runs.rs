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

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)
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

/// Like [`app_with_script`] but also returns the `Arc<RunManager>` so QE-407 tests can drive HTTP
/// **and** call `shutdown` / `reconcile_orphans` on the very same manager the router serves.
fn app_and_manager_with_script(
    data_dir: &Path,
    script: &Path,
    max_concurrency: usize,
) -> (Router, Arc<RunManager>) {
    let spawner = Arc::new(CliJobSpawner::new(script.to_path_buf()));
    let manager = Arc::new(RunManager::new(
        data_dir.join("runs"),
        spawner,
        max_concurrency,
    ));
    let auth = common::auth_context(TEST_EMAIL, None);
    let router = build_router(
        &data_dir.join("static"),
        common::app_state_under(Arc::clone(&manager), auth, data_dir),
    );
    (router, manager)
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

/// A create-run request body for a minimal valid training run (QE-261). The universe/store come from
/// config (not flags), so only the window is required.
fn create_train_body() -> Value {
    json!({
        "type": "train",
        "params": {
            "start": "2021-01-01",
            "end": "2021-02-01",
            "resolution": "1h",
            "generations": 2,
            "seed": 7
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

    // List shows the run (QE-410 slim envelope: `{ runs, next_cursor }`).
    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(lstatus, StatusCode::OK);
    let arr = list["runs"].as_array().expect("runs is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], json!(id));
    assert_eq!(arr[0]["status"], json!("succeeded"));
    assert_eq!(list["next_cursor"], Value::Null, "single page: {list}");
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
        let arr = list["runs"].as_array().unwrap();
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
    assert_eq!(list["runs"].as_array().unwrap().len(), 0, "no run created");
}

/// QE-261 Part A: a `type:"train"` run spawns the **train** job and the supervisor captures the QE-260
/// rich progress (`gen`/`ensemble`/`gate`) + the sealed vintage id from the terminal `done` into
/// `meta.json`, so `GET /api/runs/{id}` exposes them for the training monitor. Backtest runs are
/// unregressed (covered by the other tests in this file, which continue to pass).
#[tokio::test]
async fn train_run_captures_rich_progress_and_vintage() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_train.sh");
    // A fake `qe train` job: emit the QE-260 stream (a `-inf`/`null` best-so-far on gen 1, then a
    // finite one), write result.json, and finish with a `done` naming the sealed vintage.
    write_script(
        &script,
        r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{"t":"gen","pct":30,"stage":"search","generation":1,"generations":2,"coverage":3,"coverage_long":2,"coverage_short":1,"best_fitness":null}\n'
printf '{"t":"gen","pct":70,"stage":"search","generation":2,"generations":2,"coverage":5,"coverage_long":3,"coverage_short":2,"best_fitness":1.23}\n'
printf '{"t":"ensemble","pct":75,"stage":"ensemble","folds":4,"members":3,"score":0.42}\n'
printf '{"t":"gate","pct":85,"stage":"gate","promoted":true,"failed":[],"in_sample_sharpe":1.5,"holdout_sharpe":1.1,"dsr":0.8,"spa_pvalue":0.03,"n_trials":12}\n'
printf '{"ok":true}' > "$run_dir/result.json"
printf '{"t":"done","result":"result.json","vintage":"vintage-abc123"}\n'
exit 0
"#,
    );
    let app = app_with_script(tmp.path(), &script, 2);

    let (status, body) = post_run(&app, &create_train_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create returned {body}");
    let id = body["id"].as_str().expect("id in response").to_owned();

    let meta = poll_status(&app, &id, "succeeded", TIMEOUT).await;
    assert_eq!(meta["type"], json!("train"));
    assert_eq!(meta["artifacts"], json!(["result.json"]));

    // The rich training progress was tailed into meta.json (latest of each kind).
    let train = &meta["train"];
    assert_eq!(train["generation"]["generation"], json!(2));
    assert_eq!(train["generation"]["generations"], json!(2));
    assert_eq!(train["generation"]["coverage"], json!(5));
    assert_eq!(train["generation"]["coverage_long"], json!(3));
    assert_eq!(train["generation"]["coverage_short"], json!(2));
    assert_eq!(train["generation"]["best_fitness"], json!(1.23));
    assert_eq!(train["ensemble"]["folds"], json!(4));
    assert_eq!(train["ensemble"]["members"], json!(3));
    assert_eq!(train["gate"]["promoted"], json!(true));
    assert_eq!(train["gate"]["n_trials"], json!(12));
    assert_eq!(train["gate"]["failed"], json!([]));
    // The sealed vintage id from the terminal `done` is exposed for the deep-link.
    assert_eq!(train["vintage"], json!("vintage-abc123"));

    // The coarse progress bar reaches 100% on a succeeded train run (past the gate line's 85%).
    assert_eq!(meta["progress"]["pct"], json!(100));
    assert_eq!(meta["progress"]["stage"], json!("done"));

    // The result endpoint serves the artefact the train job wrote.
    let (rstatus, rbody) = get(&app, &format!("/api/runs/{id}/result")).await;
    assert_eq!(rstatus, StatusCode::OK);
    assert_eq!(rbody, json!({ "ok": true }));
}

/// QE-261: a `type:"train"` run with a missing window field is a uniform `400` (never a serde `422`),
/// and no run is created — mirroring the backtest validation contract.
#[tokio::test]
async fn train_run_rejects_missing_window_400() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_ok.sh");
    write_script(&script, "#!/bin/sh\nexit 0\n");
    let app = app_with_script(tmp.path(), &script, 2);

    // `start` omitted ⇒ 400 naming the field.
    let missing_start = json!({
        "type": "train",
        "params": { "end": "2021-02-01", "resolution": "1h" }
    });
    let (status, body) = post_run(&app, &missing_start).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing start: {body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("start"),
        "clear error naming the field: {body}"
    );

    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(lstatus, StatusCode::OK);
    assert_eq!(list["runs"].as_array().unwrap().len(), 0, "no run created");
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

/// QE-411: `list_runs` batches its per-run `meta.json` reads into one `spawn_blocking` closure. This
/// proves the batched read preserves behaviour: the list is newest-first (index order reversed), and an
/// indexed run whose `meta.json` is missing is skipped (not a 500).
#[tokio::test]
async fn list_runs_newest_first_and_skips_missing_meta() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_quick.sh");
    // A job that immediately writes result.json and finishes — so both runs reach `succeeded` fast.
    write_script(
        &script,
        r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{"ok":true}' > "$run_dir/result.json"
printf '{"t":"done","result":"result.json"}\n'
exit 0
"#,
    );
    let app = app_with_script(tmp.path(), &script, 2);

    // Create A then B (index insertion order [A, B]); the newest-first list is therefore [B, A].
    let (_, a) = post_run(&app, &create_body()).await;
    let id_a = a["id"].as_str().unwrap().to_owned();
    poll_status(&app, &id_a, "succeeded", TIMEOUT).await;
    let (_, b) = post_run(&app, &create_body()).await;
    let id_b = b["id"].as_str().unwrap().to_owned();
    poll_status(&app, &id_b, "succeeded", TIMEOUT).await;

    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(lstatus, StatusCode::OK);
    let arr = list["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 2, "both runs listed: {list}");
    assert_eq!(arr[0]["id"], json!(id_b), "newest first: {list}");
    assert_eq!(arr[1]["id"], json!(id_a), "then the older run: {list}");

    // Delete A's `meta.json`: it stays in `index.json` but has no meta — the list skips it (not a 500).
    let meta_a = tmp.path().join("runs").join(&id_a).join("meta.json");
    std::fs::remove_file(&meta_a).unwrap();
    let (lstatus, list) = get(&app, "/api/runs").await;
    assert_eq!(
        lstatus,
        StatusCode::OK,
        "indexed-but-missing meta is skipped, not an error: {list}"
    );
    let arr = list["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "only the run with meta remains: {list}");
    assert_eq!(arr[0]["id"], json!(id_b), "the surviving run is B: {list}");
}

/// QE-408: `GET /api/runs?type=` filters the listing to a single run type. A mixed store (one backtest
/// + one train run) returns only the matching type; no `?type=` returns both (parity); an unknown type
/// returns an empty list (a `200`, not an error).
#[tokio::test]
async fn list_runs_filters_by_type_query() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_quick.sh");
    // A job that immediately writes result.json and finishes — the subcommand (backtest/train) is
    // ignored, so the one script drives both run types to `succeeded` fast.
    write_script(
        &script,
        r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{"ok":true}' > "$run_dir/result.json"
printf '{"t":"done","result":"result.json"}\n'
exit 0
"#,
    );
    let app = app_with_script(tmp.path(), &script, 2);

    // Create one backtest and one train run; wait for both to settle.
    let (_, b) = post_run(&app, &create_body()).await;
    let id_bt = b["id"].as_str().unwrap().to_owned();
    poll_status(&app, &id_bt, "succeeded", TIMEOUT).await;
    let (_, t) = post_run(&app, &create_train_body()).await;
    let id_tr = t["id"].as_str().unwrap().to_owned();
    poll_status(&app, &id_tr, "succeeded", TIMEOUT).await;

    // No filter → both runs (parity with the pre-QE-408 behaviour, now under the QE-410 envelope).
    let (s, all) = get(&app, "/api/runs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        all["runs"].as_array().unwrap().len(),
        2,
        "unfiltered lists both: {all}"
    );

    // ?type=backtest → only the backtest run.
    let (s, only_bt) = get(&app, "/api/runs?type=backtest").await;
    assert_eq!(s, StatusCode::OK);
    let arr = only_bt["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "only the backtest run: {only_bt}");
    assert_eq!(arr[0]["id"], json!(id_bt));
    assert_eq!(arr[0]["type"], json!("backtest"));

    // ?type=train → only the train run.
    let (s, only_tr) = get(&app, "/api/runs?type=train").await;
    assert_eq!(s, StatusCode::OK);
    let arr = only_tr["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "only the train run: {only_tr}");
    assert_eq!(arr[0]["id"], json!(id_tr));
    assert_eq!(arr[0]["type"], json!("train"));

    // An unrecognised type matches nothing — an empty `200`, not an error.
    let (s, none) = get(&app, "/api/runs?type=bogus").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        none["runs"].as_array().unwrap().len(),
        0,
        "unknown type → empty list: {none}"
    );

    // QE-410: `?status=` composes with `?type=`. Both runs succeeded, so `?status=succeeded&type=train`
    // returns the one train run; `?status=running` (none) returns an empty page.
    let (s, tr_ok) = get(&app, "/api/runs?type=train&status=succeeded").await;
    assert_eq!(s, StatusCode::OK);
    let arr = tr_ok["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "type+status compose: {tr_ok}");
    assert_eq!(arr[0]["id"], json!(id_tr));
    let (s, running) = get(&app, "/api/runs?status=running").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        running["runs"].as_array().unwrap().len(),
        0,
        "no running runs → empty page: {running}"
    );

    // QE-410: the list rows are the SLIM projection — they carry `label` but defer heavy `params`,
    // while `GET /api/runs/{id}` still serves the full meta (with `params`).
    let (_, list) = get(&app, "/api/runs?type=backtest").await;
    let row = &list["runs"].as_array().unwrap()[0];
    assert!(row["label"].is_string(), "slim row has label: {row}");
    assert!(row["params"].is_null(), "slim row defers params: {row}");
    let (_, full) = get(&app, &format!("/api/runs/{id_bt}")).await;
    assert!(
        full["params"].is_object(),
        "detail endpoint still serves full params: {full}"
    );
}

/// QE-410: `?limit=`/`?cursor=` paginate with an **id-anchored** cursor that is stable under concurrent
/// creates. Page 1 caps at `limit` and yields a `next_cursor`; walking the cursor returns the older rows
/// with no overlap — and a run created *between* the two page fetches (which prepends to the newest-first
/// view) never leaks into, skips, or duplicates the cursor-paginated older page.
#[tokio::test]
async fn list_runs_paginates_stably_under_concurrent_creates() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_quick.sh");
    write_script(
        &script,
        r#"#!/bin/sh
run_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) run_dir="$2"; shift 2 ;;
    *) shift ;;
  esac
done
printf '{"ok":true}' > "$run_dir/result.json"
printf '{"t":"done","result":"result.json"}\n'
exit 0
"#,
    );
    let app = app_with_script(tmp.path(), &script, 4);

    // Create 4 runs in order [r0, r1, r2, r3]; newest-first view is [r3, r2, r1, r0].
    let mut ids = Vec::new();
    for _ in 0..4 {
        let (_, b) = post_run(&app, &create_body()).await;
        let id = b["id"].as_str().unwrap().to_owned();
        poll_status(&app, &id, "succeeded", TIMEOUT).await;
        ids.push(id);
    }
    let newest_first = [&ids[3], &ids[2], &ids[1], &ids[0]];

    // Page 1: limit=2 → the two newest [r3, r2], with a next_cursor.
    let (_, p1) = get(&app, "/api/runs?limit=2").await;
    let arr = p1["runs"].as_array().unwrap();
    assert_eq!(arr.len(), 2, "page capped at limit: {p1}");
    assert_eq!(arr[0]["id"], json!(*newest_first[0]));
    assert_eq!(arr[1]["id"], json!(*newest_first[1]));
    let cursor = p1["next_cursor"].as_str().expect("next_cursor present");
    assert_eq!(
        cursor,
        newest_first[1].as_str(),
        "cursor anchors on last row"
    );

    // Interleave a NEW create (prepends to the newest-first view) BEFORE fetching page 2.
    let (_, nb) = post_run(&app, &create_body()).await;
    let new_id = nb["id"].as_str().unwrap().to_owned();
    poll_status(&app, &new_id, "succeeded", TIMEOUT).await;

    // Page 2 via the cursor: exactly the original older rows [r1, r0] — the new run does NOT appear,
    // and nothing is skipped or duplicated (stability under concurrent creates).
    let (_, p2) = get(&app, &format!("/api/runs?limit=2&cursor={cursor}")).await;
    let arr = p2["runs"].as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "second page returns the remaining older rows: {p2}"
    );
    assert_eq!(arr[0]["id"], json!(*newest_first[2]));
    assert_eq!(arr[1]["id"], json!(*newest_first[3]));
    assert_eq!(p2["next_cursor"], Value::Null, "last page: {p2}");
    let page2_ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(
        !page2_ids.contains(&new_id.as_str()),
        "a concurrently-created run never leaks into an older cursor page: {p2}"
    );
}

// ---------------------------------------------------------------------------
// QE-407: run-lifecycle robustness — graceful shutdown, task registry, honest success.
// ---------------------------------------------------------------------------

/// AC (hard-kill reconcile): a `running`/`queued` `meta.json` left behind by a crashed prior process
/// has no live supervisor on the next boot; the startup reconciler fails it, while a `succeeded` run is
/// left untouched (so the reconciler is non-vacuous and never clobbers a terminal run).
#[test]
fn reconcile_orphans_fails_running_leftovers_but_keeps_terminal() {
    let tmp = TempDir::new().unwrap();
    let runs_dir = tmp.path().join("runs");

    // Seed the on-disk store exactly as a hard-killed process would leave it.
    let seed = |id: &str, status: &str| {
        let dir = runs_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let terminal = status == "succeeded";
        let meta = json!({
            "id": id,
            "type": "backtest",
            "status": status,
            "params": {},
            "progress": { "pct": 10, "stage": "load", "msg": "loading" },
            "created_ms": 1,
            "started_ms": 2,
            "finished_ms": (if terminal { json!(3) } else { Value::Null }),
            "exit": (if terminal { json!(0) } else { Value::Null }),
            "error": Value::Null,
            "artifacts": (if terminal { json!(["result.json"]) } else { json!([]) }),
        });
        std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    };
    seed("run-running", "running");
    seed("run-queued", "queued");
    seed("run-succeeded", "succeeded");
    let index = json!([
        { "id": "run-running", "type": "backtest", "created_ms": 1, "label": "v" },
        { "id": "run-queued", "type": "backtest", "created_ms": 1, "label": "v" },
        { "id": "run-succeeded", "type": "backtest", "created_ms": 1, "label": "v" },
    ]);
    std::fs::write(runs_dir.join("index.json"), index.to_string()).unwrap();

    let spawner = Arc::new(CliJobSpawner::new(tmp.path().join("never-spawned")));
    let manager = RunManager::new(runs_dir.clone(), spawner, 2);

    let reconciled = manager.reconcile_orphans().expect("reconcile ok");
    assert_eq!(
        reconciled, 2,
        "running + queued reconciled; succeeded left alone"
    );

    let read = |id: &str| -> Value {
        serde_json::from_slice(&std::fs::read(runs_dir.join(id).join("meta.json")).unwrap())
            .unwrap()
    };
    for id in ["run-running", "run-queued"] {
        let m = read(id);
        assert_eq!(m["status"], json!("failed"), "{id} now failed: {m}");
        assert!(
            m["error"]
                .as_str()
                .unwrap()
                .contains("interrupted by a server restart"),
            "{id} honest reason: {m}"
        );
        assert!(m["finished_ms"].is_u64(), "{id} finished_ms set: {m}");
    }
    let ok = read("run-succeeded");
    assert_eq!(ok["status"], json!("succeeded"), "terminal run untouched");
}

/// AC (graceful shutdown drain): a run blocked in `running` is drained within the bounded window —
/// `shutdown` aborts it and terminally marks it `failed` (no `running` meta survives a clean
/// shutdown) — and the manager then refuses new runs (`503`).
#[tokio::test]
async fn shutdown_drains_running_run_and_stops_accepting() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_block.sh");
    // Blocks forever in `running` (never releases), so the drain window must abort + terminally mark.
    write_script(
        &script,
        r#"#!/bin/sh
printf '{"t":"progress","pct":10,"stage":"load","msg":"loading"}\n'
while true; do sleep 0.05; done
"#,
    );
    let (app, manager) = app_and_manager_with_script(tmp.path(), &script, 2);

    let (status, body) = post_run(&app, &create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create returned {body}");
    let id = body["id"].as_str().unwrap().to_owned();

    // Observe `running` before shutting down (the job cannot finish on its own).
    let running = poll_status(&app, &id, "running", TIMEOUT).await;
    assert_eq!(running["status"], json!("running"));

    // Bounded drain: the job never finishes, so shutdown aborts + terminally marks it.
    manager.shutdown(Duration::from_millis(200)).await;

    let (_, meta) = get(&app, &format!("/api/runs/{id}")).await;
    assert_eq!(
        meta["status"],
        json!("failed"),
        "drained run is failed: {meta}"
    );
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .contains("before server shutdown"),
        "honest shutdown reason: {meta}"
    );
    assert!(meta["finished_ms"].is_u64(), "finished_ms set: {meta}");

    // The manager no longer accepts runs once shutdown has begun.
    let (rstatus, rbody) = post_run(&app, &create_body()).await;
    assert_eq!(
        rstatus,
        StatusCode::SERVICE_UNAVAILABLE,
        "post-shutdown create is refused: {rbody}"
    );
}

/// AC (honest success): a job that prints `done` and exits 0 but writes **no** `result.json` is
/// `failed` (not falsely `succeeded`), with the honest reason; `GET /result` then reports a `409` on a
/// *failed* run rather than 409-ing on a run the UI showed green.
#[tokio::test]
async fn done_without_result_json_is_failed_not_succeeded() {
    let tmp = TempDir::new().unwrap();
    let script = tmp.path().join("job_liar.sh");
    write_script(
        &script,
        r#"#!/bin/sh
printf '{"t":"progress","pct":50,"stage":"simulate","msg":"working"}\n'
printf '{"t":"done","result":"result.json"}\n'
exit 0
"#,
    );
    let app = app_with_script(tmp.path(), &script, 2);

    let (status, body) = post_run(&app, &create_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create returned {body}");
    let id = body["id"].as_str().unwrap().to_owned();

    let meta = poll_status(&app, &id, "failed", TIMEOUT).await;
    assert_eq!(
        meta["exit"],
        json!(0),
        "exited 0 yet failed for a missing result: {meta}"
    );
    assert!(
        meta["error"]
            .as_str()
            .unwrap()
            .contains("wrote no result.json"),
        "honest reason: {meta}"
    );
    assert_eq!(
        meta["artifacts"],
        json!([]),
        "no artefacts recorded: {meta}"
    );

    // The result endpoint 409s on a *failed* run — not on a falsely-green one.
    let (rstatus, rbody) = get(&app, &format!("/api/runs/{id}/result")).await;
    assert_eq!(rstatus, StatusCode::CONFLICT);
    assert_eq!(
        rbody["status"],
        json!("failed"),
        "409 reports a failed run, not a green one: {rbody}"
    );
}
