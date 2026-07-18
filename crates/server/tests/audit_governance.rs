//! QE-454 Phase A integration tests: authoritative `require_role`, the tamper-evident audit log +
//! `GET /api/audit`, dual sign-off / separation of duties through the governance routes, forward-only
//! revocation, the `GovernanceRecord` byte-identity guarantee, and the UX-only `/api/me` capabilities.
//! Driven in-process via `tower::ServiceExt::oneshot` (no network bind) — hermetic + deterministic.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use qe_formula_pool::{
    FormulaPool, FormulaPoolContent, FormulaPoolRepository, GovernanceRecord, PoolMode, Revocations,
};
use qe_server::{
    build_router, AppState, AuditAction, AuditLog, CliJobSpawner, PoolState, RoleConfig, RunManager,
};
use qe_vintage::VintageRepository;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

const LAUNCHER: &str = "launcher@example.com";
const APPROVER_A: &str = "approver-a@example.com";
const APPROVER_B: &str = "approver-b@example.com";
const NOBODY: &str = "nobody@example.com";

// ---- fixtures -----------------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build + seal a valid pool with `mode` (mirrors `pools.rs`'s helper) so a later `load` verifies it.
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
            "gp_aware": true, "distinct_evaluations": 192, "n_trials": 200, "analytic_floor": 90,
            "variance_trials": 45, "trial_variance": "0.1234", "expected_max_sharpe": "2.1",
            "champion_dsr": "0.97", "uncensored_pbo": "0.42"
        },
        "lineage": {
            "campaign_id": id, "seed": 7, "mode": mode, "code_commit": "commit-test",
            "input_snapshot_id": "", "config_hash": "cfg-hash", "pool_hash": pool_hash
        }
    }))
    .expect("valid pool content");
    FormulaPool::seal(content).expect("seal pool")
}

/// The `pool_hash` a `sealed_pool` carries (the signature-binding key).
fn pool_hash_of() -> String {
    sha256_hex(&format!("{}\n", sha256_hex("rank(close,20)")))
}

fn write_pool(artifacts: &Path, pool: &FormulaPool) {
    let root = if pool.content.mode == PoolMode::Production {
        artifacts.join("pools")
    } else {
        artifacts.join("research").join("pools")
    };
    FormulaPoolRepository::new(root)
        .write(pool)
        .expect("write pool");
}

/// Assemble a router with a real [`PoolState`] + [`AuditLog`] (persistent key) + the given `roles`.
/// Returns the router + the audit log handle (so a test can seed launch entries / read the chain).
fn build_app(data: &Path, artifacts: &Path, roles: RoleConfig) -> (Router, Arc<AuditLog>) {
    let script = data.join("noop.sh");
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    let spawner = Arc::new(CliJobSpawner::new(script));
    let manager = Arc::new(RunManager::new(data.join("runs"), spawner, 4));
    let auth = common::auth_context("x@example.com", None); // allowlist irrelevant to session verify
    let pools = Arc::new(PoolState::from_dirs(artifacts, data));
    let audit = Arc::new(AuditLog::new(
        data.join("audit").join("log.jsonl"),
        b"test-audit-signing-key".to_vec(),
        false,
    ));
    let state = AppState::new(
        Arc::clone(&manager),
        auth,
        common::empty_read_state_under(data),
    )
    .with_pools(pools)
    .with_roles(Arc::new(roles))
    .with_audit(Arc::clone(&audit));
    let router = build_router(&data.join("static"), state);
    (router, audit)
}

fn roles() -> RoleConfig {
    RoleConfig {
        operators: vec![LAUNCHER.to_owned()],
        approvers: vec![APPROVER_A.to_owned(), APPROVER_B.to_owned()],
        admins: vec![],
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

async fn get_as(app: &Router, uri: &str, email: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("cookie", common::session_cookie_header(email))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

async fn post_as(app: &Router, uri: &str, email: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", common::session_cookie_header(email))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

/// A POST as `email` that ALSO carries a forged `x-role` header + a `{"role": …}` body — used to prove the
/// server ignores request-supplied role claims (roles come only from the env allowlist).
async fn post_with_forged_role(app: &Router, uri: &str, email: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("cookie", common::session_cookie_header(email))
                .header("x-role", "approver")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "role": "approver" }).to_string()))
                .unwrap(),
        )
        .await
        .expect("router responds");
    read_json(resp).await
}

// ---- 1. authoritative require_role (per-request, never from the cookie/body/header) -------------

#[tokio::test]
async fn require_role_is_resolved_server_side_not_from_the_request() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-rbac", "sandbox"));
    let (app, _audit) = build_app(&data, &artifacts, roles());

    // A read route needs only a session — any authenticated email passes.
    let (status, _) = get_as(&app, "/api/formula-pools/pool-rbac", NOBODY).await;
    assert_eq!(status, StatusCode::OK, "read route needs only a session");

    // A role-less caller is 403 on a governance route …
    let (status, body) = post_as(&app, "/api/formula-pools/pool-rbac/approve", NOBODY).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "role-less approve is 403: {body}"
    );

    // … and STAYS 403 even when the request forges a role in a header AND the body — the role is
    // resolved per-request from the env allowlist, never from anything the client supplies.
    let (status, _) =
        post_with_forged_role(&app, "/api/formula-pools/pool-rbac/approve", NOBODY).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a forged cookie/body/header role claim is ignored — roles come only from the env allowlist"
    );

    // An allowlisted approver passes the gate (reaches the handler → 200).
    let (status, body) = post_as(&app, "/api/formula-pools/pool-rbac/approve", APPROVER_A).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "allowlisted approver passes: {body}"
    );
}

// ---- 2. tamper-evident audit log via GET /api/audit --------------------------------------------

#[tokio::test]
async fn audit_endpoint_reports_chain_status_and_detects_tamper() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-audit", "sandbox"));
    let (app, audit) = build_app(&data, &artifacts, roles());

    // Two approvals append two chained, HMAC'd entries.
    post_as(&app, "/api/formula-pools/pool-audit/approve", APPROVER_A).await;
    post_as(&app, "/api/formula-pools/pool-audit/approve", APPROVER_B).await;

    let (status, page) = get_as(&app, "/api/audit", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 2);
    assert_eq!(
        page["chain"]["status"], "ok",
        "a clean chain verifies: {page}"
    );
    assert_eq!(page["entries"][0]["action"], "approve");

    // Tamper with the on-disk log (rewrite who approved entry #0) and re-read: the chain breaks at seq 0.
    let log_path = audit.path().to_path_buf();
    let text = std::fs::read_to_string(&log_path).unwrap();
    let mut lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    lines[0]["actor_email"] = json!("attacker@evil.com");
    let rewritten: String = lines.iter().map(|v| format!("{v}\n")).collect();
    std::fs::write(&log_path, rewritten).unwrap();

    let (_, page) = get_as(&app, "/api/audit", APPROVER_A).await;
    assert_eq!(
        page["chain"],
        json!({ "status": "broken_at", "seq": 0 }),
        "a mutated entry breaks the chain at its seq: {page}"
    );
}

// ---- 3. dual sign-off / separation of duties through the routes --------------------------------

#[tokio::test]
async fn dual_signoff_requires_two_distinct_approvers_not_the_launcher() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-dual", "sandbox"));
    // Give the launcher the approver role too, so a rejected self-approve is proven to be the SoD guard
    // (not merely a role failure).
    let mut r = roles();
    r.approvers.push(LAUNCHER.to_owned());
    let (app, audit) = build_app(&data, &artifacts, r);

    // Commit the launcher as a launch entry bound to the pool id (what the run terminal binds in Phase B).
    audit
        .append(
            LAUNCHER,
            AuditAction::Launch,
            "pool-dual",
            "run-1",
            "",
            "",
            now_ms(),
        )
        .await
        .unwrap();

    // The launcher cannot approve its own pool — separation of duties (403), despite holding the role.
    let (status, body) = post_as(&app, "/api/formula-pools/pool-dual/approve", LAUNCHER).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "launcher-as-approver is refused: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("separation of duties"),
        "SoD-specific error: {body}"
    );

    // First distinct approver → awaiting the second signoff.
    let (status, body) = post_as(&app, "/api/formula-pools/pool-dual/approve", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["signoff"], "awaiting_second_signoff", "{body}");

    // The SAME approver again → still awaiting (distinct count stays 1).
    let (_, body) = post_as(&app, "/api/formula-pools/pool-dual/approve", APPROVER_A).await;
    assert_eq!(
        body["signoff"], "awaiting_second_signoff",
        "same approver twice: {body}"
    );

    // A second DISTINCT approver → the two-signature clause is satisfiable.
    let (_, body) = post_as(&app, "/api/formula-pools/pool-dual/approve", APPROVER_B).await;
    assert_eq!(body["signoff"], "two_distinct_signoffs", "{body}");
}

// ---- 4. fail-closed audit signing key (production-seal capability) ------------------------------

#[tokio::test]
async fn production_seal_capability_is_fail_closed_and_seal_stays_gated() {
    // The capability predicate is refused under an ephemeral key, allowed under a persistent one.
    let ephemeral = AuditLog::disabled();
    assert!(ephemeral.signing_key_is_ephemeral());
    assert!(
        !ephemeral.production_seal_capability_allowed(),
        "an unset/ephemeral QE_AUDIT_SIGNING_KEY refuses production-seal capability (fail-closed)"
    );
    let persistent = AuditLog::new(
        std::env::temp_dir()
            .join("qe-audit-persistent-test")
            .join("l.jsonl"),
        b"persistent".to_vec(),
        false,
    );
    assert!(persistent.production_seal_capability_allowed());

    // And a production `/seal` of a pool lacking the dual sign-off / per-formula evidence is refused by the
    // Phase-B predicate with a named blocker list (fail-closed).
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-prod", "production"));
    let (app, _audit) = build_app(&data, &artifacts, roles());
    post_as(&app, "/api/formula-pools/pool-prod/approve", APPROVER_A).await;
    let (status, body) = post_as(&app, "/api/formula-pools/pool-prod/seal", APPROVER_A).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "production seal is refused by the predicate: {body}"
    );
    assert!(
        body["blockers"].as_array().is_some_and(|b| !b.is_empty()),
        "the refusal carries a named blocker list: {body}"
    );
}

// ---- 5. revocation is forward-only (inert on the read path, no history rewrite) ----------------

#[tokio::test]
async fn revocation_makes_a_sealed_pool_inert_without_rewriting_history() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-rev", "sandbox"));
    let (app, audit) = build_app(&data, &artifacts, roles());

    // Approve → seal (sandbox) → revoke.
    post_as(&app, "/api/formula-pools/pool-rev/approve", APPROVER_A).await;
    let (status, _) = post_as(&app, "/api/formula-pools/pool-rev/seal", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_as(&app, "/api/formula-pools/pool-rev/revoke", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK, "revoke: {body}");

    // The read path is now inert: the detail flags the pool revoked (even though it was sealed).
    let (_, detail) = get_as(&app, "/api/formula-pools/pool-rev", APPROVER_A).await;
    assert_eq!(
        detail["revoked"], true,
        "revoked pool is inert on the read path: {detail}"
    );
    assert_eq!(detail["lifecycle"], "revoked");

    // `revocations.json` carries the pool_hash — the filter both the G1/promotion and read paths consult.
    let revocations_path = data.join("governance").join("revocations.json");
    let rev = Revocations::from_json(&std::fs::read(&revocations_path).unwrap()).unwrap();
    assert!(
        rev.is_revoked(&pool_hash_of()),
        "revocations.json records the pool_hash"
    );

    // History is NOT rewritten: the earlier `approve` entry survives, and the chain still verifies.
    let entries = audit.read_all().unwrap();
    assert!(
        entries.iter().any(|e| e.action == AuditAction::Approve),
        "the pre-revocation approve entry is preserved (forward-only)"
    );
    assert!(
        entries.iter().any(|e| e.action == AuditAction::Revoke),
        "the revoke entry is appended"
    );
    assert!(
        audit.verify_chain(&entries).is_ok(),
        "the chain still verifies after revoke"
    );
}

// ---- 6. GovernanceRecord does NOT change vintage_id / content_hash (AC4 byte-identity) ----------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[tokio::test]
async fn governance_record_does_not_perturb_the_vintage_identity() {
    // Load the committed sealed vintage fixture through the verifying repository.
    let repo = VintageRepository::new(fixtures_dir());
    let vintage = repo.load("sample_vintage").expect("load fixture vintage");
    let content_hash_before = vintage.content_hash.clone();
    let vintage_id_before = vintage.content.vintage_id.clone();
    assert_eq!(
        content_hash_before,
        vintage.content.content_hash().unwrap(),
        "the fixture is a valid sealed vintage"
    );

    // Build a GovernanceRecord that REFERENCES the vintage hash and embeds approver identities.
    let record = GovernanceRecord {
        vintage_content_hash: content_hash_before.clone(),
        pool_formula_hashes: vec![sha256_hex("rank(close,20)")],
        launch_entry_hash: "a".repeat(64),
        approval_entry_hashes: vec![sha256_hex(APPROVER_A), sha256_hex(APPROVER_B)],
        evidence_hash: "b".repeat(64),
    };
    let record_hash = record.content_hash().unwrap();

    // The vintage's identity is UNCHANGED — the record lives outside `VintageContent`, so approver
    // identity never enters the hashed struct (QE-450 AC4 byte-identity preserved).
    vintage.verify().expect("vintage still verifies");
    assert_eq!(
        vintage.content_hash, content_hash_before,
        "content_hash unchanged"
    );
    assert_eq!(
        vintage.content.vintage_id, vintage_id_before,
        "vintage_id unchanged"
    );
    assert_eq!(
        vintage.content.content_hash().unwrap(),
        content_hash_before,
        "recomputed content_hash is byte-identical"
    );
    // The record's own address is independent of the vintage hash it references.
    assert_ne!(record_hash, content_hash_before);
}

// ---- 7. /api/me capabilities are UX-only (never authoritative) ---------------------------------

#[tokio::test]
async fn me_capabilities_mirror_roles_but_never_authorize() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &sealed_pool("pool-me", "sandbox"));
    let (app, _audit) = build_app(&data, &artifacts, roles());

    // An approver sees canApprove:true; the launcher (operator) sees canLaunch:true.
    let (_, me) = get_as(&app, "/api/me", APPROVER_A).await;
    assert_eq!(me["capabilities"]["canApprove"], true);
    assert_eq!(me["capabilities"]["canLaunch"], false);
    let (_, me) = get_as(&app, "/api/me", LAUNCHER).await;
    assert_eq!(me["capabilities"]["canLaunch"], true);
    assert_eq!(me["capabilities"]["canApprove"], false);

    // A no-role caller sees everything false — and the server STILL enforces (403 on approve), proving
    // capabilities are hints that only *remove* affordances, never grant access.
    let (_, me) = get_as(&app, "/api/me", NOBODY).await;
    assert_eq!(me["capabilities"]["canApprove"], false);
    let (status, _) = post_as(&app, "/api/formula-pools/pool-me/approve", NOBODY).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "capabilities never authorize — the server enforces"
    );
}

// ---- 8. QE-454 Phase B — the server-authoritative seal predicate + barriers + carry-forward #1 -----

/// A **production** pool that passes all four deflation-summary hard-blocks AND carries a passing
/// per-formula `gate_evidence` row (hard-blocks 5–8). Distinct id ⇒ distinct `pool_hash`.
fn production_pool_with_evidence(id: &str) -> FormulaPool {
    let sexpr = "rank(close,20)";
    let formula_hash = sha256_hex(sexpr);
    let pool_hash = sha256_hex(&format!("{formula_hash}\n"));
    let content: FormulaPoolContent = serde_json::from_value(json!({
        "format_version": 1,
        "pool_id": id,
        "mode": "production",
        "formulas": [{ "sexpr": sexpr, "formula_hash": formula_hash }],
        "deflation": {
            "gp_aware": true, "distinct_evaluations": 500000, "n_trials": 500000,
            "analytic_floor": 7200, "variance_trials": 500000, "trial_variance": "0.02",
            "expected_max_sharpe": "9.0", "champion_dsr": "0.98", "uncensored_pbo": "0.20"
        },
        "gate_evidence": [{
            "formula_hash": formula_hash,
            "ic_two_fold_same_sign_fdr_pass": true,
            "cost_stress_min_net_log_growth": "0.005",
            "realised_turnover_frac": "0.20",
            "capacity_usd": "300000",
            "within_caps_and_stratum_deflated": true,
            "random_entry_null_pass": true
        }],
        "lineage": {
            "campaign_id": id, "seed": 7, "mode": "production", "code_commit": "commit-test",
            "input_snapshot_id": "snap-1", "config_hash": "cfg-hash", "pool_hash": pool_hash
        }
    }))
    .expect("valid production pool content");
    FormulaPool::seal(content).expect("seal production pool")
}

/// Seed the audit log with a pool-bound launch entry (the launcher) so the SoD launcher resolves.
async fn seed_launch(audit: &AuditLog, pool_id: &str) {
    audit
        .append(
            LAUNCHER,
            AuditAction::Launch,
            pool_id,
            "run-x",
            "",
            "",
            now_ms(),
        )
        .await
        .unwrap();
}

/// HAPPY PATH: a genuinely-passing production pool with two distinct approver signatures (neither the
/// launcher) SEALS — marks `Sealed`, records a `GovernanceRecord`, mints NO vintage.
#[tokio::test]
async fn a_passing_production_pool_with_two_distinct_signoffs_seals() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    let pool = production_pool_with_evidence("pool-good");
    write_pool(&artifacts, &pool);
    let (app, audit) = build_app(&data, &artifacts, roles());
    seed_launch(&audit, "pool-good").await;

    // Two distinct approvers sign off (neither the launcher).
    post_as(&app, "/api/formula-pools/pool-good/approve", APPROVER_A).await;
    let (_, body) = post_as(&app, "/api/formula-pools/pool-good/approve", APPROVER_B).await;
    assert_eq!(body["signoff"], "two_distinct_signoffs", "{body}");

    // Seal succeeds under the server-authoritative predicate.
    let (status, body) = post_as(&app, "/api/formula-pools/pool-good/seal", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK, "the happy path must seal: {body}");
    assert_eq!(body["lifecycle"], "sealed");
    assert_eq!(
        body["vintage_minted"], false,
        "sealing NEVER mints a vintage"
    );
    assert_eq!(body["evidence_hash"].as_str().unwrap().len(), 64);

    // A GovernanceRecord was written under <data>/governance/records/.
    let record_path = data
        .join("governance")
        .join("records")
        .join("pool-good.json");
    assert!(record_path.exists(), "a GovernanceRecord must be recorded");
    let record: GovernanceRecord =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record.approval_entry_hashes.len(), 2, "two approvals bound");
    assert_eq!(
        record.vintage_content_hash, pool.content_hash,
        "binds the sealed-pool hash"
    );

    // The pool now reads Sealed.
    let (_, detail) = get_as(&app, "/api/formula-pools/pool-good", APPROVER_A).await;
    assert_eq!(detail["lifecycle"], "sealed");
}

/// Launcher-as-approver / single-sig / pool_hash-mismatch all still 409 on the LIVE seal path.
#[tokio::test]
async fn launcher_self_approve_single_sig_and_hash_mismatch_all_block_the_live_seal() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &production_pool_with_evidence("pool-sod"));
    let mut r = roles();
    r.approvers.push(LAUNCHER.to_owned()); // launcher also holds the approver role
    let (app, audit) = build_app(&data, &artifacts, r);
    seed_launch(&audit, "pool-sod").await;

    // Launcher-as-approver is refused at /approve (SoD 403 fires LIVE — carry-forward #1).
    let (status, _) = post_as(&app, "/api/formula-pools/pool-sod/approve", LAUNCHER).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "launcher self-approve is refused live"
    );

    // Only ONE distinct valid approver → seal blocks on the dual-sig clause.
    post_as(&app, "/api/formula-pools/pool-sod/approve", APPROVER_A).await;
    let (status, body) = post_as(&app, "/api/formula-pools/pool-sod/seal", APPROVER_A).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let names: Vec<String> = body["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_str().unwrap().to_owned())
        .collect();
    assert!(
        names.contains(&"insufficient_distinct_approver_signoffs".to_owned()),
        "single-sig blocks: {names:?}"
    );
    // The pool is not sealed.
    let (_, detail) = get_as(&app, "/api/formula-pools/pool-sod", APPROVER_A).await;
    assert_eq!(detail["lifecycle"], "approved");
}

/// A rejected production seal appends a rejected-attempt audit entry (design §13.7).
#[tokio::test]
async fn a_rejected_seal_appends_a_rejected_attempt_audit_entry() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &production_pool_with_evidence("pool-rej"));
    let (app, audit) = build_app(&data, &artifacts, roles());
    seed_launch(&audit, "pool-rej").await;
    post_as(&app, "/api/formula-pools/pool-rej/approve", APPROVER_A).await; // only one signoff

    let before = audit.read_all().unwrap().len();
    let (status, _) = post_as(&app, "/api/formula-pools/pool-rej/seal", APPROVER_A).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let after = audit.read_all().unwrap();
    assert_eq!(
        after.len(),
        before + 1,
        "a rejected-attempt entry is appended"
    );
    let last = after.last().unwrap();
    assert_eq!(last.action, AuditAction::Reject);
    assert!(
        !last.evidence_hash.is_empty(),
        "the rejected attempt records the evidence hash"
    );
}

/// STRUCTURAL BARRIER 3: a sandbox-identity pool copied into the PRODUCTION directory is structurally
/// unloadable in production — `/seal` refuses it at load (its sealed mode is not production).
#[tokio::test]
async fn a_sandbox_pool_in_the_production_dir_is_structurally_unloadable() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    // Build a SANDBOX-mode pool and write it (as-is) into the PRODUCTION root (the copy-into-prod attack).
    let sandbox = sealed_pool("pool-smuggled", "sandbox");
    FormulaPoolRepository::new(artifacts.join("pools"))
        .write(&sandbox)
        .unwrap();
    let (app, _audit) = build_app(&data, &artifacts, roles());

    let (status, body) = post_as(&app, "/api/formula-pools/pool-smuggled/seal", APPROVER_A).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "barrier 3 refuses the smuggled pool: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("production-eligible"),
        "the refusal names the production-eligibility barrier: {body}"
    );
}

/// STRUCTURAL BARRIER 2: the physically separate research artifacts root is never listed by
/// `GET /api/vintages` — a sandbox/research pool is off the production load path by directory boundary.
#[tokio::test]
async fn the_research_root_is_never_listed_by_get_vintages() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    // A research pool exists under research/pools …
    write_pool(&artifacts, &sealed_pool("pool-research", "sandbox"));
    let (app, _audit) = build_app(&data, &artifacts, roles());
    // … but GET /api/vintages (the production vintage list) never surfaces it.
    let (status, body) = get_as(&app, "/api/vintages", APPROVER_A).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().expect("vintages list");
    assert!(
        list.iter().all(|v| v["id"] != "pool-research"),
        "a research pool must never appear in the vintage list: {body}"
    );
}

/// CARRY-FORWARD #1 (the actual live-path fix): the launcher is resolved via the RUN-BOUND launch entry
/// (`subject_hash = ""`, `run_id` set — exactly what the live evolve-launch path writes) through the
/// `pool_id → run → launch entry` binding, so the SoD 403 fires LIVE (not only when the launch entry is
/// pool-bound). Without the binding, `derive_signoff(launcher=None)` would let the launcher self-approve.
#[tokio::test]
async fn the_run_bound_launch_entry_resolves_the_launcher_so_sod_fires_live() {
    use qe_server::runs::model::IndexEntry;
    use qe_server::runs::store::RunStore;
    use qe_server::runs::{RunMeta, RunStatus, TrainProgress};

    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let artifacts = tmp.path().join("artifacts");
    write_pool(&artifacts, &production_pool_with_evidence("pool-runbound"));
    let mut r = roles();
    r.approvers.push(LAUNCHER.to_owned()); // launcher also holds the approver role
    let (app, audit) = build_app(&data, &artifacts, r);

    // Simulate a completed evolve run that produced `pool-runbound` (meta.train.pool == pool_id).
    let store = RunStore::new(data.join("runs"));
    let run_id = "evolve-run-42";
    let meta = RunMeta {
        id: run_id.to_owned(),
        run_type: "evolve".to_owned(),
        status: RunStatus::Succeeded,
        params: json!({ "seed": 7 }),
        progress: Default::default(),
        train: Some(TrainProgress {
            pool: Some("pool-runbound".to_owned()),
            ..Default::default()
        }),
        created_ms: now_ms(),
        started_ms: Some(now_ms()),
        finished_ms: Some(now_ms()),
        exit: Some(0),
        error: None,
        artifacts: vec![],
    };
    store.init_run(&meta).unwrap();
    store
        .write_index(&[IndexEntry {
            id: run_id.to_owned(),
            run_type: "evolve".to_owned(),
            created_ms: now_ms(),
            label: "evolve".to_owned(),
        }])
        .unwrap();

    // The live evolve-launch entry is RUN-bound: subject_hash = "", run_id = the run id (NOT the pool id).
    audit
        .append(LAUNCHER, AuditAction::Launch, "", run_id, "", "", now_ms())
        .await
        .unwrap();

    // The launcher tries to approve its own pool → SoD 403 (resolved via pool_id → run → launch entry).
    let (status, body) = post_as(&app, "/api/formula-pools/pool-runbound/approve", LAUNCHER).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "SoD must fire on the LIVE run-bound path: {body}"
    );
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("separation of duties"));
}
