//! Integration test for the `qe-cli train` search job (QE-260).
//!
//! Runs a small-budget training pipeline (MAP-Elites search → ensemble → validation → G1 gate → seal)
//! over the committed QE-251 sample store fixture (`tests/fixtures/sample_store/`, BTCUSDT 1h, 120 bars)
//! with a fixed seed, and asserts:
//!  1. a **sealed vintage** whose `verify()` passes is written, with a progress stream covering
//!     generations (+ archive coverage) → ensemble (CV folds) → G1 gate result;
//!  2. the sealed vintage is **backtestable by QE-251** (`run_backtest` over the same store) — the
//!     direct catalogue-schema alignment proof;
//!  3. two runs with the **same seed** produce the **same vintage id and content hash** (deterministic),
//!     and a different seed produces a different vintage id.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::{Path, PathBuf};

use qe_cli::jobs::backtest::{run_backtest, BacktestParams};
use qe_cli::jobs::train::{run_train_job, TrainParams};
use qe_cli::jobs::{ProgressLine, RunError};
use qe_determinism::Lineage;
use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, Timestamp};
use qe_signal::{CatalogueConfig, FeatureSchema};
use qe_storage::MarketStore;
use qe_vintage::VintageRepository;
use rust_decimal::Decimal;

/// Matches the map size the committed fixture store was written with (`backtest_job.rs`).
const FIXTURE_MAP_SIZE: usize = 1 << 20; // 1 MiB

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the committed store into a scratch dir so opening it (a write txn for schema init) never mutates
/// the fixture. (Same helper shape as `backtest_job.rs`.)
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

/// A deterministic lineage for the test (the sealed vintage id is `lineage.id()`).
fn lineage(seed: u64) -> Lineage {
    Lineage::new("qe-260-train-test-cfg", "", "qe-260-commit", vec![seed])
}

/// Small-budget params over the fixture store (sub-second, deterministic).
fn params(store_path: PathBuf, vintage_root: PathBuf, seed: u64) -> TrainParams {
    TrainParams {
        store_path,
        map_size: FIXTURE_MAP_SIZE,
        vintage_root,
        instrument: "BTCUSDT".to_owned(),
        start: "2021-01-01".to_owned(),
        end: "2021-01-10".to_owned(), // 9 days > 120 hours
        resolution: "1h".to_owned(),
        seed,
        generations: 4,
        // 31 holdout bars → 30 holdout returns (returns = bars − 1), meeting G1's default
        // `min_holdout_samples = 30` (matches the corrected `DEFAULT_TRAIN_HOLDOUT`).
        population: 16,
        holdout: 31,
        embargo: 2,
        // The committed fixture store has full 8h funding coverage (stamps every 8 bars over 120 bars),
        // so the default 0.90 floor is comfortably cleared.
        funding_coverage_min: 0.90,
        // QE-415: 2 purged OOS folds keeps the small-budget fixture search fast (test blocks partition the
        // ~87-bar train window, so per-genome eval stays ~one pass over the train bars).
        cv_folds: 2,
        // QE-443: default OFF ⇒ the deployed weights keep the equal-weight `1/N` seed (goldens unchanged).
        seed_weighting: qe_ensemble::SeedWeighting::Equal,
        lineage: lineage(seed),
        profile: "train".to_owned(),
        // Un-steered by default (byte-identical to the pre-QE-458 seal).
        indicator_subset: None,
        windows: None,
        folds: None,
        // Plain train (not a composite-flow sub-job) ⇒ no frozen-holdout lineage; byte-identical seal.
        flow: false,
    }
}

fn catalogue_schema() -> FeatureSchema {
    FeatureSchema::from_catalogue(&CatalogueConfig::default())
}

#[test]
fn train_over_fixture_store_seals_verifiable_vintage() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let vintage_root = tmp.path().join("artifacts/vintages");

    let mut lines: Vec<ProgressLine> = Vec::new();
    let outcome = run_train_job(&params(store_path, vintage_root.clone(), 42), &mut |line| {
        lines.push(line)
    })
    .expect("train job runs");

    // (1) A sealed vintage was written at <root>/<id>.json and re-loads + verifies.
    let vintage_path = vintage_root.join(format!("{}.json", outcome.vintage_id));
    assert!(vintage_path.exists(), "sealed vintage file must exist");
    assert_eq!(outcome.vintage_path, vintage_path);
    let loaded = VintageRepository::new(&vintage_root)
        .load(&outcome.vintage_id)
        .expect("sealed vintage loads");
    loaded.verify().expect("sealed vintage verifies");
    assert!(
        !loaded.content.chromosomes.is_empty(),
        "vintage must carry chromosomes"
    );
    assert_eq!(
        loaded.content.weights.len(),
        loaded.content.chromosomes.len(),
        "weights aligned to chromosomes"
    );

    // Catalogue-schema alignment: every sealed chromosome is valid against the SAME schema the QE-251
    // backtest job assembles against (`CatalogueConfig::default()`).
    let schema = catalogue_schema();
    for g in &loaded.content.chromosomes {
        assert!(
            g.is_valid(&schema),
            "sealed chromosome must be valid against the default catalogue schema"
        );
    }

    // (2) The progress stream covers generations (+ coverage) → ensemble (folds) → gate.
    let gen_lines: Vec<&ProgressLine> = lines
        .iter()
        .filter(|l| matches!(l, ProgressLine::Gen { .. }))
        .collect();
    assert_eq!(
        gen_lines.len(),
        4,
        "one gen line per generation, got {}",
        gen_lines.len()
    );
    // The archive genuinely filled niches (coverage grows over the search).
    let final_coverage = gen_lines
        .iter()
        .rev()
        .find_map(|l| match l {
            ProgressLine::Gen { coverage, .. } => Some(*coverage),
            _ => None,
        })
        .unwrap();
    assert!(final_coverage > 0, "MAP-Elites coverage must be > 0");

    assert!(
        lines
            .iter()
            .any(|l| matches!(l, ProgressLine::Ensemble { folds, .. } if *folds > 0)),
        "an ensemble line with CV folds must be emitted"
    );
    let gate = lines
        .iter()
        .find_map(|l| match l {
            ProgressLine::Gate { promoted, .. } => Some(*promoted),
            _ => None,
        })
        .expect("a G1 gate line must be emitted");
    // The gate ran and recorded a verdict (a 120-bar fixture is not expected to *pass* strict G1).
    let _ = gate;

    // The result sidecar records the full G1 decision (6 criteria, incl. the QE-437 PBO gate) for QE-261.
    assert_eq!(outcome.result.g1.criteria.len(), 6);
    assert_eq!(outcome.result.vintage_id, outcome.vintage_id);

    // QE-414: on the real fixture archive the DSR trial variance is estimated from the FULL cell
    // population (every occupied cell's champion), not the top-`MAX_POOL=10` ensemble pool. Prove the
    // population is genuinely broader than the top-10 pool and that the deflation basis is recorded.
    let robustness = &outcome.result.robustness;
    assert!(
        robustness.variance_trials > outcome.result.pool_size,
        "trial variance must be estimated from the full cell population ({}), broader than the top-N \
         ensemble pool ({})",
        robustness.variance_trials,
        outcome.result.pool_size,
    );
    assert!(
        robustness.variance_trials > 10,
        "the fixture archive must fill > 10 cells so the change is not inert (got {})",
        robustness.variance_trials,
    );
    assert!(
        robustness.trial_variance >= 0.0 && robustness.trial_variance.is_finite(),
        "the trial variance (deflation basis) must be recorded, got {}",
        robustness.trial_variance,
    );

    // QE-430: the effective N behind the correlation penalty is recorded in the ensemble score record,
    // consistently with the selected membership — `> 0` iff ≥ 2 members were selected (a penalty rests on
    // a pair), and it is a fold-slice sample size, never larger than the train window.
    let n_selected = outcome.result.selected.len();
    let eff_n = outcome.result.ensemble_corr_effective_n;
    if n_selected >= 2 {
        assert!(
            eff_n > 0,
            "≥2 selected members ⇒ the correlation penalty rests on a pair, so effective N > 0 (got {eff_n})"
        );
    } else {
        assert_eq!(
            eff_n, 0,
            "a single-member ensemble's correlation penalty rests on no pair ⇒ effective N == 0"
        );
    }
}

/// QE-416 AC (b) + (c): the sealed vintage carries a real worst-case-loss figure (not `None`), and its
/// calibration profile has a per-strategy entry for **every** member under the canonical strategy ids —
/// so `BreakerLayer::from_calibration` finds each one and pre-gates none (the vintage trades live).
#[test]
fn sealed_vintage_has_worst_case_loss_and_calibrates_every_strategy() {
    use qe_risk::DEFAULT_FAST_WINDOW;
    use qe_runtime::BreakerLayer;

    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let vintage_root = tmp.path().join("artifacts/vintages");

    let outcome = run_train_job(&params(store_path, vintage_root.clone(), 42), &mut |_| {})
        .expect("train job runs");
    let loaded = VintageRepository::new(&vintage_root)
        .load(&outcome.vintage_id)
        .expect("sealed vintage loads");
    let content = &loaded.content;

    // (b) worst_case_loss is Some, and a finite non-negative fraction (validated at seal, re-checked here).
    let wcl = content
        .worst_case_loss
        .expect("QE-416: sealed vintage must carry a worst-case-loss figure (not None)");
    assert!(
        wcl.is_finite() && wcl >= 0.0,
        "worst_case_loss must be a finite non-negative fraction, got {wcl}"
    );

    // (c) every sealed strategy is present in the calibration profile under its canonical id …
    let ids = content.strategy_ids();
    assert_eq!(ids.len(), content.chromosomes.len());
    for id in &ids {
        assert!(
            content.calibration.per_strategy.contains_key(id),
            "strategy `{id}` is missing from the calibration profile — it would be pre-gated"
        );
    }

    // … so `from_calibration` pre-gates none of them (before any equity is observed) and the profile is
    // wired for the full member set — the opposite of the empty-map placeholder that gated everything.
    let layer = BreakerLayer::from_calibration(&content.calibration, &ids, DEFAULT_FAST_WINDOW);
    assert_eq!(layer.strategy_count(), content.chromosomes.len());
    for i in 0..content.chromosomes.len() {
        assert!(
            !layer.is_gated(i),
            "member {i} was pre-gated despite being calibrated (unintended pre-gate)"
        );
    }
}

#[test]
fn sealed_vintage_is_backtestable_by_qe251() {
    // Alignment proof: a vintage sealed by `train` loads + runs through the QE-251 backtest job over the
    // same store window (the schemas match, so `check_schema` inside the backtest passes).
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let vintage_root = tmp.path().join("artifacts/vintages");

    let outcome = run_train_job(
        &params(store_path.clone(), vintage_root.clone(), 42),
        &mut |_| {},
    )
    .expect("train job runs");

    let bt = BacktestParams {
        store_path,
        map_size: FIXTURE_MAP_SIZE,
        vintage_root,
        vintage_id: outcome.vintage_id.clone(),
        strategy: None,
        start: "2021-01-01".to_owned(),
        end: "2021-01-10".to_owned(),
        resolution: "1h".to_owned(),
        universe: vec!["BTCUSDT".to_owned()],
        taker_fee_bps: 2.0,
        slippage_model: "square-root-impact".to_owned(),
        // QE-428: match the selection cost model's size-impact (default) — this train→backtest
        // integration test now reports net-of-cost PnL consistent with what the search selected on.
        reporting_impact: None,
    };
    let doc = run_backtest(&bt, &mut |_, _, _| {}).expect("sealed vintage backtests");
    assert_eq!(doc.strategy.name, outcome.vintage_id);
    assert!(
        !doc.equity_curve.is_empty(),
        "the backtest produced an equity curve from the sealed ensemble"
    );
}

#[test]
fn train_is_deterministic_for_a_fixed_seed() {
    // Two independent runs with the same seed produce a byte-identical sealed vintage (same id AND same
    // content hash — the search / ensemble / lineage are all deterministic).
    let run = |seed: u64| {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = copy_store_to(tmp.path());
        let vintage_root = tmp.path().join("artifacts/vintages");
        let outcome = run_train_job(&params(store_path, vintage_root, seed), &mut |_| {})
            .expect("train job runs");
        (outcome.vintage_id, outcome.content_hash)
    };

    let (id_a, hash_a) = run(42);
    let (id_b, hash_b) = run(42);
    assert_eq!(id_a, id_b, "same seed ⇒ same vintage id");
    assert_eq!(hash_a, hash_b, "same seed ⇒ byte-identical sealed content");

    // A different seed changes the vintage id (the seed folds into the lineage).
    let (id_c, _) = run(7);
    assert_ne!(id_a, id_c, "a different seed must change the vintage id");
}

/// Epoch-ms of 2021-01-01 (matches the window in `params`).
const START_MS: i64 = 18_628 * 86_400_000;
const HOUR_MS: i64 = 3_600_000;

/// Write a store with 120 hourly bars but **no funding stamps**, so the QE-403 funding-coverage gate must
/// fire. Prices swing (a triangle wave) so feature assembly is well-defined.
fn write_no_funding_store(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("no_funding_store");
    std::fs::create_dir_all(&path).unwrap();
    let store = MarketStore::open(&path, FIXTURE_MAP_SIZE).unwrap();
    let inst = InstrumentId::new("BTCUSDT").unwrap();
    let bars: Vec<Bar> = (0..120i64)
        .map(|i| {
            let phase = i % 24;
            let tri = if phase <= 12 { phase } else { 24 - phase };
            let mid =
                Decimal::from(100) + Decimal::from(tri) * Decimal::new(15, 1) - Decimal::from(9);
            let t = Timestamp::from_millis(START_MS + i * HOUR_MS);
            Bar::new(
                t,
                Resolution::H1,
                Price::new(mid).unwrap(),
                Price::new(mid + Decimal::new(5, 1)).unwrap(),
                Price::new(mid - Decimal::new(5, 1)).unwrap(),
                Price::new(mid).unwrap(),
                Qty::new(Decimal::from(1000)).unwrap(),
                100,
            )
            .unwrap()
        })
        .collect();
    store.put_bars(&inst, &bars).unwrap();
    // Deliberately no `put_funding` — the coverage gate must catch the gap.
    path
}

#[test]
fn no_funding_window_trips_coverage_gate_and_seals_nothing() {
    // QE-403 AC: a training run over a window with no funding data errors (an explicit "funding coverage
    // 0%" gate failure) rather than sealing.
    let tmp = tempfile::tempdir().unwrap();
    let store_path = write_no_funding_store(tmp.path());
    let vintage_root = tmp.path().join("artifacts/vintages");

    let err = run_train_job(&params(store_path, vintage_root.clone(), 42), &mut |_| {})
        .expect_err("a funding-free window must not seal");

    match err {
        RunError::FundingCoverage {
            present,
            coverage_pct,
            threshold_pct,
            ..
        } => {
            assert_eq!(present, 0, "the store has no funding stamps");
            assert_eq!(coverage_pct, 0, "coverage must read 0%");
            assert_eq!(threshold_pct, 90, "the default floor is 90%");
        }
        other => panic!("expected RunError::FundingCoverage, got {other:?}"),
    }

    // Nothing was sealed.
    let sealed_any = vintage_root.exists()
        && std::fs::read_dir(&vintage_root)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
    assert!(
        !sealed_any,
        "no vintage may be written when the funding gate fails"
    );
}

// ---- QE-458 steered-search live-path wiring (cardinality→N, steer delta, coverage floor) -------------

/// A steered params over the fixture: the indicator subset is the FULL catalogue in catalogue order, so
/// the search draw sequence is byte-identical to the un-steered run (a full allowlist in order collapses
/// `allowed[below(rng,len)]` back to `below(rng,len)`) — isolating the steer's effect to the recorded
/// trial basis `N` and the steer delta, which makes the assertions exact.
fn steered_full_catalogue_params(
    store_path: PathBuf,
    vintage_root: PathBuf,
    seed: u64,
) -> TrainParams {
    let ids: Vec<String> = catalogue_schema().ids().to_vec();
    TrainParams {
        indicator_subset: Some(ids),
        ..params(store_path, vintage_root, seed)
    }
}

#[test]
fn steered_run_records_steer_delta_raises_the_trial_basis_and_stays_golden_for_unsteered() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());

    // Un-steered run (the golden path).
    let un_root = tmp.path().join("artifacts/unsteered");
    let un = run_train_job(
        &params(store_path.clone(), un_root.clone(), 42),
        &mut |_| {},
    )
    .expect("un-steered train runs");
    let un_v = VintageRepository::new(&un_root)
        .load(&un.vintage_id)
        .unwrap();

    // Steered run (full-catalogue subset ⇒ identical search, isolating the steer effect).
    let st_root = tmp.path().join("artifacts/steered");
    let st = run_train_job(
        &steered_full_catalogue_params(store_path.clone(), st_root.clone(), 42),
        &mut |_| {},
    )
    .expect("steered train runs (coverage floor cleared)");
    let st_v = VintageRepository::new(&st_root)
        .load(&st.vintage_id)
        .unwrap();

    // (e) The un-steered vintage records NO steer delta; the steered one records the applied delta with the
    // subset hash over the catalogue in play + the real budget/window/fold counts.
    assert!(
        un_v.content.provenance.steer_delta.is_none(),
        "an un-steered run must not record a steer delta (byte-identical provenance)"
    );
    let delta = st_v
        .content
        .provenance
        .steer_delta
        .clone()
        .expect("a steered run must record a steer delta");
    let full_ids: Vec<String> = catalogue_schema().ids().to_vec();
    assert_eq!(
        delta.indicator_subset_hash,
        qe_vintage::SteerDelta::subset_hash(&full_ids, &[]),
        "the recorded subset hash must be the hash of the catalogue in play"
    );
    assert_eq!(delta.generations, 4);
    assert_eq!(delta.windows, 2); // un-steered windows default
    assert_eq!(delta.folds, 2); // == cv_folds when --folds is not set

    // (a) Cardinality feeds the LIVE trial basis: the steered run's recorded N is strictly greater than the
    // equivalent un-steered run's (multiplied by the available-feature-space size), and >= it (monotone).
    let un_n = un_v.content.seal_evidence.n_trials;
    let st_n = st_v.content.seal_evidence.n_trials;
    assert!(
        st_n > un_n,
        "steered N ({st_n}) must exceed un-steered N ({un_n}) — cardinality feeds N live"
    );

    // The steer changes the sealed artefact (steer delta + N) but NOT the lineage id (same seed/config).
    assert_eq!(
        st.vintage_id, un.vintage_id,
        "the vintage id is the lineage id — independent of the steer"
    );
    assert_ne!(
        st_v.content_hash, un_v.content_hash,
        "the steered vintage must be a distinct content-addressed artefact"
    );

    // Golden stability: a second un-steered run reproduces the un-steered content hash byte-for-byte.
    let un_root2 = tmp.path().join("artifacts/unsteered2");
    let un2 = run_train_job(&params(store_path, un_root2.clone(), 42), &mut |_| {})
        .expect("un-steered train re-runs");
    let un2_v = VintageRepository::new(&un_root2)
        .load(&un2.vintage_id)
        .unwrap();
    assert_eq!(
        un2_v.content_hash, un_v.content_hash,
        "the un-steered seal must be deterministic / byte-identical (golden unchanged)"
    );
}

#[test]
fn steered_run_with_an_unknown_indicator_errors_rather_than_running_unsteered() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let vintage_root = tmp.path().join("artifacts/vintages");

    let bad = TrainParams {
        indicator_subset: Some(vec!["not_a_real_indicator".to_owned()]),
        ..params(store_path, vintage_root.clone(), 42)
    };
    let err = run_train_job(&bad, &mut |_| {}).expect_err("an unknown indicator must error");
    assert!(
        matches!(err, RunError::UnknownIndicator { ref id } if id == "not_a_real_indicator"),
        "a misnamed steer indicator must be rejected, not silently run over the full catalogue: {err:?}"
    );
    // Nothing was sealed.
    let sealed_any = vintage_root.exists()
        && std::fs::read_dir(&vintage_root)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
    assert!(
        !sealed_any,
        "no vintage may be sealed for an unknown-indicator steer"
    );
}

// ---- QE-460 composite-flow train sub-job: frozen-holdout lineage recording -----------------------

/// Build `--flow` train params over the fixture with a holdout large enough to span ≥ 2 regimes.
fn flow_params(store_path: PathBuf, vintage_root: PathBuf, seed: u64) -> TrainParams {
    TrainParams {
        flow: true,
        holdout: 60,
        embargo: 2,
        ..params(store_path, vintage_root, seed)
    }
}

/// AC2/AC3: a `--flow` seal records the frozen holdout split + regime composition into the vintage lineage
/// (QE-467 schema), and the resolved holdout window is the SINGLE consultation the flow's backtest runs on
/// (the recorded holdout range), not a disjoint OOS window.
#[test]
fn flow_seal_records_frozen_holdout_split_and_regime_composition() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let root = tmp.path().join("artifacts/flow");
    let outcome = run_train_job(&flow_params(store_path, root.clone(), 42), &mut |_| {})
        .expect("flow train seals");
    let v = VintageRepository::new(&root)
        .load(&outcome.vintage_id)
        .unwrap();
    let prov = &v.content.provenance;

    // The frozen split {holdout_range, train_range, embargo} is recorded.
    let split = &prov.holdout_split;
    assert!(split.holdout_range.is_some(), "holdout_range recorded");
    assert!(split.train_range.is_some(), "train_range recorded");
    assert_eq!(split.embargo_bars, 2);

    // The holdout is regime-stratified: ≥ 2 distinct QE-125 regime labels, and the bar tallies reconcile.
    assert!(
        prov.regime_composition.len() >= 2,
        "holdout spans ≥ 2 regimes: {:?}",
        prov.regime_composition
    );
    assert!(prov.regime_composition.iter().all(|r| r.bars > 0));

    // First-consultation of a fresh holdout ⇒ count == 1.
    assert_eq!(prov.consultation_count, 1);

    // AC2 single consultation: the flow's backtest window IS the recorded holdout (evaluates ON it). The
    // handoff window's start is the calendar date of the recorded holdout range's first bar.
    let hw = outcome
        .result
        .holdout_window
        .expect("flow records the holdout window");
    let holdout_start_ms: i64 = split.holdout_range.as_ref().unwrap().start.parse().unwrap();
    let expect_day = {
        // format_ymd(holdout_start_ms) — recompute the civil date the flow recorded.
        let days = holdout_start_ms.div_euclid(86_400_000);
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = y + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}")
    };
    assert_eq!(
        hw.start, expect_day,
        "backtest window start = the recorded holdout range's first bar"
    );
    assert_eq!(
        hw.end, "2021-01-10",
        "backtest window end = the flow-window (snapshot) right edge"
    );
}

/// AC6 determinism: two `--flow` seals from the same seed + snapshot in FRESH repos reproduce the vintage
/// byte-identically (content hash), including the frozen-holdout lineage.
#[test]
fn flow_seal_is_byte_identical_for_the_same_seed_in_a_fresh_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let a = run_train_job(
        &flow_params(store_path.clone(), tmp.path().join("art/a"), 42),
        &mut |_| {},
    )
    .expect("flow a");
    let b = run_train_job(
        &flow_params(store_path, tmp.path().join("art/b"), 42),
        &mut |_| {},
    )
    .expect("flow b");
    assert_eq!(a.vintage_id, b.vintage_id, "same seed ⇒ same vintage id");
    assert_eq!(
        a.content_hash, b.content_hash,
        "same seed + snapshot ⇒ byte-identical flow vintage (incl. frozen-holdout lineage)"
    );
}

/// AC4: the consultation counter is overlap-keyed — a second flow whose holdout INTERSECTS a prior sealed
/// flow's holdout (different sizes, so not exact equality) records `consultation_count == 2`.
#[test]
fn flow_consultation_counter_is_overlap_keyed_not_equality() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    // Both flows seal into the SAME repo (a campaign) so the counter sees the prior vintage.
    let root = tmp.path().join("artifacts/campaign");

    // Flow A: holdout of 60 bars (the last 60).
    let a = run_train_job(
        &TrainParams {
            flow: true,
            holdout: 60,
            embargo: 2,
            ..params(store_path.clone(), root.clone(), 1)
        },
        &mut |_| {},
    )
    .expect("flow a");
    let va = VintageRepository::new(&root).load(&a.vintage_id).unwrap();
    assert_eq!(
        va.content.provenance.consultation_count, 1,
        "A is the first consultation"
    );

    // Flow B: holdout of 40 bars (the last 40) ⊂ A's holdout — a DIFFERENT range that still INTERSECTS A's,
    // so the overlap-keyed counter (not exact equality) counts A.
    let b = run_train_job(
        &TrainParams {
            flow: true,
            holdout: 40,
            embargo: 2,
            ..params(store_path, root.clone(), 2)
        },
        &mut |_| {},
    )
    .expect("flow b");
    let vb = VintageRepository::new(&root).load(&b.vintage_id).unwrap();
    // The two holdout ranges are not equal (60 vs 40 bars) …
    assert_ne!(
        va.content.provenance.holdout_split.holdout_range,
        vb.content.provenance.holdout_split.holdout_range,
        "the two holdouts are different ranges (not exact equality)"
    );
    // … yet B counts A because their bars intersect (overlap-keyed budget).
    assert_eq!(
        vb.content.provenance.consultation_count, 2,
        "B's holdout overlaps A's ⇒ count 2"
    );
}

/// A plain (non-flow) train leaves the frozen-holdout lineage EMPTY — un-flow vintages are unaffected by
/// QE-460 (byte-identical seal, no golden move).
#[test]
fn plain_train_records_no_frozen_holdout_lineage() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let root = tmp.path().join("artifacts/plain");
    let o = run_train_job(&params(store_path, root.clone(), 42), &mut |_| {}).expect("plain train");
    let v = VintageRepository::new(&root).load(&o.vintage_id).unwrap();
    let prov = &v.content.provenance;
    assert!(
        prov.holdout_split.holdout_range.is_none(),
        "no holdout range on a plain train"
    );
    assert!(
        prov.regime_composition.is_empty(),
        "no regime composition on a plain train"
    );
    assert_eq!(
        prov.consultation_count, 0,
        "no consultation recorded on a plain train"
    );
    assert!(
        o.result.holdout_window.is_none(),
        "no holdout window on a plain train result"
    );
}
