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
