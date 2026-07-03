//! QE-012 acceptance: a point-in-time universe is resolved from config, excludes instruments not
//! tradable at an as-of date, and resizes by config alone (no code change).
//!
//! `figment::Error` is a large enum fixed by the `Jail` API, so allow the lint for this test file.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)
#![allow(clippy::result_large_err)]

use figment::Jail;
use qe_config::{Config, Profile};
use qe_domain::{InstrumentId, Timestamp};
use std::path::Path;

/// ISO date → UTC-midnight `Timestamp` for the assertions (mirrors the crate's own parser).
fn date(s: &str) -> Timestamp {
    qe_config::universe::parse_iso_date(s).expect("valid date")
}
fn inst(s: &str) -> InstrumentId {
    InstrumentId::new(s).unwrap()
}

const STORAGE: &str = r#"
[storage]
market_dir = "data/lmdb/market"
synthetic_dir = "data/lmdb/synthetic"
artifacts_dir = "data/artifacts"
"#;

#[test]
fn point_in_time_membership_excludes_unlisted_and_delisted() {
    Jail::expect_with(|jail| {
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
{STORAGE}
"#
        );
        jail.create_file("config.toml", &toml)?;
        let cfg = Config::load(Profile::Train, Path::new("config.toml")).expect("load");
        let u = cfg.universe().expect("universe");

        // Before any listing → empty; after BTC only; both live; ETH delisted → BTC only.
        assert!(u.members_at(date("2019-01-01")).is_empty());
        assert_eq!(u.members_at(date("2019-10-01")), vec![inst("BTCUSDT")]);
        assert_eq!(
            u.members_at(date("2020-06-01")),
            vec![inst("BTCUSDT"), inst("ETHUSDT")]
        );
        assert_eq!(u.members_at(date("2025-06-01")), vec![inst("BTCUSDT")]);

        // Delisted ETH is retained in the full roster (no survivorship drop).
        assert_eq!(u.all_known().len(), 2);
        Ok(())
    });
}

#[test]
fn universe_size_is_config_only() {
    // Same code path, different config → different size. No per-instrument code.
    Jail::expect_with(|jail| {
        let three = format!(
            r#"
instruments = ["BTCUSDT"]
[[universe]]
instrument = "BTCUSDT"
listed = "2020-01-01"
[[universe]]
instrument = "ETHUSDT"
listed = "2020-01-01"
[[universe]]
instrument = "SOLUSDT"
listed = "2020-08-11"
{STORAGE}
"#
        );
        jail.create_file("config.toml", &three)?;
        let cfg = Config::load(Profile::Train, Path::new("config.toml")).expect("load");
        let u = cfg.universe().expect("universe");
        assert_eq!(u.members_at(date("2020-09-01")).len(), 3);
        assert_eq!(u.members_at(date("2020-02-01")).len(), 2); // SOL not yet listed
        Ok(())
    });
}

#[test]
fn flat_instruments_fallback_is_open_ended() {
    // No [[universe]] section → derive an always-member universe from the flat list.
    Jail::expect_with(|jail| {
        let toml = format!(
            r#"
instruments = ["BTCUSDT", "ETHUSDT"]
{STORAGE}
"#
        );
        jail.create_file("config.toml", &toml)?;
        let cfg = Config::load(Profile::Train, Path::new("config.toml")).expect("load");
        let u = cfg.universe().expect("universe");
        assert_eq!(u.len(), 2);
        // Always members regardless of as-of date.
        assert_eq!(u.members_at(date("1999-01-01")).len(), 2);
        assert_eq!(u.members_at(date("2099-01-01")).len(), 2);
        Ok(())
    });
}

#[test]
fn invalid_universe_entry_is_rejected_at_load() {
    Jail::expect_with(|jail| {
        let toml = format!(
            r#"
instruments = ["BTCUSDT"]
[[universe]]
instrument = "BTCUSDT"
listed = "2020-01-01"
delisted = "2019-01-01"
{STORAGE}
"#
        );
        jail.create_file("config.toml", &toml)?;
        // Load validates the universe (delisted < listed) and fails fast.
        let err = Config::load(Profile::Train, Path::new("config.toml")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("universe[0].delisted"), "got: {msg}");
        Ok(())
    });
}
