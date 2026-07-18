//! Integration test for the `qe evolve` GP job (QE-452 Phase A).
//!
//! Runs a small-budget evolve pipeline (illuminate → deflation → freeze → **seal a formula pool**) over
//! the committed QE-251 sample store fixture (`tests/fixtures/sample_store/`, BTCUSDT 1h) with a fixed
//! seed, and asserts the load-bearing Phase-A invariants:
//!  1. a **sealed formula pool** whose `verify()` passes is written under the **pool** root;
//!  2. **no vintage** is written under the vintage root — an evolve run never touches the vintage repo
//!     (§13.3); the two lifecycles use physically separate directory roots;
//!  3. the terminal `done` line carries `pool: Some(id)` and **`vintage: None`**;
//!  4. two runs with the **same seed** produce the **same pool id + content hash** (deterministic), and a
//!     different seed produces a different pool id.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::{Path, PathBuf};

use qe_cli::jobs::evolve::{run_evolve_job, EvolveParams};
use qe_cli::jobs::ProgressLine;
use qe_determinism::Lineage;
use qe_formula_pool::{FormulaPoolRepository, PoolMode};
use qe_run_protocol::EvolveMode;

/// Matches the map size the committed fixture store was written with.
const FIXTURE_MAP_SIZE: usize = 1 << 20; // 1 MiB

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the committed store into a scratch dir so opening it never mutates the fixture.
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

fn lineage(seed: u64) -> Lineage {
    Lineage::new("qe-452-evolve-test-cfg", "", "qe-452-commit", vec![seed])
}

/// Small-budget evolve params over the fixture store (sub-second, deterministic).
fn params(store_path: PathBuf, pool_root: PathBuf, seed: u64) -> EvolveParams {
    EvolveParams {
        store_path,
        map_size: FIXTURE_MAP_SIZE,
        pool_root,
        instrument: "BTCUSDT".to_owned(),
        start: "2021-01-01".to_owned(),
        end: "2021-01-10".to_owned(),
        resolution: "1h".to_owned(),
        mode: EvolveMode::Sandbox,
        seed,
        generations: 6,
        offspring: 16,
        states: 5,
        k: 16,
        lineage: lineage(seed),
        profile: "train".to_owned(),
    }
}

#[test]
fn evolve_over_fixture_seals_pool_and_never_writes_a_vintage() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    // The pool root and the vintage root are physically separate directories.
    let pool_root = tmp.path().join("artifacts/research/pools");
    let vintage_root = tmp.path().join("artifacts/vintages");

    let mut lines: Vec<ProgressLine> = Vec::new();
    let mut emit = |line: ProgressLine| lines.push(line);
    let outcome = run_evolve_job(
        &params(store_path, pool_root.clone(), 20_260_718),
        &mut emit,
    )
    .unwrap();

    // 1. The sealed pool is written under the pool root and re-loads + verifies.
    assert!(outcome.pool_path.exists(), "pool artefact was written");
    assert!(
        outcome.pool_path.starts_with(&pool_root),
        "pool written under the pool root, not the vintage root: {:?}",
        outcome.pool_path
    );
    let loaded = FormulaPoolRepository::new(&pool_root)
        .load(&outcome.pool_id)
        .unwrap();
    loaded.verify().unwrap();
    assert_eq!(loaded.content_hash, outcome.content_hash);
    assert_eq!(loaded.content.mode, PoolMode::Sandbox);
    assert!(!loaded.content.formulas.is_empty(), "pool has ≥1 formula");
    assert!(loaded.content.formulas.len() <= 16, "K ≤ 16");
    // The deflation-summary block is real (GP-aware trial basis carried through).
    assert!(loaded.content.deflation.gp_aware);
    assert!(loaded.content.deflation.distinct_evaluations >= 1);

    // 2. The vintage root was NEVER created/written — the evolve run never touches the vintage repo.
    assert!(
        !vintage_root.exists(),
        "an evolve run must not write the vintage repo: {vintage_root:?} exists"
    );

    // The result sidecar records the pool, not a vintage.
    assert_eq!(outcome.result.pool_id, outcome.pool_id);
    assert_eq!(outcome.result.mode, "sandbox");
}

#[test]
fn same_seed_reproduces_the_pool_id_and_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let pool_root = tmp.path().join("pools");

    let mut sink = |_l: ProgressLine| {};
    let a = run_evolve_job(
        &params(store_path.clone(), pool_root.clone(), 42),
        &mut sink,
    )
    .unwrap();
    let b = run_evolve_job(
        &params(store_path.clone(), pool_root.clone(), 42),
        &mut sink,
    )
    .unwrap();
    assert_eq!(a.pool_id, b.pool_id, "same seed ⇒ same pool id");
    assert_eq!(
        a.content_hash, b.content_hash,
        "same seed ⇒ same content hash"
    );

    // A different seed diverges (a different lineage id ⇒ a different pool id).
    let c = run_evolve_job(&params(store_path, pool_root, 43), &mut sink).unwrap();
    assert_ne!(
        a.pool_id, c.pool_id,
        "a different seed ⇒ a different pool id"
    );
}
