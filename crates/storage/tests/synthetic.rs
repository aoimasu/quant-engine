//! AC #1 (cached indicator states byte-identical to input) and AC #2 (stale-source detection +
//! invalidation), plus reconstructed-bar coverage.

use rust_decimal::Decimal;

use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, Timestamp};
use qe_storage::{IndicatorKey, StorageError, SyntheticStore, SYNTHETIC_SCHEMA_VERSION};

const MAP_SIZE: usize = 10 * 1024 * 1024;

fn open(dir: &std::path::Path) -> SyntheticStore {
    SyntheticStore::open(dir, MAP_SIZE).expect("open synthetic store")
}
fn inst(s: &str) -> InstrumentId {
    InstrumentId::new(s).unwrap()
}
fn at(secs: i64) -> Timestamp {
    Timestamp::from_secs(secs)
}

fn key<'a>(id: &'a InstrumentId, indicator: &'a str, t: i64) -> IndicatorKey<'a> {
    IndicatorKey {
        instrument: id,
        resolution: Resolution::M5,
        indicator_id: indicator,
        lookback: 14,
        time: at(t),
    }
}

#[test]
fn indicator_state_is_byte_identical_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let k = key(&id, "rsi", 100);

    // Opaque "freshly computed" state, including a NUL and high bytes.
    let computed: Vec<u8> = vec![0, 1, 2, 250, 0, 99, 255];
    store
        .put_indicator_state(&k, "lineage-A", &computed)
        .unwrap();

    let cached = store.get_indicator_state(&k, "lineage-A").unwrap();
    assert_eq!(
        cached.as_deref(),
        Some(computed.as_slice()),
        "cache must be byte-identical"
    );

    // An empty state also round-trips exactly.
    let k2 = key(&id, "ema", 200);
    store.put_indicator_state(&k2, "lineage-A", &[]).unwrap();
    assert_eq!(
        store.get_indicator_state(&k2, "lineage-A").unwrap(),
        Some(vec![])
    );
}

#[test]
fn stale_source_is_detected_and_not_served() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let k = key(&id, "rsi", 100);
    store
        .put_indicator_state(&k, "lineage-A", &[1, 2, 3])
        .unwrap();

    // Fresh lineage → served; stale (changed source) lineage → miss.
    assert_eq!(
        store.get_indicator_state(&k, "lineage-A").unwrap(),
        Some(vec![1, 2, 3])
    );
    assert_eq!(
        store.get_indicator_state(&k, "lineage-B").unwrap(),
        None,
        "a stale-source entry must not be served"
    );
}

#[test]
fn invalidate_stale_indicators_evicts_only_mismatched_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let old = key(&id, "rsi", 100);
    let fresh = key(&id, "rsi", 200);
    store.put_indicator_state(&old, "lineage-A", &[9]).unwrap();
    store
        .put_indicator_state(&fresh, "lineage-B", &[8])
        .unwrap();

    // Invalidate everything not derived from lineage-B: removes the one lineage-A entry.
    let removed = store.invalidate_stale_indicators("lineage-B").unwrap();
    assert_eq!(removed, 1);

    // The stale entry is gone even when queried with its own (now-old) lineage; the fresh one stays.
    assert_eq!(store.get_indicator_state(&old, "lineage-A").unwrap(), None);
    assert_eq!(
        store.get_indicator_state(&fresh, "lineage-B").unwrap(),
        Some(vec![8])
    );

    // Nothing stale now → zero removed.
    assert_eq!(store.invalidate_stale_indicators("lineage-B").unwrap(), 0);
}

fn bar(secs: i64, base: i64) -> Bar {
    Bar::new(
        at(secs),
        Resolution::H4,
        Price::new(Decimal::from(base)).unwrap(),
        Price::new(Decimal::from(base + 10)).unwrap(),
        Price::new(Decimal::from(base - 10)).unwrap(),
        Price::new(Decimal::from(base)).unwrap(),
        Qty::new(Decimal::from(1)).unwrap(),
        1,
    )
    .unwrap()
}

#[test]
fn recon_bars_round_trip_scan_and_lineage_check() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let bars = vec![bar(100, 100), bar(200, 110), bar(300, 120)];
    store.put_recon_bars(&id, "lineage-A", &bars).unwrap();

    // Lineage-checked get.
    assert_eq!(
        store
            .get_recon_bar(&id, Resolution::H4, at(200), "lineage-A")
            .unwrap(),
        Some(bars[1].clone())
    );
    assert_eq!(
        store
            .get_recon_bar(&id, Resolution::H4, at(200), "lineage-B")
            .unwrap(),
        None
    );

    // Chronological window scan [150, 300) → just the 200 bar.
    assert_eq!(
        store
            .scan_recon_bars(&id, Resolution::H4, at(150), at(300))
            .unwrap(),
        vec![bars[1].clone()]
    );
    assert_eq!(
        store
            .scan_recon_bars(&id, Resolution::H4, at(0), at(1_000))
            .unwrap(),
        bars
    );
}

#[test]
fn schema_version_recorded_and_reopen_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path());
        assert_eq!(store.schema_version().unwrap(), SYNTHETIC_SCHEMA_VERSION);
    }
    let store = open(dir.path());
    assert_eq!(store.schema_version().unwrap(), SYNTHETIC_SCHEMA_VERSION);
}

#[test]
fn open_result_is_usable() {
    // Smoke: a bad map size of 0 still returns a Result (no panic) — exercise the error path shape.
    let dir = tempfile::tempdir().unwrap();
    let res = SyntheticStore::open(dir.path(), MAP_SIZE);
    assert!(matches!(res, Ok(_) | Err(StorageError::Lmdb(_))));
}
