//! Layering integration tests: base TOML file overridden by `QE_`-prefixed env vars.
//!
//! Uses `figment::Jail` so the temp file + env mutations are scoped to the test (safe under the
//! parallel test runner).
//!
//! `figment::Error` is a large enum and `Jail::expect_with`'s closure return type is fixed by
//! the figment API (we can't box it), so the `result_large_err` lint is allowed for this
//! test-only file.
#![allow(clippy::result_large_err)]

use figment::Jail;
use qe_config::{Config, Profile};
use std::path::Path;

const BASE: &str = r#"
profile = "train"
instruments = ["BTCUSDT"]

[bars]
base = "5m"
reconstructed = ["30m", "4h"]

[storage]
market_dir = "data/lmdb/market"
synthetic_dir = "data/lmdb/synthetic"
artifacts_dir = "data/artifacts"

[determinism]
seed = 7
"#;

#[test]
fn file_loads_without_env() {
    Jail::expect_with(|jail| {
        jail.create_file("config.toml", BASE)?;
        let cfg = Config::load(Path::new("config.toml")).expect("load");
        assert_eq!(cfg.profile, Profile::Train);
        assert_eq!(cfg.bars.base, "5m");
        assert_eq!(cfg.instruments, vec!["BTCUSDT".to_owned()]);
        Ok(())
    });
}

#[test]
fn env_overrides_file_value() {
    Jail::expect_with(|jail| {
        jail.create_file("config.toml", BASE)?;
        // string override avoids any numeric-coercion ambiguity; 1m is strictly finer than the
        // 30m/4h reconstructions, so the override stays valid.
        jail.set_env("QE_BARS__BASE", "1m");
        let cfg = Config::load(Path::new("config.toml")).expect("load with env override");
        assert_eq!(
            cfg.bars.base, "1m",
            "env QE_BARS__BASE should win over the file"
        );
        Ok(())
    });
}

#[test]
fn env_override_keeps_hash_deterministic() {
    // Two loads with identical file+env must hash identically.
    let mut hashes = Vec::new();
    for _ in 0..2 {
        Jail::expect_with(|jail| {
            jail.create_file("config.toml", BASE)?;
            jail.set_env("QE_PROFILE", "runtime-sim");
            let cfg = Config::load(Path::new("config.toml")).expect("load");
            assert_eq!(cfg.profile, Profile::RuntimeSim);
            hashes.push(cfg.content_hash().expect("hash"));
            Ok(())
        });
    }
    assert_eq!(hashes[0], hashes[1]);
}
