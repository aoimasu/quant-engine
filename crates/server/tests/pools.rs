//! QE-452 Phase B integration tests: the formula-pool read routes, the governance lifecycle routes
//! (approve/reject/revoke/seal), the fail-closed production seal, the `require_role` seam, and the run
//! `/halt` — driven in-process via `tower::ServiceExt::oneshot` (no network bind), hermetic + deterministic.
//!
//! Unix-only: the halt test spawns a POSIX shell fake job (dev + CI are macOS/Linux), mirroring the QE-255
//! run-lifecycle tests' spawn-seam strategy.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use qe_formula_pool::{
    FormulaPool, FormulaPoolContent, FormulaPoolRepository, PoolGovernanceStore,
};
use qe_run_protocol::EvolveArchive;
use qe_server::{build_router, AppState, CliJobSpawner, PoolState, RoleConfig, RunManager};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::time::Instant;
use tower::ServiceExt;

mod common;

/// The allowlisted email these tests authenticate as (session-gated `/api`).
const TEST_EMAIL: &str = "operator@example.com";
const TIMEOUT: Duration = Duration::from_secs(10);

// ---- fixtures -----------------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

/// Build + seal a valid formula pool with `mode` (`"sandbox"`/`"production"`) via the real
/// `FormulaPool::seal`, so a later `FormulaPool::load` verifies it. Deflation stats are constructed as
/// string-serialised `Decimal`s (never a raw `f64`-derived literal — Phase-A nit-2 carry-forward).
fn sealed_pool(id: &str, mode: &str) -> FormulaPool {
    let sexpr = "rank(close,20)";
    let formula_hash = sha256_hex(sexpr);
    let pool_hash = sha256_hex(&format!("{formula_hash}\n"));
    let content: FormulaPoolContent = serde_json::from_value(json!({
        "format_version": 1,
        "pool_id": id,
        "mode": mode,
        "formulas": [{ "sexpr": sexpr, "formula_hash": formula_hash }],
        "deflation": {
            "gp_aware": true,
            "distinct_evaluations": 192,
            "n_trials": 200,
            "analytic_floor": 90,
            "variance_trials": 45,
            "trial_variance": "0.1234",
            "expected_max_sharpe": "2.1",
            "champion_dsr": "0.97",
            "uncensored_pbo": "0.42"
        },
        "lineage": {
            "campaign_id": id,
            "seed": 7,
            "mode": mode,
            "code_commit": "commit-test",
            "input_snapshot_id": "",
            "config_hash": "cfg-hash",
            "pool_hash": pool_hash
        }
    }))
    .expect("valid pool content");
    FormulaPool::seal(content).expect("seal pool")
}

/// Write a sealed pool under the mode-appropriate root (`<artifacts>/research/pools` for sandbox,
/// `<artifacts>/pools` for production).
fn write_pool(artifacts: &Path, pool: &FormulaPool) {
    let root = if pool.content.mode == qe_formula_pool::PoolMode::Production {
        artifacts.join("pools")
    } else {
        artifacts.join("research").join("pools")
    };
    FormulaPoolRepository::new(root)
        .write(pool)
        .expect("write pool");
}

/// Assemble a router with a real [`PoolState`] (rooted at `artifacts`/`data`) + the given `roles`, over a
/// [`CliJobSpawner`] pointed at `script`. Returns the router + the manager (for direct assertions).
fn build_app(
    data: &Path,
    artifacts: &Path,
    script: &Path,
    roles: RoleConfig,
) -> (Router, Arc<RunManager>) {
    let spawner = Arc::new(CliJobSpawner::new(script.to_path_buf()));
    let manager = Arc::new(RunManager::new(data.join("runs"), spawner, 4));
    let auth = common::auth_context(TEST_EMAIL, None);
    let pools = Arc::new(PoolState::from_dirs(artifacts, data));
    let state = AppState::new(
        Arc::clone(&manager),
        auth,
        common::empty_read_state_under(data),
    )
    .with_pools(pools)
    .with_roles(Arc::new(roles));
    let router = build_router(&data.join("static"), state);
    (router, manager)
}

/// A no-op fake job (exits immediately) — for tests that don't drive a run.
fn noop_script(dir: &Path) -> PathBuf {
    let path = dir.join("noop.sh");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn roles_with_approver() -> RoleConfig {
    RoleConfig {
        operators: vec![TEST_EMAIL.to_owned()],
        approvers: vec![TEST_EMAIL.to_owned()],
        ..RoleConfig::default()
    }
}

// ---- request helpers ----------------------------------------------------------------------------

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

/// A GET carrying a valid session cookie for `TEST_EMAIL`.
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

/// A POST carrying a valid session cookie for `TEST_EMAIL`.
async fn post(app: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", common::session_cookie_header(TEST_EMAIL))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

/// A request with **no** session cookie (for the auth-rejection test).
async fn unauth(app: &Router, method: &str, uri: &str) -> StatusCode {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    resp.status()
}

async fn create_run(app: &Router, body: &Value) -> (StatusCode, Value) {
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

async fn poll_status(app: &Router, id: &str, want: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let (_, meta) = get(app, &format!("/api/runs/{id}")).await;
        if meta["status"] == want {
            return meta;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for `{want}`: {meta}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---- tests --------------------------------------------------------------------------------------

/// Every new route is behind `protected_routes` — an unauthenticated request is `401`.
#[tokio::test]
async fn unauthenticated_requests_to_new_routes_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let script = noop_script(tmp.path());
    let (app, _m) = build_app(
        &tmp.path().join("data"),
        &tmp.path().join("artifacts"),
        &script,
        roles_with_approver(),
    );
    for (method, uri) in [
        ("GET", "/api/formula-pools"),
        ("GET", "/api/formula-pools/some-id"),
        ("GET", "/api/runs/some-id/archive"),
        ("POST", "/api/formula-pools/some-id/approve"),
        ("POST", "/api/formula-pools/some-id/reject"),
        ("POST", "/api/formula-pools/some-id/revoke"),
        ("POST", "/api/formula-pools/some-id/seal"),
        ("POST", "/api/runs/some-id/halt"),
    ] {
        assert_eq!(
            unauth(&app, method, uri).await,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must be 401 without a session"
        );
    }
}

/// `GET /api/formula-pools[/{id}]` returns the verified pool shape; a missing id is `404`.
#[tokio::test]
async fn list_and_detail_return_verified_pool_shape() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-alpha", "sandbox"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let (status, list) = get(&app, "/api/formula-pools").await;
    assert_eq!(status, StatusCode::OK);
    let arr = list.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "pool-alpha");
    assert_eq!(arr[0]["mode"], "sandbox");
    assert_eq!(arr[0]["formula_count"], 1);
    assert_eq!(arr[0]["gp_aware"], true);
    assert_eq!(arr[0]["lifecycle"], "draft"); // no governance record yet ⇒ Draft

    let (status, detail) = get(&app, "/api/formula-pools/pool-alpha").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["content"]["formulas"][0]["sexpr"], "rank(close,20)");
    assert_eq!(detail["content"]["deflation"]["gp_aware"], true);
    assert_eq!(detail["content"]["lineage"]["seed"], 7);
    assert_eq!(detail["lifecycle"], "draft");

    let (status, _) = get(&app, "/api/formula-pools/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `GET /api/runs/{id}/archive` serves the run-dir `archive.json`, and `404`s an absent one.
#[tokio::test]
async fn archive_route_returns_shape_and_404s_missing() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    let script = noop_script(tmp.path());
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    // Seed a run dir with an archive.json (the shape the evolve job writes).
    let run_id = "run-xyz";
    let run_dir = data.join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    let archive = EvolveArchive {
        pool_id: "pool-alpha".to_owned(),
        mode: "sandbox".to_owned(),
        generations: 6,
        offspring: 16,
        cells: vec![qe_run_protocol::ArchiveCell {
            family: "Momentum".to_owned(),
            timescale: "Fast".to_owned(),
            complexity: "Simple".to_owned(),
            node_count: 3,
            best_fitness: Some(0.42),
        }],
        trial_basis: qe_run_protocol::ArchiveTrialBasis {
            distinct_evaluations: 192,
            n_trials: 200,
            analytic_floor: 90,
            expected_max_sharpe: Some(2.1),
            occupied_cells: 1,
            total_cells: 45,
        },
    };
    std::fs::write(
        run_dir.join("archive.json"),
        serde_json::to_vec(&archive).unwrap(),
    )
    .unwrap();

    let (status, body) = get(&app, &format!("/api/runs/{run_id}/archive")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pool_id"], "pool-alpha");
    assert_eq!(body["trial_basis"]["distinct_evaluations"], 192);
    assert_eq!(body["trial_basis"]["total_cells"], 45);
    assert_eq!(body["cells"][0]["family"], "Momentum");

    let (status, _) = get(&app, "/api/runs/no-such-run/archive").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The legal `draft → approved → (sandbox) sealed` path succeeds and is reflected in the detail lifecycle.
#[tokio::test]
async fn legal_draft_approved_sandbox_sealed_path() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-legal", "sandbox"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let (status, body) = post(&app, "/api/formula-pools/pool-legal/approve").await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    assert_eq!(body["lifecycle"], "approved");

    let (status, body) = post(&app, "/api/formula-pools/pool-legal/seal").await;
    assert_eq!(status, StatusCode::OK, "sandbox seal: {body}");
    assert_eq!(body["lifecycle"], "sealed");

    let (_, detail) = get(&app, "/api/formula-pools/pool-legal").await;
    assert_eq!(detail["lifecycle"], "sealed");
    assert_eq!(detail["history"].as_array().unwrap().len(), 2);
}

/// An illegal lifecycle edge at the route level (seal-before-approve) is rejected with `409`, and the pool
/// stays `Draft`.
#[tokio::test]
async fn illegal_seal_before_approve_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-illegal", "sandbox"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let (status, body) = post(&app, "/api/formula-pools/pool-illegal/seal").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "seal-before-approve must 409: {body}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("illegal"),
        "structured illegal-transition error: {body}"
    );
    let (_, detail) = get(&app, "/api/formula-pools/pool-illegal").await;
    assert_eq!(
        detail["lifecycle"], "draft",
        "pool stays Draft after an illegal seal"
    );
}

/// LOAD-BEARING (QE-454 Phase B): a `production`-mode pool without the dual sign-off / per-formula evidence
/// / a resolved launcher is refused by the server-authoritative `seal_allowed` predicate with a **named
/// blocker list** (`409`), and the pool is **never** sealed. (Approve first, to prove the refusal is the
/// seal predicate, not the lifecycle edge.) The richer happy-path + per-hard-block coverage lives in
/// `audit_governance.rs` (which wires a persistent-key audit log).
#[tokio::test]
async fn production_seal_without_signoffs_is_blocked_with_named_blockers() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-prod", "production"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    // Approve succeeds (only /seal runs the predicate).
    let (status, body) = post(&app, "/api/formula-pools/pool-prod/approve").await;
    assert_eq!(status, StatusCode::OK, "approve prod: {body}");
    assert_eq!(body["lifecycle"], "approved");

    // Seal is REFUSED by the predicate with a named blocker list.
    let (status, body) = post(&app, "/api/formula-pools/pool-prod/seal").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "production seal must 409: {body}"
    );
    let blockers = body["blockers"].as_array().expect("named blocker list");
    let names: Vec<&str> = blockers.iter().filter_map(|b| b.as_str()).collect();
    // The fixture has no dual sign-off, no per-formula evidence, and a censored dispersion population.
    assert!(
        names.contains(&"launcher_unresolved")
            || names.contains(&"insufficient_distinct_approver_signoffs"),
        "must block on the SoD/launcher clause: {names:?}"
    );
    assert!(
        names.contains(&"hb5_8_per_formula_evidence_absent"),
        "must block on absent per-formula evidence: {names:?}"
    );
    assert!(!body["evidence_hash"].as_str().unwrap_or("").is_empty());

    // The pool is NOT sealed — it stays `Approved`.
    let (_, detail) = get(&app, "/api/formula-pools/pool-prod").await;
    assert_eq!(
        detail["lifecycle"], "approved",
        "a blocked production pool never reaches Sealed"
    );
}

/// The `require_role` seam is on the governance path: an authenticated but role-less caller is `403` on a
/// governance route, while the same session reaches the read route (`200`).
#[tokio::test]
async fn require_role_seam_gates_governance_but_not_reads() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-role", "sandbox"));
    // EMPTY roles ⇒ TEST_EMAIL holds neither role (fail-closed).
    let (app, _m) = build_app(&data, &artifacts, &script, RoleConfig::default());

    // Read route: session is enough ⇒ 200.
    let (status, _) = get(&app, "/api/formula-pools/pool-role").await;
    assert_eq!(status, StatusCode::OK, "read route needs only a session");

    // Governance route: no role ⇒ 403 (the seam QE-454 hardens).
    let (status, body) = post(&app, "/api/formula-pools/pool-role/approve").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "role-less approve must 403: {body}"
    );

    // Halt is operator-gated ⇒ also 403 without the role.
    let (status, _) = post(&app, "/api/runs/whatever/halt").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "role-less halt must 403");
}

/// `POST /api/runs/{id}/halt` cooperatively stops a **running** evolve run (reusing the run-cancel
/// machinery): the run transitions to a terminal state carrying the operator-halt reason.
#[tokio::test]
async fn halt_cooperatively_stops_a_running_evolve_run() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    let release = tmp.path().join("release"); // never created ⇒ the job blocks forever until halted
    let script = tmp.path().join("job_block.sh");
    // Emit a progress line (so the supervisor marks it running), then block until a sentinel appears.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             echo '{{\"t\":\"progress\",\"pct\":30,\"stage\":\"search\",\"msg\":\"illuminating\"}}'\n\
             while [ ! -f \"{}\" ]; do sleep 0.05; done\n\
             exit 0\n",
            release.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let body = json!({
        "type": "evolve",
        "params": { "seed": 7, "start": "2021-01-01", "end": "2021-01-10", "resolution": "1h" }
    });
    let (status, created) = create_run(&app, &body).await;
    assert_eq!(status, StatusCode::CREATED, "create evolve: {created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // Deterministically observe `running` before halting (the job cannot finish on its own).
    poll_status(&app, &id, "running", TIMEOUT).await;

    let (status, halt_body) = post(&app, &format!("/api/runs/{id}/halt")).await;
    assert_eq!(status, StatusCode::OK, "halt: {halt_body}");
    assert_eq!(halt_body["halted"], true);

    // The run is terminal, marked failed with the operator-halt reason (RunStatus stays 4-state).
    let meta = poll_status(&app, &id, "failed", TIMEOUT).await;
    assert!(
        meta["error"].as_str().unwrap_or("").contains("halt"),
        "halted run records the operator-halt reason: {meta}"
    );

    // Halting an already-terminal run is a 409.
    let (status, _) = post(&app, &format!("/api/runs/{id}/halt")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "re-halt of a terminal run is 409"
    );
}

/// QE-461 §5.3: `POST /api/runs/{id}/halt` on a composite **flow** yields terminal `Failed` carrying a halt
/// reason (no new `RunStatus` variant) and RETAINS the partially-sealed vintage. The flow's train phase seals
/// (writes `train/result.json` + the recorded vintage), then the backtest phase blocks forever, so we halt
/// mid-backtest and assert the sealed-checkpoint artefact is retained + auditable.
#[tokio::test]
async fn halt_flow_yields_failed_with_halt_reason_and_retains_partial_vintage() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    let release = tmp.path().join("release"); // never created ⇒ the backtest blocks until halted
    let script = tmp.path().join("qe_flow.sh");
    // train: seal + emit handoff/vintage; backtest: emit progress then block forever on the release.
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
sub="$1"
run_dir=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--run-dir" ]; then run_dir="$a"; fi
  prev="$a"
done
if [ "$sub" = "train" ]; then
  printf '{{"t":"gate","pct":85,"stage":"gate","promoted":true,"n_trials":10}}\n'
  printf '%s\n' '{{"instrument":"BTCUSDT","gate_taker_fee_bps":5.0,"holdout_window":{{"start":"2021-01-05","end":"2021-01-10","resolution":"1h"}},"vintage_id":"vint-flow-1"}}' > "$run_dir/result.json"
  printf '{{"t":"done","result":"result.json","protocol_version":3,"vintage":"vint-flow-1"}}\n'
else
  printf '{{"t":"progress","pct":50,"stage":"simulate","msg":"backtest"}}\n'
  while [ ! -f "{release}" ]; do sleep 0.05; done
  exit 0
fi
"#,
            release = release.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let body = json!({
        "type": "flow",
        "params": { "seed": 7, "start": "2021-01-01", "end": "2021-01-10", "resolution": "1h" }
    });
    let (status, created) = create_run(&app, &body).await;
    assert_eq!(status, StatusCode::CREATED, "create flow: {created}");
    let id = created["id"].as_str().unwrap().to_owned();

    // Wait until the flow has entered the (blocking) backtest phase — its vintage is already sealed by then.
    let running = poll_flow_backtest(&app, &id, TIMEOUT).await;
    assert_eq!(
        running["flow"]["vintage"], "vint-flow-1",
        "vintage sealed: {running}"
    );

    let (status, halt_body) = post(&app, &format!("/api/runs/{id}/halt")).await;
    assert_eq!(status, StatusCode::OK, "halt: {halt_body}");
    assert_eq!(halt_body["halted"], true);

    // Terminal `failed` (NOT a new status) carrying the operator-halt reason.
    let meta = poll_status(&app, &id, "failed", TIMEOUT).await;
    assert!(
        meta["error"].as_str().unwrap_or("").contains("halt"),
        "halted flow records the operator-halt reason: {meta}"
    );
    // The partially-sealed vintage checkpoint (the train sub-run's sealed result) is RETAINED + auditable.
    assert!(
        data.join("runs")
            .join(&id)
            .join("train")
            .join("result.json")
            .exists(),
        "the partially-sealed vintage checkpoint must be retained after a halt"
    );
    assert_eq!(
        meta["flow"]["train_run"], "train",
        "the train lineage is auditable: {meta}"
    );

    // Re-halting a terminal flow is a 409.
    let (status, _) = post(&app, &format!("/api/runs/{id}/halt")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "re-halt of a terminal flow is 409"
    );
}

/// Poll `GET /api/runs/{id}` until the flow has recorded its backtest sub-run (i.e. it sealed the vintage and
/// entered the backtest phase), returning the meta.
async fn poll_flow_backtest(app: &Router, id: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let (_, meta) = get(app, &format!("/api/runs/{id}")).await;
        if meta["flow"]["backtest_run"] == "backtest" {
            return meta;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the flow backtest phase: {meta}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A legal reject edge and a revoke edge function through the routes (sandbox).
#[tokio::test]
async fn reject_and_revoke_edges_function() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-reject", "sandbox"));
    write_pool(&artifacts, &sealed_pool("pool-revoke", "sandbox"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    let (status, body) = post(&app, "/api/formula-pools/pool-reject/reject").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], "rejected");

    post(&app, "/api/formula-pools/pool-revoke/approve").await;
    let (status, body) = post(&app, "/api/formula-pools/pool-revoke/revoke").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], "revoked");
}

/// A tampered pool on disk is never served (load verifies) — the read route treats it as absent (`404`).
#[tokio::test]
async fn tampered_pool_is_not_served() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    let pool = sealed_pool("pool-tampered", "sandbox");
    write_pool(&artifacts, &pool);
    // Corrupt the on-disk content so the pinned hash no longer verifies.
    let path = FormulaPoolRepository::new(artifacts.join("research").join("pools"))
        .path_for("pool-tampered");
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["content"]["deflation"]["champion_dsr"] = json!("1.0"); // rosier than sealed
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());
    let (status, _) = get(&app, "/api/formula-pools/pool-tampered").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a pool that fails verify() is never served"
    );
}

/// Governance state is persisted alongside the pool artefact (in the governance store), not in the run.
#[tokio::test]
async fn governance_state_persists_in_the_governance_store() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let data = tmp.path().join("data");
    let script = noop_script(tmp.path());
    write_pool(&artifacts, &sealed_pool("pool-persist", "sandbox"));
    let (app, _m) = build_app(&data, &artifacts, &script, roles_with_approver());

    post(&app, "/api/formula-pools/pool-persist/approve").await;

    // The governance record exists on disk under `<data>/governance`, at `Approved`.
    let store = PoolGovernanceStore::new(data.join("governance"));
    let record = store.read("pool-persist").unwrap();
    assert_eq!(record.state.as_str(), "approved");
    assert_eq!(record.history.len(), 1);
    assert_eq!(record.history[0].actor, TEST_EMAIL);
}
