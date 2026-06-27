//! AC #1 (round-trip + range-scan per record kind) and AC #2 (schema version recorded + mismatch
//! detected), plus concurrent-read coverage.

use std::sync::Arc;

use heed::types::Str;
use heed::{Database, EnvOpenOptions};
use rust_decimal::Decimal;

use qe_domain::{
    Bar, FundingRate, FundingRateSample, InstrumentId, Price, Qty, Resolution, Timestamp,
};
use qe_storage::{FuturesMetrics, MarketStore, PremiumSample, StorageError, SCHEMA_VERSION};

const MAP_SIZE: usize = 10 * 1024 * 1024;

fn open(dir: &std::path::Path) -> MarketStore {
    MarketStore::open(dir, MAP_SIZE).expect("open store")
}

fn inst(s: &str) -> InstrumentId {
    InstrumentId::new(s).unwrap()
}
fn price(n: i64) -> Price {
    Price::new(Decimal::from(n)).unwrap()
}
fn at(secs: i64) -> Timestamp {
    Timestamp::from_secs(secs)
}

fn bar(res: Resolution, secs: i64, base: i64) -> Bar {
    Bar::new(
        at(secs),
        res,
        price(base),
        price(base + 10),
        price(base - 10),
        price(base),
        Qty::new(Decimal::from(1)).unwrap(),
        1,
    )
    .unwrap()
}

#[test]
fn bars_round_trip_and_range_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let res = Resolution::M5;
    let bars = vec![bar(res, 100, 100), bar(res, 200, 110), bar(res, 300, 120)];
    store.put_bars(&id, &bars).unwrap();

    // Exact round-trip.
    assert_eq!(
        store.get_bar(&id, res, at(200)).unwrap(),
        Some(bars[1].clone())
    );
    assert_eq!(store.get_bar(&id, res, at(250)).unwrap(), None);

    // Range scan [150, 300) → only the 200 bar (300 is exclusive).
    assert_eq!(
        store.scan_bars(&id, res, at(150), at(300)).unwrap(),
        vec![bars[1].clone()]
    );
    // Full range, in chronological order.
    assert_eq!(store.scan_bars(&id, res, at(0), at(1_000)).unwrap(), bars);
    // from inclusive.
    assert_eq!(
        store.scan_bars(&id, res, at(100), at(201)).unwrap(),
        vec![bars[0].clone(), bars[1].clone()]
    );
    // Empty range.
    assert!(store
        .scan_bars(&id, res, at(400), at(500))
        .unwrap()
        .is_empty());
}

#[test]
fn bars_scan_isolates_instrument_and_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let btc = inst("BTCUSDT");
    let eth = inst("ETHUSDT");
    store
        .put_bars(&btc, &[bar(Resolution::M5, 100, 100)])
        .unwrap();
    store
        .put_bars(&eth, &[bar(Resolution::M5, 100, 200)])
        .unwrap();
    store
        .put_bars(&btc, &[bar(Resolution::H1, 100, 300)])
        .unwrap();

    let btc_m5 = store
        .scan_bars(&btc, Resolution::M5, at(0), at(1_000))
        .unwrap();
    assert_eq!(btc_m5.len(), 1);
    assert_eq!(btc_m5[0].open(), price(100)); // not ETH's 200, not BTC's H1 bar
}

#[test]
fn bars_scan_isolates_prefix_substring_instruments() {
    // The footgun: one instrument's name is a strict prefix of another's. The 0x00 delimiter must
    // keep "BTC" rows out of a "BTCUSDT" scan and vice-versa (and across funding too).
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let short = inst("BTC");
    let long = inst("BTCUSDT");
    store
        .put_bars(
            &short,
            &[bar(Resolution::M5, 100, 100), bar(Resolution::M5, 200, 101)],
        )
        .unwrap();
    store
        .put_bars(
            &long,
            &[
                bar(Resolution::M5, 100, 200),
                bar(Resolution::M5, 200, 201),
                bar(Resolution::M5, 300, 202),
            ],
        )
        .unwrap();

    let s = store
        .scan_bars(&short, Resolution::M5, at(0), at(10_000))
        .unwrap();
    let l = store
        .scan_bars(&long, Resolution::M5, at(0), at(10_000))
        .unwrap();
    assert_eq!(s.len(), 2, "BTC scan must not bleed into BTCUSDT rows");
    assert_eq!(l.len(), 3, "BTCUSDT scan must not bleed into BTC rows");
    assert!(s
        .iter()
        .all(|b| b.open() == price(100) || b.open() == price(101)));
    assert!(l
        .iter()
        .all(|b| b.open().get() >= rust_decimal::Decimal::from(200)));
}

#[test]
fn funding_round_trip_and_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let mk = |secs: i64, r: &str| FundingRateSample {
        instrument: id.clone(),
        time: at(secs),
        rate: FundingRate::new(Decimal::from_str_exact(r).unwrap()),
    };
    let samples = vec![mk(100, "0.0001"), mk(200, "-0.0002"), mk(300, "0.0003")];
    store.put_funding(&samples).unwrap();

    assert_eq!(
        store.get_funding(&id, at(200)).unwrap(),
        Some(samples[1].clone())
    );
    assert_eq!(
        store.scan_funding(&id, at(150), at(350)).unwrap(),
        vec![samples[1].clone(), samples[2].clone()]
    );
}

#[test]
fn premium_round_trip_and_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let mk = |secs: i64, p: &str| PremiumSample {
        instrument: id.clone(),
        time: at(secs),
        premium: Decimal::from_str_exact(p).unwrap(),
    };
    let samples = vec![mk(100, "0.001"), mk(200, "-0.002")];
    store.put_premium(&samples).unwrap();
    assert_eq!(
        store.get_premium(&id, at(100)).unwrap(),
        Some(samples[0].clone())
    );
    assert_eq!(store.scan_premium(&id, at(0), at(1_000)).unwrap(), samples);
}

#[test]
fn futures_metrics_round_trip_and_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    let id = inst("BTCUSDT");
    let mk = |secs: i64| FuturesMetrics {
        instrument: id.clone(),
        time: at(secs),
        long_short_ratio: Decimal::from_str_exact("1.25").unwrap(),
        open_interest: Decimal::from(1_000),
        taker_buy_sell_ratio: Decimal::from_str_exact("0.98").unwrap(),
    };
    let samples = vec![mk(100), mk(200)];
    store.put_futures(&samples).unwrap();
    assert_eq!(
        store.get_futures(&id, at(200)).unwrap(),
        Some(samples[1].clone())
    );
    assert_eq!(store.scan_futures(&id, at(0), at(1_000)).unwrap(), samples);
}

#[test]
fn schema_version_is_recorded_and_reopen_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(dir.path());
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }
    // Reopening the same directory with the matching version succeeds.
    let store = open(dir.path());
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn schema_version_mismatch_is_detected_on_open() {
    let dir = tempfile::tempdir().unwrap();
    // Seed a store directory carrying a *different* schema version, then close it.
    {
        // SAFETY: same single-owner invariant as MarketStore::open — one exclusive env, dropped
        // before MarketStore re-opens the path. Used only to fabricate a version mismatch.
        #[allow(unsafe_code)]
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(MAP_SIZE)
                .max_dbs(8)
                .open(dir.path())
                .unwrap()
        };
        let mut wtxn = env.write_txn().unwrap();
        let meta: Database<Str, Str> = env.create_database(&mut wtxn, Some("meta")).unwrap();
        meta.put(&mut wtxn, "schema_version", "999").unwrap();
        wtxn.commit().unwrap();
        drop(env);
    }
    match MarketStore::open(dir.path(), MAP_SIZE) {
        Err(StorageError::SchemaMismatch { expected, found }) => {
            assert_eq!(expected, SCHEMA_VERSION);
            assert_eq!(found, 999);
        }
        Err(e) => panic!("expected SchemaMismatch, got error {e:?}"),
        Ok(_) => panic!("expected SchemaMismatch, but open succeeded"),
    }
}

#[test]
fn corrupt_schema_version_record_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    {
        // SAFETY: same single-owner invariant as MarketStore::open — one exclusive env, dropped
        // before re-opening. Seeds an unparseable version to exercise the SchemaCorrupt path.
        #[allow(unsafe_code)]
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(MAP_SIZE)
                .max_dbs(8)
                .open(dir.path())
                .unwrap()
        };
        let mut wtxn = env.write_txn().unwrap();
        let meta: Database<Str, Str> = env.create_database(&mut wtxn, Some("meta")).unwrap();
        meta.put(&mut wtxn, "schema_version", "not-a-number")
            .unwrap();
        wtxn.commit().unwrap();
        drop(env);
    }
    match MarketStore::open(dir.path(), MAP_SIZE) {
        Err(StorageError::SchemaCorrupt(s)) => assert_eq!(s, "not-a-number"),
        Err(e) => panic!("expected SchemaCorrupt, got {e:?}"),
        Ok(_) => panic!("expected SchemaCorrupt, but open succeeded"),
    }
}

#[test]
fn reads_are_concurrent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open(dir.path()));
    let id = inst("BTCUSDT");
    let bars: Vec<Bar> = (0..50).map(|i| bar(Resolution::M5, 100 + i, 100)).collect();
    store.put_bars(&id, &bars).unwrap();

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let store = Arc::clone(&store);
            let id = id.clone();
            std::thread::spawn(move || {
                let got = store
                    .scan_bars(&id, Resolution::M5, at(0), at(10_000))
                    .unwrap();
                got.len()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 50);
    }
}

// ---- QE-105 vintage lineage ledger ----------------------------------------------------------

#[test]
fn lineage_ledger_records_idempotently_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());

    // Unknown until recorded.
    assert!(!store.has_lineage("vintage-a").unwrap());

    // First record is new (true); a repeat is a no-op (false) — idempotency keyed by lineage.
    assert!(store.record_lineage("vintage-a").unwrap());
    assert!(!store.record_lineage("vintage-a").unwrap());
    assert!(store.has_lineage("vintage-a").unwrap());

    // A second distinct vintage is independent.
    assert!(store.record_lineage("vintage-b").unwrap());

    let mut ids = store.lineages().unwrap();
    ids.sort();
    assert_eq!(ids, vec!["vintage-a".to_owned(), "vintage-b".to_owned()]);
}

#[test]
fn lineage_ledger_is_independent_of_schema_version_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path());
    // Recording lineage must not disturb the schema-version record (shared `meta` db).
    store.record_lineage("v1").unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.lineages().unwrap(), vec!["v1".to_owned()]);
}
