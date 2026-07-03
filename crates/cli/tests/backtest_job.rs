//! Golden integration test for the `qe-cli backtest` job (QE-251).
//!
//! `backtest_over_fixture_store_matches_golden` runs [`run_backtest`] against the committed fixtures
//! (`tests/fixtures/sample_store/` + `sample_vintage.json`) and asserts the result is byte-identical to
//! `tests/fixtures/golden_result.json` — the determinism lock.
//!
//! The committed fixtures are produced by the `#[ignore]`d `regenerate_fixtures` test:
//! `cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`
//! (run once, eyeball the golden, commit). The sample store is reused by QE-253.

#![allow(clippy::unwrap_used)] // integration test: whole file is test-only code (QE-267)

use std::path::{Path, PathBuf};

use qe_cli::jobs::backtest::{run_backtest, BacktestParams};
use qe_domain::{
    Bar, FundingRate, FundingRateSample, InstrumentId, Price, Qty, Resolution, Timestamp,
};
use qe_signal::genome::{Clause, ExitParams, Genome, RiskParams, RuleSet, REP_VERSION};
use qe_storage::{MarketStore, PremiumSample};
use rust_decimal::Decimal;

/// Small LMDB map size: keeps the committed `data.mdb` to a few pages (a large map would be a sparse
/// file git would materialise). Ample for the handful of fixture bars.
const FIXTURE_MAP_SIZE: usize = 1 << 20; // 1 MiB

/// 2021-01-01T00:00:00Z in epoch-ms (18628 days since the epoch).
const START_MS: i64 = 18_628 * 86_400_000;
const HOUR_MS: i64 = 3_600_000;
const N_BARS: usize = 120;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn instrument() -> InstrumentId {
    InstrumentId::new("BTCUSDT").unwrap()
}

/// Deterministic OHLCV: a strong triangular swing around 100 so `sma_ratio_20` (feature 0) sweeps its
/// full quantised range — guaranteeing both long and short entries (and both wins and losses).
fn fixture_bars() -> Vec<Bar> {
    let mut bars = Vec::with_capacity(N_BARS);
    for i in 0..N_BARS {
        // Triangle wave, period 24 bars, amplitude 15 around a base of 100.
        let phase = (i % 24) as i64;
        let tri = if phase <= 12 { phase } else { 24 - phase }; // 0..12..0
        let mid = Decimal::from(100) + Decimal::from(tri) * Decimal::new(15, 1) - Decimal::from(9); // 100 + tri*1.5 - 9
        let close = mid;
        let open = mid;
        let high = mid + Decimal::new(5, 1); // +0.5
        let low = mid - Decimal::new(5, 1); // -0.5
        let t = Timestamp::from_millis(START_MS + i as i64 * HOUR_MS);
        bars.push(
            Bar::new(
                t,
                Resolution::H1,
                Price::new(open).unwrap(),
                Price::new(high).unwrap(),
                Price::new(low).unwrap(),
                Price::new(close).unwrap(),
                Qty::new(Decimal::from(1000)).unwrap(),
                100,
            )
            .unwrap(),
        );
    }
    bars
}

/// Funding stamps every 8h (a small constant rate) — exercises the funding scan + decision-bar funding.
fn fixture_funding() -> Vec<FundingRateSample> {
    let inst = instrument();
    (0..N_BARS)
        .step_by(8)
        .map(|i| FundingRateSample {
            instrument: inst.clone(),
            time: Timestamp::from_millis(START_MS + i as i64 * HOUR_MS),
            rate: FundingRate::new(Decimal::new(1, 4)), // 0.0001
        })
        .collect()
}

fn fixture_premium() -> Vec<PremiumSample> {
    let inst = instrument();
    (0..N_BARS)
        .step_by(8)
        .map(|i| PremiumSample {
            instrument: inst.clone(),
            time: Timestamp::from_millis(START_MS + i as i64 * HOUR_MS),
            premium: Decimal::new(2, 4), // 0.0002
        })
        .collect()
}

/// A deterministic single-chromosome genome addressing feature 0 (`sma_ratio_20`): go long when it is
/// in the top band `[3,4]` (price well above its 20-bar mean), short in the bottom band `[0,1]`; exit
/// after 3 bars or on the opposite signal.
fn fixture_genome() -> Genome {
    let long = RuleSet {
        clauses: [
            Clause {
                enabled: true,
                feature: 0,
                lo: 3,
                hi: 4,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
        ],
        min_satisfied: 1,
    };
    let short = RuleSet {
        clauses: [
            Clause {
                enabled: true,
                feature: 0,
                lo: 0,
                hi: 1,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
            Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            },
        ],
        min_satisfied: 1,
    };
    Genome {
        version: REP_VERSION,
        long_entry: long,
        short_entry: short,
        exit: ExitParams {
            max_holding_bars: 3,
            exit_on_opposite: true,
        },
        risk: RiskParams { size_bps: 5_000 },
    }
}

fn write_sample_store(dir: &Path) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
    let store = MarketStore::open(dir, FIXTURE_MAP_SIZE).unwrap();
    let inst = instrument();
    store.put_bars(&inst, &fixture_bars()).unwrap();
    store.put_funding(&fixture_funding()).unwrap();
    store.put_premium(&fixture_premium()).unwrap();
    // store drops here, flushing the write txns to disk.
}

fn write_sample_vintage(dir: &Path) {
    use qe_determinism::Lineage;
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_vintage::{Vintage, VintageContent, VintageRepository, VINTAGE_FORMAT_VERSION};

    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: "sample_vintage".to_owned(),
        chromosomes: vec![fixture_genome()],
        weights: vec![1.0],
        calibration: CalibrationProfile::new(Fraction::new(Decimal::new(1, 1)).unwrap()),
        worst_case_loss: Some(0.1),
        lineage: Lineage::new(
            "fixture-config-hash",
            "fixture-snapshot",
            "fixture-commit",
            vec![42],
        ),
    };
    let vintage = Vintage::seal(content).unwrap();
    VintageRepository::new(dir).write(&vintage).unwrap();
}

fn fixture_params(store_path: PathBuf) -> BacktestParams {
    BacktestParams {
        store_path,
        map_size: FIXTURE_MAP_SIZE,
        vintage_root: fixtures_dir(),
        vintage_id: "sample_vintage".to_owned(),
        strategy: None,
        start: "2021-01-01".to_owned(),
        end: "2021-01-10".to_owned(), // 9 days > 120 hours
        resolution: "1h".to_owned(),
        universe: vec!["BTCUSDT".to_owned()],
        taker_fee_bps: 2.0,
        slippage_model: "square-root-impact".to_owned(),
    }
}

/// Copy the committed store into a scratch dir so opening it (which takes a write txn for schema init)
/// never mutates the fixture.
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
fn backtest_over_fixture_store_matches_golden() {
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let params = fixture_params(store_path);

    let doc = run_backtest(&params, &mut |_, _, _| {}).unwrap();
    let got = serde_json::to_value(&doc).unwrap();

    let golden = fixtures_dir().join("golden_result.json");
    let want: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden).unwrap()).unwrap();

    assert_eq!(
        got, want,
        "result diverged from the golden file (determinism)"
    );
}

/// Regenerate the committed fixtures + golden file. Ignored by default; run explicitly, eyeball, commit:
/// `cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`.
#[test]
#[ignore = "regenerates committed fixtures; run manually"]
fn regenerate_fixtures() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).unwrap();
    write_sample_store(&dir.join("sample_store"));
    write_sample_vintage(&dir);

    // Compute the golden exactly the way the real test will (from a copy of the committed store).
    let tmp = tempfile::tempdir().unwrap();
    let store_path = copy_store_to(tmp.path());
    let params = fixture_params(store_path);
    let doc = run_backtest(&params, &mut |pct, stage, msg| {
        eprintln!("[{pct:>3}%] {stage}: {msg}");
    })
    .unwrap();

    // Sanity: the swing must actually trade, with both wins and losses (so profit_factor is finite).
    eprintln!(
        "trades={} win_rate={} profit_factor={} cagr={} sharpe={}",
        doc.trades.len(),
        doc.metrics.win_rate,
        doc.metrics.profit_factor,
        doc.metrics.cagr,
        doc.metrics.sharpe
    );
    assert!(!doc.trades.is_empty(), "fixture produced no trades");
    assert!(
        doc.metrics.profit_factor.is_finite(),
        "fixture has no losing trades (profit_factor is INFINITY)"
    );

    let pretty = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(dir.join("golden_result.json"), pretty + "\n").unwrap();
    eprintln!("wrote fixtures to {}", dir.display());
}
