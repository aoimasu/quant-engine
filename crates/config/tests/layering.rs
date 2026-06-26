//! Layering integration tests: base TOML file → profile overlay → `QE_`-prefixed env vars.
//!
//! Uses `figment::Jail` so the temp files + env mutations are scoped to the test (safe under the
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
        let cfg = Config::load(Profile::Train, Path::new("config.toml")).expect("load");
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
        let cfg =
            Config::load(Profile::Train, Path::new("config.toml")).expect("load with env override");
        assert_eq!(
            cfg.bars.base, "1m",
            "env QE_BARS__BASE should win over the file"
        );
        Ok(())
    });
}

#[test]
fn profile_overlay_overrides_base_and_profile_is_forced() {
    Jail::expect_with(|jail| {
        jail.create_file("config.toml", BASE)?;
        // Overlay applies only when loading the runtime-sim profile.
        jail.create_file("config.runtime-sim.toml", "[determinism]\nseed = 99\n")?;

        let train = Config::load(Profile::Train, Path::new("config.toml")).expect("train load");
        assert_eq!(train.profile, Profile::Train);
        assert_eq!(train.determinism.seed, 7, "no train overlay → base seed");

        let sim = Config::load(Profile::RuntimeSim, Path::new("config.toml")).expect("sim load");
        assert_eq!(
            sim.profile,
            Profile::RuntimeSim,
            "requested profile is authoritative over the file's `profile = train`"
        );
        assert_eq!(
            sim.determinism.seed, 99,
            "runtime-sim overlay wins over base"
        );
        Ok(())
    });
}

#[test]
fn load_is_hash_deterministic() {
    // Two loads with identical file+overlay+env must hash identically.
    let mut hashes = Vec::new();
    for _ in 0..2 {
        Jail::expect_with(|jail| {
            jail.create_file("config.toml", BASE)?;
            jail.set_env("QE_BARS__BASE", "1m");
            let cfg = Config::load(Profile::RuntimeSim, Path::new("config.toml")).expect("load");
            assert_eq!(cfg.profile, Profile::RuntimeSim);
            hashes.push(cfg.content_hash().expect("hash"));
            Ok(())
        });
    }
    assert_eq!(hashes[0], hashes[1]);
}
