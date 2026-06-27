//! QE-106 cache bridge: reconstruct base (5m) bars into coarser tiers, cache them into the
//! synthetic store tagged with source lineage, and prove the round-trip + lineage staleness.

use rust_decimal::Decimal;

use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, Timestamp};
use qe_ingest::cache_reconstructed_tiers;
use qe_signal::reconstruct_tiers;
use qe_storage::SyntheticStore;

const MIN: i64 = 60_000;
const MAP_SIZE: usize = 10 * 1024 * 1024;

fn inst() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

fn base_bar(open_min: i64, close: i64) -> Bar {
    let c = Price::new(Decimal::from(close)).unwrap();
    Bar::new(
        Timestamp::from_millis(open_min * MIN),
        Resolution::M5,
        c,
        Price::new(Decimal::from(close + 5)).unwrap(),
        Price::new(Decimal::from(close - 5)).unwrap(),
        c,
        Qty::new(Decimal::ONE).unwrap(),
        1,
    )
    .unwrap()
}

/// 48 contiguous 5m bars = one 4h window = eight 30m windows.
fn base_series() -> Vec<Bar> {
    (0..48).map(|i| base_bar(i * 5, 100 + i)).collect()
}

#[test]
fn reconstruct_caches_tiers_and_round_trips_under_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = SyntheticStore::open(dir.path(), MAP_SIZE).unwrap();
    let base = base_series();
    let tiers = [Resolution::M30, Resolution::H4];

    let cached =
        cache_reconstructed_tiers(&store, &inst(), "vintage-1", &base, Resolution::M5, &tiers)
            .unwrap();
    assert_eq!(cached, 9); // 8 × 30m + 1 × 4h

    // The cached tiers equal a fresh reconstruction (the bridge persisted exactly what it computed).
    let expected = reconstruct_tiers(&base, Resolution::M5, &tiers).unwrap();
    let expected_30m: Vec<Bar> = expected
        .iter()
        .filter(|b| b.resolution() == Resolution::M30)
        .cloned()
        .collect();

    let scanned_30m = store
        .scan_recon_bars(
            &inst(),
            Resolution::M30,
            Timestamp::from_millis(0),
            Timestamp::from_millis(i64::MAX),
        )
        .unwrap();
    assert_eq!(scanned_30m, expected_30m);

    // The single 4h bar round-trips by exact key.
    let h4 = store
        .get_recon_bar(
            &inst(),
            Resolution::H4,
            Timestamp::from_millis(0),
            "vintage-1",
        )
        .unwrap();
    assert!(h4.is_some());
    assert_eq!(h4.unwrap().resolution(), Resolution::H4);
}

#[test]
fn cached_tiers_are_stale_under_a_different_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = SyntheticStore::open(dir.path(), MAP_SIZE).unwrap();
    let base = base_series();

    cache_reconstructed_tiers(
        &store,
        &inst(),
        "vintage-1",
        &base,
        Resolution::M5,
        &[Resolution::M30],
    )
    .unwrap();

    // A keyed read under the SAME lineage serves the tier bar...
    assert!(store
        .get_recon_bar(
            &inst(),
            Resolution::M30,
            Timestamp::from_millis(0),
            "vintage-1",
        )
        .unwrap()
        .is_some());

    // ...but under a DIFFERENT lineage it is stale → not served.
    let stale = store
        .get_recon_bar(
            &inst(),
            Resolution::M30,
            Timestamp::from_millis(0),
            "vintage-2",
        )
        .unwrap();
    assert!(stale.is_none());
}
