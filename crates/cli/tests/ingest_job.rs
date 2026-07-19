//! Integration tests for the `qe-cli ingest` job + coverage query (QE-253).
//!
//! `coverage_over_sample_store_reports_expected_rows` runs [`coverage`] against the committed QE-251
//! sample store (`tests/fixtures/sample_store/`, BTCUSDT / 1h / 120 bars) and asserts the read-only
//! coverage rows. `ingest_populates_store_from_in_memory_source` drives [`run_ingest`] with an
//! in-memory [`HistoricalSource`] and confirms the bars land in a fresh store.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::{Path, PathBuf};

use qe_cli::jobs::ingest::{coverage, run_ingest, CoverageRow, IngestParams, SyntheticSource};
use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, Timestamp};
use qe_runtime::{BootstrapError, HistoricalSource, HistoricalWindow};
use qe_storage::{Calibration, MarketStore, Provenance};
use rust_decimal::Decimal;

/// Small LMDB map size — matches the fixture's writer (`backtest_job.rs`); ample for a handful of bars.
const FIXTURE_MAP_SIZE: usize = 1 << 20; // 1 MiB

/// 2021-01-01T00:00:00Z in epoch-ms (18628 days since the epoch) — the fixture's first bar.
const START_MS: i64 = 18_628 * 86_400_000;
const HOUR_MS: i64 = 3_600_000;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn btcusdt() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

/// Copy the committed store into a scratch dir so opening it (a write txn for schema init) never
/// mutates the fixture. Mirrors `backtest_job.rs::copy_store_to`.
fn copy_store_to(tmp: &Path) -> PathBuf {
    let src = fixtures_dir().join("sample_store");
    let dst = tmp.join("sample_store");
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    dst
}

#[test]
fn coverage_over_sample_store_reports_expected_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let store = MarketStore::open(&store_path, FIXTURE_MAP_SIZE).unwrap();

    let rows = coverage(&store, &[btcusdt()]).unwrap();

    assert_eq!(
        rows,
        vec![CoverageRow {
            symbol: "BTCUSDT".to_owned(),
            resolution: "1h".to_owned(),
            from: START_MS,
            to: START_MS + 119 * HOUR_MS,
            bars: 120,
            // The committed fixture predates provenance tagging ⇒ legacy `unknown` (documented default).
            provenance: "unknown".to_owned(),
            calibrated: false,
        }],
        "coverage over the committed sample store diverged"
    );
}

#[test]
fn coverage_is_empty_for_unknown_instrument() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let store = MarketStore::open(&store_path, FIXTURE_MAP_SIZE).unwrap();

    let rows = coverage(&store, &[InstrumentId::new("ETHUSDT").unwrap()]).unwrap();
    assert!(rows.is_empty(), "unknown instrument must yield no rows");
}

/// A one-shot in-memory source: hands back a single pre-built window, then errors on re-fetch.
struct InMemorySource {
    window: Option<HistoricalWindow>,
}

impl HistoricalSource for InMemorySource {
    fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError> {
        self.window
            .take()
            .ok_or_else(|| BootstrapError::Decode("source exhausted".to_owned()))
    }
}

fn p(v: i64) -> Price {
    Price::new(Decimal::from(v)).unwrap()
}

/// Five deterministic 1h bars starting at `START_MS`.
fn sample_bars(n: i64) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let base = 100 + i;
            Bar::new(
                Timestamp::from_millis(START_MS + i * HOUR_MS),
                Resolution::H1,
                p(base),
                p(base + 2),
                p(base - 1),
                p(base + 1),
                Qty::new(Decimal::from(10)).unwrap(),
                7,
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn ingest_populates_store_from_in_memory_source() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");

    let window = HistoricalWindow {
        base: Resolution::H1,
        bars: sample_bars(5),
        funding: vec![
            (START_MS, Decimal::new(1, 4)),
            (START_MS + HOUR_MS, Decimal::new(-1, 4)),
        ],
        open_interest: Vec::new(),
        premium: vec![(START_MS, Decimal::new(2, 4))],
        mark_price: Vec::new(),
    };
    let mut source = InMemorySource {
        window: Some(window),
    };

    let params = IngestParams {
        store_path: store_path.clone(),
        map_size: FIXTURE_MAP_SIZE,
        instrument: "BTCUSDT".to_owned(),
    };

    run_ingest(
        &params,
        &mut source,
        Provenance::Real,
        Calibration::Uncalibrated,
        &mut |_, _, _| {},
    )
    .unwrap();

    // Reopen and confirm via the coverage query.
    let store = MarketStore::open(&store_path, FIXTURE_MAP_SIZE).unwrap();
    let rows = coverage(&store, &[btcusdt()]).unwrap();
    assert_eq!(
        rows,
        vec![CoverageRow {
            symbol: "BTCUSDT".to_owned(),
            resolution: "1h".to_owned(),
            from: START_MS,
            to: START_MS + 4 * HOUR_MS,
            bars: 5,
            // A real ingest tags `real`; the klines-only slice is uncalibrated (QE-463 handoff).
            provenance: "real".to_owned(),
            calibrated: false,
        }]
    );

    // Funding + premium landed too (the backtest job scans these).
    let funding = store
        .scan_funding(
            &btcusdt(),
            Timestamp::from_millis(START_MS),
            Timestamp::from_millis(i64::MAX),
        )
        .unwrap();
    assert_eq!(funding.len(), 2);
    let premium = store
        .scan_premium(
            &btcusdt(),
            Timestamp::from_millis(START_MS),
            Timestamp::from_millis(i64::MAX),
        )
        .unwrap();
    assert_eq!(premium.len(), 1);
}

// ---------------------------------------------------------------------------------------------------
// Offline synthetic generator (`qe ingest --synthetic`) — SyntheticSource drives the same `run_ingest`.
// ---------------------------------------------------------------------------------------------------

/// A deterministic seed for the synthetic tests (stands in for `config.determinism.seed`).
const SYNTH_SEED: u64 = 42;

/// Generate the bars for one instrument index over `n` 1h bars from `START_MS`, without any store.
fn synthetic_bars(index: u64, n: i64) -> Vec<Bar> {
    let end = START_MS + n * HOUR_MS; // exclusive ⇒ exactly `n` bars
    let mut source = SyntheticSource::new(Resolution::H1, START_MS, end, SYNTH_SEED, index);
    assert_eq!(
        source.bar_count(),
        n as usize,
        "bar_count must match the window"
    );
    source.fetch().unwrap().bars
}

#[test]
fn synthetic_ingest_populates_fresh_store_with_expected_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("store");
    let n: i64 = 72;
    let end = START_MS + n * HOUR_MS;

    // Two instruments, exactly as `run_ingest_command` loops the config universe.
    for (idx, symbol) in ["BTCUSDT", "ETHUSDT"].iter().enumerate() {
        let mut source =
            SyntheticSource::new(Resolution::H1, START_MS, end, SYNTH_SEED, idx as u64);
        let params = IngestParams {
            store_path: store_path.clone(),
            map_size: FIXTURE_MAP_SIZE,
            instrument: (*symbol).to_owned(),
        };
        run_ingest(
            &params,
            &mut source,
            Provenance::Synthetic,
            Calibration::Uncalibrated,
            &mut |_, _, _| {},
        )
        .unwrap();
    }

    let store = MarketStore::open(&store_path, FIXTURE_MAP_SIZE).unwrap();
    let rows = coverage(
        &store,
        &[
            InstrumentId::new("BTCUSDT").unwrap(),
            InstrumentId::new("ETHUSDT").unwrap(),
        ],
    )
    .unwrap();

    // One coverage row per instrument, each spanning the whole window with exactly `n` bars, tagged
    // `synthetic` (the offline generator) — never silently real.
    assert_eq!(
        rows,
        vec![
            CoverageRow {
                symbol: "BTCUSDT".to_owned(),
                resolution: "1h".to_owned(),
                from: START_MS,
                to: START_MS + (n - 1) * HOUR_MS,
                bars: n as usize,
                provenance: "synthetic".to_owned(),
                calibrated: false,
            },
            CoverageRow {
                symbol: "ETHUSDT".to_owned(),
                resolution: "1h".to_owned(),
                from: START_MS,
                to: START_MS + (n - 1) * HOUR_MS,
                bars: n as usize,
                provenance: "synthetic".to_owned(),
                calibrated: false,
            },
        ]
    );
}

#[test]
fn synthetic_bars_are_valid_ohlc_and_chained() {
    let n: i64 = 240;
    let bars = synthetic_bars(0, n);
    assert_eq!(bars.len(), n as usize);

    for (i, b) in bars.iter().enumerate() {
        // OHLC invariant: high brackets the top, low brackets the bottom.
        assert!(
            b.high() >= b.open() && b.high() >= b.close(),
            "high below body at {i}"
        );
        assert!(
            b.low() <= b.open() && b.low() <= b.close(),
            "low above body at {i}"
        );
        assert!(b.high() >= b.low(), "high < low at {i}");
        // Strictly positive prices and volume — nothing degenerate reaches the store.
        assert!(b.low().get() > Decimal::ZERO, "non-positive low at {i}");
        assert!(
            b.volume().get() > Decimal::ZERO,
            "non-positive volume at {i}"
        );
        // Timestamps step by the resolution from START_MS.
        assert_eq!(
            b.open_time(),
            Timestamp::from_millis(START_MS + i as i64 * HOUR_MS)
        );
        // Each bar opens at the prior bar's close (a continuous walk).
        if i > 0 {
            assert_eq!(b.open(), bars[i - 1].close(), "open != prior close at {i}");
        }
    }
}

#[test]
fn synthetic_bars_reproduce_for_same_seed_and_decorrelate_across_instruments() {
    // Same (seed, index, window, resolution) ⇒ byte-identical bar VALUES.
    assert_eq!(
        synthetic_bars(0, 48),
        synthetic_bars(0, 48),
        "same seed must reproduce identical bars"
    );
    // Distinct instrument indices draw decorrelated streams ⇒ different bars.
    assert_ne!(
        synthetic_bars(0, 48),
        synthetic_bars(1, 48),
        "different instrument index must diverge"
    );
}

#[test]
fn ingest_rejects_invalid_instrument() {
    let tmp = tempfile::tempdir().unwrap();
    let window = HistoricalWindow {
        base: Resolution::H1,
        bars: sample_bars(1),
        funding: Vec::new(),
        open_interest: Vec::new(),
        premium: Vec::new(),
        mark_price: Vec::new(),
    };
    let mut source = InMemorySource {
        window: Some(window),
    };
    let params = IngestParams {
        store_path: tmp.path().join("store"),
        map_size: FIXTURE_MAP_SIZE,
        instrument: String::new(), // invalid
    };
    assert!(run_ingest(
        &params,
        &mut source,
        Provenance::Real,
        Calibration::Uncalibrated,
        &mut |_, _, _| {}
    )
    .is_err());
}
