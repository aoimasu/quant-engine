//! QE-108 acceptance: assemble per-bar feature vectors, cache the complete ones into the synthetic
//! store, and prove the round-trip + lineage staleness + reproducibility.

use rust_decimal::Decimal;

use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, Timestamp};
use qe_ingest::{assemble_and_cache_features, read_cached_feature};
use qe_signal::{assemble_batch, CatalogueConfig, FeatureSchema};
use qe_storage::SyntheticStore;

const MIN: i64 = 60_000;
const MAP_SIZE: usize = 20 * 1024 * 1024;

fn inst() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

fn samples(n: usize) -> Vec<qe_signal::Sample> {
    (0..n)
        .map(|i| {
            let i = i as i64;
            let base = 100 + (i % 7) * 3 + i / 5;
            let bar = Bar::new(
                Timestamp::from_millis(i * 5 * MIN),
                Resolution::M5,
                Price::new(Decimal::from(base)).unwrap(),
                Price::new(Decimal::from(base + 6)).unwrap(),
                Price::new(Decimal::from(base - 6)).unwrap(),
                Price::new(Decimal::from(base + (i % 3) - 1)).unwrap(),
                Qty::new(Decimal::from(10 + (i % 5))).unwrap(),
                1,
            )
            .unwrap();
            qe_signal::Sample {
                bar,
                funding: Some(Decimal::new((i % 5) - 2, 4)),
                open_interest: Some(Decimal::from(1000 + i * 7)),
                premium: Some(Decimal::new((i % 3) - 1, 4)),
            }
        })
        .collect()
}

#[test]
fn assemble_cache_and_read_back_complete_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let store = SyntheticStore::open(dir.path(), MAP_SIZE).unwrap();
    let cfg = CatalogueConfig::default();
    let s = samples(80);

    let cached =
        assemble_and_cache_features(&store, &inst(), Resolution::M5, "vintage-1", &cfg, &s)
            .unwrap();

    // Count of cached == number of complete (fully-warmed) vectors.
    let expected_complete = assemble_batch(&cfg, &s)
        .iter()
        .filter(|v| v.is_complete())
        .count();
    assert_eq!(cached, expected_complete);
    assert!(cached > 0);

    // The last bar's vector round-trips byte-for-byte through the cache.
    let want = assemble_batch(&cfg, &s).pop().unwrap();
    let got = read_cached_feature(
        &store,
        &inst(),
        Resolution::M5,
        Timestamp::from_millis(want.time_ms),
        "vintage-1",
        &cfg,
    )
    .unwrap();
    assert_eq!(got.as_ref(), Some(&want));
}

#[test]
fn cached_feature_is_stale_under_a_different_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = SyntheticStore::open(dir.path(), MAP_SIZE).unwrap();
    let cfg = CatalogueConfig::default();
    let s = samples(80);

    assemble_and_cache_features(&store, &inst(), Resolution::M5, "vintage-1", &cfg, &s).unwrap();
    let last_time = assemble_batch(&cfg, &s).pop().unwrap().time_ms;

    // Same lineage serves it; a different lineage sees it as stale (None).
    let t = Timestamp::from_millis(last_time);
    assert!(
        read_cached_feature(&store, &inst(), Resolution::M5, t, "vintage-1", &cfg)
            .unwrap()
            .is_some()
    );
    assert!(
        read_cached_feature(&store, &inst(), Resolution::M5, t, "vintage-2", &cfg)
            .unwrap()
            .is_none()
    );
}

#[test]
fn caching_is_reproducible_across_runs() {
    let cfg = CatalogueConfig::default();
    let s = samples(80);

    let count_for = |path: &std::path::Path| {
        let store = SyntheticStore::open(path, MAP_SIZE).unwrap();
        assemble_and_cache_features(&store, &inst(), Resolution::M5, "v", &cfg, &s).unwrap()
    };

    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    assert_eq!(count_for(d1.path()), count_for(d2.path()));

    // And the schema width is stable (the decode contract).
    assert!(FeatureSchema::from_catalogue(&cfg).len() >= 20);
}
