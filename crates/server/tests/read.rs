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

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use qe_determinism::Lineage;
use qe_risk::{CalibrationProfile, Fraction, PortfolioSizer, ShockConfig, SlippageCalibration};
use qe_server::{build_router, CliJobSpawner, ReadState, RunManager};
use qe_signal::{
    CatalogueIdentity, Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET,
    REP_VERSION,
};
use qe_storage::{MarketStore, DEFAULT_MAP_SIZE};
use qe_vintage::{
    DataProvenance, HoldoutReturnSeries, HoldoutSplit, RegimeShare, ResearchProvenance,
    SealEvidence, SteerDelta, TimeRange, Vintage, VintageContent, VintageRepository,
    VINTAGE_FORMAT_VERSION,
};
use rust_decimal::Decimal;
use serde_json::{json, Value};
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
    build_app_with_artifacts(tmp, fixtures_dir())
}

/// Like [`build_app`] but with a caller-chosen artifacts dir (so a test can seal its own vintage into a
/// tempdir). The run store is always rooted at `<tmp>/runs`, letting a test drop `meta.json`/`index.json`
/// there to exercise the QE-456 vintage→run reverse-join.
fn build_app_with_artifacts(tmp: &TempDir, artifacts: PathBuf) -> Router {
    let store_path = copy_store_to(tmp.path());
    let market_store = Arc::new(MarketStore::open(&store_path, DEFAULT_MAP_SIZE).unwrap());
    let vintages = VintageRepository::new(artifacts);
    let read = Arc::new(ReadState::new(vintages, market_store));

    let spawner = Arc::new(CliJobSpawner::new(PathBuf::from("qe")));
    let manager = Arc::new(RunManager::new(tmp.path().join("runs"), spawner, 2));
    let auth = common::auth_context(TEST_EMAIL, None);

    build_router(
        &tmp.path().join("static"),
        common::app_state_with_read(manager, auth, read),
    )
}

/// Seal a vintage with **populated** QE-467 evidence (a non-empty holdout series, a full seal-evidence
/// block, a steer delta, and a holdout split + regime composition) into `dir`, returning the sealed
/// artefact so a test can assert the endpoint reslices it byte-for-byte. Mirrors the `qe-vintage`
/// crate's own seal fixtures.
fn seal_rich_vintage(dir: &Path, id: &str) -> Vintage {
    let off = Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    };
    let mut clauses = [off; CLAUSES_PER_SET];
    clauses[0] = Clause {
        enabled: true,
        feature: 0,
        lo: 1,
        hi: 2,
    };
    let genome = Genome {
        version: REP_VERSION,
        long_entry: RuleSet {
            clauses,
            min_satisfied: 1,
        },
        short_entry: RuleSet {
            clauses: [off; CLAUSES_PER_SET],
            min_satisfied: 1,
        },
        exit: ExitParams {
            max_holding_bars: 10,
            exit_on_opposite: false,
        },
        risk: RiskParams { size_bps: 5_000 },
    };
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: id.to_owned(),
        chromosomes: vec![genome],
        weights: vec![1.0],
        calibration: CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()),
        slippage: SlippageCalibration::default(),
        sizer: PortfolioSizer::default(),
        shocks: ShockConfig::default(),
        worst_case_loss: Some(0.31),
        catalogue: CatalogueIdentity::current(),
        lineage: Lineage::new("cfg-hash", "snapshot", "commit", vec![7, 42]),
        seal_evidence: SealEvidence {
            dsr: 0.9,
            pbo: 0.12,
            spa_pvalue: 0.03,
            n_trials: 128,
            realised_turnover: 0.44,
            capacity_usd: 2_000_000.0,
            cost_stress_net_min: Some(0.15),
            ..SealEvidence::default()
        },
        holdout_series: HoldoutReturnSeries {
            returns: vec![0.01, -0.02, 0.03, 0.015],
        },
        provenance: ResearchProvenance {
            data_provenance: DataProvenance::Synthetic,
            holdout_split: HoldoutSplit {
                holdout_range: Some(TimeRange {
                    start: "2021-06-01".to_owned(),
                    end: "2021-07-01".to_owned(),
                }),
                train_range: Some(TimeRange {
                    start: "2020-01-01".to_owned(),
                    end: "2021-05-01".to_owned(),
                }),
                embargo_bars: 24,
            },
            regime_composition: vec![
                RegimeShare {
                    regime: "trend".to_owned(),
                    bars: 300,
                },
                RegimeShare {
                    regime: "chop".to_owned(),
                    bars: 120,
                },
            ],
            consultation_count: 2,
            steer_delta: Some(SteerDelta {
                indicator_subset_hash: "a".repeat(64),
                generations: 40,
                population: 12,
                windows: 6,
                folds: 4,
            }),
        },
    };
    let sealed = Vintage::seal(content).unwrap();
    VintageRepository::new(dir).write(&sealed).unwrap();
    sealed
}

/// Drop a **completed `train` run** on disk that produced `vintage` (its `meta.train.vintage`), plus its
/// `index.json` entry — the on-disk shape [`RunStore::find_runs_by_vintage`] scans for the reverse-join.
/// Writes the whole `index.json` from `runs` in one shot, so pass every run at once.
fn write_train_runs(runs_dir: &Path, runs: &[(&str, &str, u64)]) {
    std::fs::create_dir_all(runs_dir).unwrap();
    let index: Vec<Value> = runs
        .iter()
        .map(|(id, _vintage, created_ms)| {
            json!({ "id": id, "type": "train", "created_ms": created_ms, "label": "train" })
        })
        .collect();
    std::fs::write(
        runs_dir.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    for (id, vintage, created_ms) in runs {
        let dir = runs_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = json!({
            "id": id,
            "type": "train",
            "status": "succeeded",
            "params": {},
            "progress": { "pct": 100, "stage": "report", "msg": "done" },
            "train": { "vintage": vintage },
            "created_ms": created_ms,
            "started_ms": null,
            "finished_ms": null,
            "exit": 0,
            "error": null,
            "artifacts": [],
        });
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
    }
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
            // QE-464: the committed fixture predates provenance tagging ⇒ legacy `unknown` / uncalibrated.
            "provenance": "unknown",
            "calibrated": false,
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

// ---- QE-456 `GET /api/vintages/{id}` detail endpoint ------------------------------------------------

#[tokio::test]
async fn vintage_detail_reslices_the_committed_fixture() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, body) = send(&app, get_authed("/api/vintages/sample_vintage")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    assert_eq!(body["id"], "sample_vintage");
    assert_eq!(body["label"], "sample_vintage");
    assert_eq!(body["format_version"], 8);
    assert_eq!(body["data_provenance"], "real");
    // The 64-hex content hash pins the sealed artefact.
    assert_eq!(body["content_hash"].as_str().unwrap().len(), 64);

    // Composition: exactly one chromosome, weight 1.0, feature 0 resolved to a *catalogue* indicator id.
    let comp = body["composition"].as_array().unwrap();
    assert_eq!(comp.len(), 1, "one chromosome: {body}");
    assert_eq!(comp[0]["index"], 0);
    assert_eq!(comp[0]["weight"], 1.0);
    let inds = comp[0]["indicators"].as_array().unwrap();
    assert!(!inds.is_empty(), "genome references at least one indicator");
    let f0 = inds.iter().find(|i| i["feature"] == 0).unwrap();
    assert_eq!(f0["source"], "catalogue");
    assert!(
        f0["id"].as_str().is_some_and(|s| !s.is_empty()),
        "feature 0 resolves to a catalogue indicator id: {f0}"
    );

    // The persisted seal-evidence block is present and reads the sealed (default) values, never recomputed.
    assert!(body["seal_evidence"].is_object(), "{body}");
    assert_eq!(body["seal_evidence"]["n_trials"], 0);

    // The holdout series comes back as a HANDLE, never inline data.
    let series_handle = qe_vintage::VintageRepository::new(fixtures_dir())
        .load("sample_vintage")
        .unwrap()
        .content
        .holdout_series
        .handle()
        .unwrap();
    assert_eq!(body["holdout_series_handle"], series_handle);
    assert_eq!(body["holdout_series_len"], 0);
    assert!(
        find_key(&body, "returns").is_none(),
        "the raw holdout `returns` array must NOT be inlined anywhere in the body: {body}"
    );

    // Sidecars already sealed in the content are surfaced.
    assert!(body["sidecars"]["slippage"].is_object());
    assert!(body["sidecars"]["sizer"].is_object());
    assert!(body["sidecars"]["calibration"].is_object());
    assert!(body["sidecars"]["catalogue"].is_object());
}

#[tokio::test]
async fn vintage_detail_reslices_populated_evidence_and_keeps_the_series_a_handle() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let sealed = seal_rich_vintage(&artifacts, "rich_vintage");
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/rich_vintage")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    // Provenance + populated seal evidence are resliced exactly as sealed (no recompute, no reshape).
    assert_eq!(body["data_provenance"], "synthetic");
    assert_eq!(body["seal_evidence"]["dsr"], 0.9);
    assert_eq!(body["seal_evidence"]["n_trials"], 128);
    assert_eq!(body["seal_evidence"]["capacity_usd"], 2_000_000.0);
    assert_eq!(body["seal_evidence"]["cost_stress_net_min"], 0.15);
    assert_eq!(body["consultation_count"], 2);
    assert_eq!(body["holdout_split"]["embargo_bars"], 24);
    assert_eq!(
        body["holdout_split"]["holdout_range"]["start"],
        "2021-06-01"
    );
    assert_eq!(body["regime_composition"].as_array().unwrap().len(), 2);
    assert_eq!(body["steer_delta"]["generations"], 40);
    assert_eq!(body["sidecars"]["worst_case_loss"], 0.31);

    // The non-empty holdout series is returned ONLY as its content handle + length — never inline.
    let expected = sealed.content.holdout_series.handle().unwrap();
    assert_eq!(body["holdout_series_handle"], expected);
    assert_eq!(body["holdout_series_len"], 4);
    assert!(
        find_key(&body, "returns").is_none(),
        "a 4-point holdout series must not be inlined: {body}"
    );
}

#[tokio::test]
async fn vintage_detail_reverse_join_lists_producing_runs_deterministically() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    seal_rich_vintage(&artifacts, "rich_vintage");
    // Two train runs produced the same content-identical vintage; write them OUT of order to prove the
    // deterministic tie-break (earliest created_ms first, then lexicographic id) is applied on read.
    write_train_runs(
        &tmp.path().join("runs"),
        &[
            ("run-zzz", "rich_vintage", 2_000),
            ("run-aaa", "rich_vintage", 1_000),
            ("run-other", "some_other_vintage", 500),
        ],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/rich_vintage")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    let producers = body["producing_runs"].as_array().unwrap();
    let ids: Vec<&str> = producers
        .iter()
        .map(|r| r["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["run-aaa", "run-zzz"],
        "only the two producers, earliest created_ms first: {body}"
    );
    assert_eq!(producers[0]["run_type"], "train");
    assert_eq!(producers[0]["status"], "succeeded");
    assert_eq!(body["primary_run"], "run-aaa");
}

#[tokio::test]
async fn vintage_detail_unknown_id_is_404() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, body) = send(&app, get_authed("/api/vintages/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown id ⇒ 404: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|e| e.contains("not found")),
        "404 carries the read-module error body: {body}"
    );
}

#[tokio::test]
async fn vintage_detail_corrupt_artefact_is_500_not_a_panic() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    seal_rich_vintage(&artifacts, "rich_vintage");

    // Tamper the sealed artefact on disk: overwrite the stored `content_hash` so `Vintage::load`'s
    // hash verification fails (HashMismatch → DetailOutcome::Internal → 500), exercising AC #4
    // end-to-end. The file is `{ "content": {...}, "content_hash": "..." }`.
    let path = artifacts.join("rich_vintage.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["content_hash"] = Value::String("0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();

    let app = build_app_with_artifacts(&tmp, artifacts);
    let (status, body) = send(&app, get_authed("/api/vintages/rich_vintage")).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a corrupt/failing-verify artefact is a 500, never a panic: {body}"
    );
    assert!(
        body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "500 carries an error message body: {body}"
    );
}

#[tokio::test]
async fn vintage_detail_requires_a_session() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, _) = send(&app, get_no_session("/api/vintages/sample_vintage")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no session ⇒ 401");
}

// ---- QE-466 `GET /api/vintages/leaderboard` (informational, NOT a selector) ------------------------

/// A minimal single-chromosome genome for a sealed leaderboard fixture (mirrors `seal_rich_vintage`).
fn one_genome() -> Genome {
    let off = Clause {
        enabled: false,
        feature: 0,
        lo: 0,
        hi: 0,
    };
    let mut clauses = [off; CLAUSES_PER_SET];
    clauses[0] = Clause {
        enabled: true,
        feature: 0,
        lo: 1,
        hi: 2,
    };
    Genome {
        version: REP_VERSION,
        long_entry: RuleSet {
            clauses,
            min_satisfied: 1,
        },
        short_entry: RuleSet {
            clauses: [off; CLAUSES_PER_SET],
            min_satisfied: 1,
        },
        exit: ExitParams {
            max_holding_bars: 10,
            exit_on_opposite: false,
        },
        risk: RiskParams { size_bps: 5_000 },
    }
}

/// Seal a vintage with **caller-chosen** ranking metrics — the persisted net-of-cost figure, DSR, the
/// overlap-keyed consultation count and the holdout series — so a leaderboard test can drive the ranking +
/// consultation-budget enforcement directly. Every metric lands in the sealed content (QE-467), so the
/// endpoint READS it, never recomputes it.
#[allow(clippy::too_many_arguments)]
fn seal_leaderboard_vintage(
    dir: &Path,
    id: &str,
    provenance: DataProvenance,
    cost_net: Option<f64>,
    dsr: f64,
    consultation: u64,
    series: Vec<f64>,
) -> Vintage {
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: id.to_owned(),
        chromosomes: vec![one_genome()],
        weights: vec![1.0],
        calibration: CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()),
        slippage: SlippageCalibration::default(),
        sizer: PortfolioSizer::default(),
        shocks: ShockConfig::default(),
        worst_case_loss: Some(0.2),
        catalogue: CatalogueIdentity::current(),
        lineage: Lineage::new("cfg-hash", "snapshot", "commit", vec![7, 42]),
        seal_evidence: SealEvidence {
            dsr,
            pbo: 0.1,
            spa_pvalue: 0.02,
            n_trials: 64,
            realised_turnover: 0.33,
            capacity_usd: 1_500_000.0,
            cost_stress_net_min: cost_net,
            ..SealEvidence::default()
        },
        holdout_series: HoldoutReturnSeries { returns: series },
        provenance: ResearchProvenance {
            data_provenance: provenance,
            holdout_split: HoldoutSplit {
                holdout_range: None,
                train_range: None,
                embargo_bars: 12,
            },
            regime_composition: vec![RegimeShare {
                regime: "trend".to_owned(),
                bars: 100,
            }],
            consultation_count: consultation,
            steer_delta: Some(SteerDelta {
                indicator_subset_hash: "b".repeat(64),
                generations: 30,
                population: 10,
                windows: 5,
                folds: 3,
            }),
        },
    };
    let sealed = Vintage::seal(content).unwrap();
    VintageRepository::new(dir).write(&sealed).unwrap();
    sealed
}

fn post_authed(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("cookie", common::session_cookie_header(TEST_EMAIL))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn leaderboard_ranks_on_persisted_net_of_cost_and_shows_steer_diffs() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    // Both within budget; "vlo" has the higher net-of-cost so it must rank first — on the PERSISTED
    // cost-stress net, never gross Sharpe / equal-weight / lone Sharpe / in-sample.
    seal_leaderboard_vintage(
        &artifacts,
        "vlo",
        DataProvenance::Real,
        Some(0.05),
        0.6,
        1,
        vec![0.01, -0.01, 0.02, 0.0, 0.01],
    );
    seal_leaderboard_vintage(
        &artifacts,
        "vhi",
        DataProvenance::Real,
        Some(0.20),
        0.9,
        1,
        vec![0.02, 0.01, -0.02, 0.03, 0.0],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/leaderboard")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "both sealed vintages ranked: {body}");
    assert_eq!(
        entries[0]["id"], "vhi",
        "higher persisted net ranks first: {body}"
    );
    assert_eq!(entries[0]["rank"], 1);
    assert_eq!(entries[0]["cost_stress_net_min"], 0.20);
    assert_eq!(entries[1]["id"], "vlo");
    assert_eq!(entries[1]["rank"], 2);

    // Steer/param diffs are surfaced per vintage.
    assert_eq!(entries[0]["steer_delta"]["generations"], 30);
    assert_eq!(entries[0]["steer_delta"]["windows"], 5);

    // The forbidden ranking bases must be structurally ABSENT — no gross Sharpe, equal-weight, lone Sharpe,
    // or in-sample field anywhere in the body.
    for forbidden in [
        "gross_sharpe",
        "equal_weight",
        "sharpe",
        "in_sample",
        "gross",
    ] {
        assert!(
            find_key(&body, forbidden).is_none(),
            "the leaderboard must not expose `{forbidden}` (would be a QE-450 §13.5 inversion): {body}"
        );
    }
}

#[tokio::test]
async fn leaderboard_surfaces_cross_vintage_correlation_and_effective_n() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    // Series of unequal length (5 and 3): the correlation aligns to the common minimum, so effective N = 3.
    seal_leaderboard_vintage(
        &artifacts,
        "a",
        DataProvenance::Real,
        Some(0.10),
        0.7,
        1,
        vec![0.01, 0.02, 0.03, 0.04, 0.05],
    );
    seal_leaderboard_vintage(
        &artifacts,
        "b",
        DataProvenance::Real,
        Some(0.08),
        0.7,
        1,
        vec![0.01, 0.02, 0.03],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/leaderboard")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    assert!(
        body["cross_vintage_correlation"].is_number(),
        "a QE-430-deflated cross-vintage correlation is surfaced: {body}"
    );
    assert!(
        body["cross_vintage_correlation"].as_f64().unwrap() >= 0.0,
        "the positive-mean deflated correlation is >= 0: {body}"
    );
    assert_eq!(
        body["effective_n"], 3,
        "effective N is the common (aligned) series length: {body}"
    );
    assert!(
        body["effective_n_note"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the alignment caveat is stated: {body}"
    );
}

#[tokio::test]
async fn leaderboard_enforces_consultation_budget_demotes_and_escalates() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    // "over" has the BEST net-of-cost but its holdout was re-consulted (count 2 > budget 1). "clean" is
    // within budget with a worse net. Enforcement: "clean" must still rank ABOVE "over", and "over"'s DSR
    // bar is escalated — so re-running until the top slot improves is defeated, not rewarded.
    seal_leaderboard_vintage(
        &artifacts,
        "over",
        DataProvenance::Real,
        Some(0.99),
        0.95,
        2,
        vec![0.01, 0.02, 0.03],
    );
    seal_leaderboard_vintage(
        &artifacts,
        "clean",
        DataProvenance::Real,
        Some(0.10),
        0.6,
        1,
        vec![0.01, 0.02, 0.03],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/leaderboard")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(
        entries[0]["id"], "clean",
        "the within-budget vintage ranks first despite a worse net — enforcement, not display: {body}"
    );
    assert_eq!(entries[0]["over_consulted"], false);
    assert_eq!(entries[0]["dsr_status"], "ok");
    assert_eq!(
        entries[1]["id"], "over",
        "the over-consulted vintage is DEMOTED: {body}"
    );
    assert_eq!(entries[1]["over_consulted"], true);
    assert_eq!(
        entries[1]["dsr_status"], "escalated",
        "the over-consulted vintage's DSR bar is escalated/greyed: {body}"
    );

    // The chosen enforcement posture is stated and machine-checkable.
    assert_eq!(body["enforcement_posture"], "own-evidence-only");
    assert_eq!(body["consultation_budget"], 1);
}

#[tokio::test]
async fn leaderboard_is_read_only_and_rejects_mutating_verbs() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    seal_leaderboard_vintage(
        &artifacts,
        "v",
        DataProvenance::Real,
        Some(0.1),
        0.7,
        1,
        vec![0.01],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    // A POST to the leaderboard path is METHOD NOT ALLOWED — no mutating (promote/select/seal/auto-run)
    // handler is mounted; the surface is GET-only over sealed artefacts.
    let (status, _) = send(&app, post_authed("/api/vintages/leaderboard")).await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "the leaderboard mounts no mutating verb — read-only"
    );
}

#[tokio::test]
async fn leaderboard_exposes_no_promote_or_select_action_and_labels_not_paper_confirmed() {
    let tmp = TempDir::new().unwrap();
    let artifacts = tmp.path().join("artifacts");
    seal_leaderboard_vintage(
        &artifacts,
        "v1",
        DataProvenance::Real,
        Some(0.1),
        0.7,
        1,
        vec![0.01, 0.02],
    );
    seal_leaderboard_vintage(
        &artifacts,
        "v2",
        DataProvenance::Real,
        Some(0.2),
        0.8,
        1,
        vec![0.03, 0.01],
    );
    let app = build_app_with_artifacts(&tmp, artifacts);

    let (status, body) = send(&app, get_authed("/api/vintages/leaderboard")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    // No selection/promotion affordance anywhere in the payload.
    for forbidden in ["promote", "select", "seal", "winner", "auto_run", "best"] {
        assert!(
            find_key(&body, forbidden).is_none(),
            "a read-only inspection surface must expose no `{forbidden}` action/field: {body}"
        );
    }

    // Every vintage carries the "backtest-holdout only — not paper-confirmed" label; the board too.
    assert_eq!(body["not_paper_confirmed"], true);
    for e in body["entries"].as_array().unwrap() {
        assert_eq!(
            e["not_paper_confirmed"], true,
            "every entry is not-paper-confirmed: {body}"
        );
    }

    // The standing best-of-N caveat is present.
    assert!(
        body["caveat"]
            .as_str()
            .is_some_and(|c| c.contains("best-of-N") && c.contains("INSPECTION")),
        "the standing anti-selection caveat is stated: {body}"
    );
}

#[tokio::test]
async fn leaderboard_requires_a_session() {
    let tmp = TempDir::new().unwrap();
    let app = build_app(&tmp);

    let (status, _) = send(&app, get_no_session("/api/vintages/leaderboard")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no session ⇒ 401");
}

/// Recursively search a JSON value for a key (used to prove the holdout `returns` array is never inlined).
fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return Some(v);
            }
            map.values().find_map(|v| find_key(v, key))
        }
        Value::Array(arr) => arr.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}
