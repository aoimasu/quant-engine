//! QE-105 acceptance: a full fuse→persist run is **reproducible** and **range-queryable** over the
//! public API, with **idempotent** persistence keyed by lineage.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use rust_decimal::Decimal;

use qe_domain::{
    Bar, FundingRate, FundingRateSample, InstrumentId, Price, Qty, Resolution, Timestamp,
};
use qe_ingest::{fused_bars, persist_fused, Adjustment, FusedMarket, PersistStatus};
use qe_storage::{FuturesMetrics, MarketStore, PremiumSample};

const MIN: i64 = 60_000;
const MAP_SIZE: usize = 10 * 1024 * 1024;

fn inst() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

fn bar_at(t_ms: i64, close: i64) -> Bar {
    let c = Price::new(Decimal::from(close)).unwrap();
    Bar::new(
        Timestamp::from_millis(t_ms),
        Resolution::M5,
        c,
        c,
        c,
        c,
        Qty::new(Decimal::ONE).unwrap(),
        1,
    )
    .unwrap()
}

/// Build the typed fused market from raw daily partitions + typed scalar samples (the fuse step).
fn build_market() -> FusedMarket {
    // Two daily partitions of perp bars (coalesced + identity-adjusted by `fused_bars`).
    let perp_partitions = vec![
        vec![bar_at(0, 100), bar_at(5 * MIN, 101)],
        vec![bar_at(10 * MIN, 102), bar_at(15 * MIN, 103)],
    ];
    let bars = fused_bars(&perp_partitions, Adjustment::IDENTITY).unwrap();
    FusedMarket {
        instrument: inst(),
        bars,
        funding: vec![
            FundingRateSample {
                instrument: inst(),
                time: Timestamp::from_millis(0),
                rate: FundingRate::new(Decimal::new(1, 4)),
            },
            FundingRateSample {
                instrument: inst(),
                time: Timestamp::from_millis(8 * 60 * MIN),
                rate: FundingRate::new(Decimal::new(-2, 4)),
            },
        ],
        premium: vec![PremiumSample {
            instrument: inst(),
            time: Timestamp::from_millis(0),
            premium: Decimal::new(5, 4),
        }],
        futures: vec![FuturesMetrics {
            instrument: inst(),
            time: Timestamp::from_millis(0),
            long_short_ratio: Decimal::new(15, 1),
            open_interest: Decimal::from(1000),
            taker_buy_sell_ratio: Decimal::new(11, 1),
        }],
    }
}

#[test]
fn full_fuse_persist_run_is_range_queryable() {
    let dir = tempfile::tempdir().unwrap();
    let store = MarketStore::open(dir.path(), MAP_SIZE).unwrap();
    let market = build_market();

    let report = persist_fused(&store, "vintage-xyz", &market).unwrap();
    assert_eq!(report.status, PersistStatus::Persisted);
    assert_eq!(report.bars, 4);

    // Range-queryable: scan over [0, 20min) returns the four bars in chronological order.
    let bars = store
        .scan_bars(
            &inst(),
            Resolution::M5,
            Timestamp::from_millis(0),
            Timestamp::from_millis(20 * MIN),
        )
        .unwrap();
    assert_eq!(bars, market.bars);

    // A sub-range excludes out-of-window bars.
    let mid = store
        .scan_bars(
            &inst(),
            Resolution::M5,
            Timestamp::from_millis(5 * MIN),
            Timestamp::from_millis(11 * MIN),
        )
        .unwrap();
    assert_eq!(mid, vec![bar_at(5 * MIN, 101), bar_at(10 * MIN, 102)]);

    // The scalar series round-trip too.
    assert_eq!(
        store
            .scan_funding(
                &inst(),
                Timestamp::from_millis(0),
                Timestamp::from_millis(i64::MAX)
            )
            .unwrap(),
        market.funding
    );
    assert_eq!(
        store
            .scan_premium(
                &inst(),
                Timestamp::from_millis(0),
                Timestamp::from_millis(i64::MAX)
            )
            .unwrap(),
        market.premium
    );
    assert_eq!(
        store
            .scan_futures(
                &inst(),
                Timestamp::from_millis(0),
                Timestamp::from_millis(i64::MAX)
            )
            .unwrap(),
        market.futures
    );

    // The store records the vintage's lineage.
    assert_eq!(store.lineages().unwrap(), vec!["vintage-xyz".to_owned()]);
}

#[test]
fn same_inputs_persist_to_identical_stores() {
    // Reproducibility: the same fused market persisted into two fresh stores yields identical scans.
    let market = build_market();

    let read_all = |dir: &std::path::Path| {
        let store = MarketStore::open(dir, MAP_SIZE).unwrap();
        persist_fused(&store, "v", &market).unwrap();
        store
            .scan_bars(
                &inst(),
                Resolution::M5,
                Timestamp::from_millis(0),
                Timestamp::from_millis(i64::MAX),
            )
            .unwrap()
    };

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    assert_eq!(read_all(dir_a.path()), read_all(dir_b.path()));
}

#[test]
fn re_persisting_same_lineage_is_idempotent_noop() {
    let dir = tempfile::tempdir().unwrap();
    let store = MarketStore::open(dir.path(), MAP_SIZE).unwrap();
    let market = build_market();

    persist_fused(&store, "v", &market).unwrap();
    let before = store
        .scan_bars(
            &inst(),
            Resolution::M5,
            Timestamp::from_millis(0),
            Timestamp::from_millis(i64::MAX),
        )
        .unwrap();

    // Re-running the same lineage writes nothing and leaves the store unchanged.
    let again = persist_fused(&store, "v", &market).unwrap();
    assert_eq!(again.status, PersistStatus::AlreadyPersisted);

    let after = store
        .scan_bars(
            &inst(),
            Resolution::M5,
            Timestamp::from_millis(0),
            Timestamp::from_millis(i64::MAX),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(store.lineages().unwrap(), vec!["v".to_owned()]);
}
