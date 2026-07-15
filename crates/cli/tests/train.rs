//! QE-013 / QE-260 acceptance (config + packaging aspects that need no market store).
//!
//! The real training pipeline (search → ensemble → validation → G1 → seal) is exercised over the
//! committed sample store in `train_job.rs`. This file keeps the configuration / packaging guarantees:
//! the documented example config loads, the default storage paths are relative (no hard-coded
//! absolutes), and the Docker image runs the same `qe` binary as a local run.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::Path;

use qe_config::Config;

#[test]
fn default_storage_paths_are_relative_no_absolutes() {
    // AC: the *default* storage paths are relative — no hard-coded absolute paths anywhere.
    let defaults = Config::from_toml_str("").unwrap();
    for p in [
        &defaults.storage.market_dir,
        &defaults.storage.synthetic_dir,
        &defaults.storage.artifacts_dir,
    ] {
        assert!(
            !Path::new(p).is_absolute(),
            "default storage path `{p}` must be relative"
        );
    }
}

#[test]
fn example_config_loads_and_validates() {
    // The documented one-command run uses config.example.toml — it must load + validate.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");
    let cfg = Config::load(qe_config::Profile::Train, &path).expect("example config loads");
    assert!(!cfg.universe().unwrap().is_empty());
}

#[test]
fn dockerfile_runs_the_same_binary() {
    // AC (structural, since Docker can't run in CI here): the Dockerfile builds qe-cli and runs the
    // `qe` binary as its entrypoint — the same binary as the local run.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Dockerfile");
    let text = std::fs::read_to_string(path).expect("Dockerfile exists");
    assert!(text.contains("cargo build --release --locked -p qe-cli"));
    assert!(text.contains(r#"ENTRYPOINT ["qe"]"#));
    // QE-420: the image threads a real build commit in via an ARG and exposes it as the
    // `QE_CODE_COMMIT` runtime override so the container stamps its build SHA into vintage lineage.
    assert!(text.contains("ARG QE_CODE_COMMIT"));
    assert!(text.contains("ENV QE_CODE_COMMIT=$QE_CODE_COMMIT"));
}
