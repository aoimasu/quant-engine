//! QE-013 acceptance: a documented local run produces a vintage; every persistent-state location is
//! configurable with no hard-coded absolute paths; the Docker image runs the same binary.

use std::path::Path;

use qe_cli::{run_train, Vintage};
use qe_config::Config;

/// A config rooted at `data_root` so the test writes only inside a temp dir.
fn config_under(data_root: &Path) -> Config {
    let toml = format!(
        r#"
profile = "train"
instruments = ["BTCUSDT"]

[[universe]]
instrument = "BTCUSDT"
listed = "2019-09-08"

[[universe]]
instrument = "ETHUSDT"
listed = "2019-11-27"
delisted = "2025-01-01"

[bars]
base = "5m"
reconstructed = ["30m", "4h"]

[storage]
market_dir = "{root}/market"
synthetic_dir = "{root}/synthetic"
artifacts_dir = "{root}/artifacts"

[determinism]
seed = 42
"#,
        root = data_root.display()
    );
    Config::from_toml_str(&toml).expect("valid config")
}

#[test]
fn run_train_produces_a_vintage_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_under(tmp.path());

    let Vintage { id, manifest_path } = run_train(&cfg, "commit-abc").expect("train run");

    // AC #1: a vintage is produced — manifest exists at the content-addressed location.
    assert!(manifest_path.exists(), "manifest must be written");
    let expected = tmp
        .path()
        .join("artifacts/vintages")
        .join(id.as_str())
        .join("manifest.json");
    assert_eq!(manifest_path, expected);

    // The id is a valid 64-hex vintage hash, and the manifest records the full universe roster
    // (incl. the delisted ETH — no survivorship drop).
    assert_eq!(id.as_str().len(), 64);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(json["vintage_id"], id.as_str());
    assert_eq!(json["profile"], "train");
    let roster = json["universe"].as_array().unwrap();
    assert_eq!(roster.len(), 2);
    assert!(roster
        .iter()
        .any(|r| r["instrument"] == "ETHUSDT" && r["delisted_ms"].is_number()));
}

#[test]
fn vintage_is_deterministic_for_same_inputs() {
    // AC #1 (determinism): the SAME config + commit → identical vintage id and byte-identical
    // manifest, with no wall-clock dependence (re-running yields the same artefact).
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_under(tmp.path());

    let a = run_train(&cfg, "commit-xyz").unwrap();
    let first_bytes = std::fs::read(&a.manifest_path).unwrap();
    let b = run_train(&cfg, "commit-xyz").unwrap();
    let second_bytes = std::fs::read(&b.manifest_path).unwrap();

    assert_eq!(a.id, b.id, "same inputs must yield the same vintage id");
    assert_eq!(first_bytes, second_bytes, "manifest must be byte-identical");

    // A different code commit folds into the lineage and changes the vintage id.
    let c = run_train(&cfg, "commit-DIFFERENT").unwrap();
    assert_ne!(a.id, c.id);
}

#[test]
fn all_state_is_under_configured_dirs_no_absolutes() {
    // AC #2: state dirs are configurable and the run writes ONLY under the configured artifacts dir.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_under(tmp.path());
    let vintage = run_train(&cfg, "commit-abc").unwrap();

    // The manifest is inside the configured artifacts dir, which is inside our temp root.
    assert!(vintage.manifest_path.starts_with(tmp.path()));

    // The configured state dirs were created.
    for sub in ["market", "synthetic", "artifacts"] {
        assert!(tmp.path().join(sub).is_dir(), "{sub} dir must be created");
    }

    // AC #2: the *default* storage paths are relative — no hard-coded absolute paths.
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
    // AC #3 (structural, since Docker can't run in CI here): the Dockerfile builds qe-cli and runs
    // the `qe` binary as its entrypoint — the same binary as the local run.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Dockerfile");
    let text = std::fs::read_to_string(path).expect("Dockerfile exists");
    assert!(text.contains("cargo build --release --locked -p qe-cli"));
    assert!(text.contains(r#"ENTRYPOINT ["qe"]"#));
}
