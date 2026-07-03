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

use std::path::{Path, PathBuf};

use qe_cli::jobs::backtest::{run_backtest, BacktestParams};
use qe_cli::jobs::train::{run_train_job, TrainParams};
use qe_cli::jobs::ProgressLine;
use qe_determinism::Lineage;
use qe_signal::{CatalogueConfig, FeatureSchema};
use qe_vintage::VintageRepository;

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
        lineage: lineage(seed),
        profile: "train".to_owned(),
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

    // The result sidecar records the full G1 decision (5 criteria) for QE-261.
    assert_eq!(outcome.result.g1.criteria.len(), 5);
    assert_eq!(outcome.result.vintage_id, outcome.vintage_id);
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
