//! The ingest job: populate a [`MarketStore`] from the injectable [`HistoricalSource`] seam, plus the
//! read-only [`coverage`] query the admin server's Market-data view (QE-257) calls (QE-253 Task 6).
//!
//! Deterministic: same inputs ⇒ same store contents and the same coverage rows; no wall-clock/RNG in
//! any output. Real Binance decoders live behind the default-off `http` feature (out of scope here);
//! [`run_ingest`] is exercised in tests with an in-memory source, and backtests use the committed
//! sample-store fixture.

use std::path::PathBuf;

use qe_determinism::{task_rng, DetRng};
use qe_domain::{
    Bar, FundingRate, FundingRateSample, InstrumentId, Price, Qty, Resolution, Timestamp,
};
use qe_runtime::{BootstrapError, HistoricalSource, HistoricalWindow};
use qe_storage::{MarketStore, PremiumSample};
use rand_core::RngCore;
use rust_decimal::Decimal;

use super::RunError;

// The coverage query + its row type moved to `qe-storage` (QE-257) so the admin server — which cannot
// depend on `qe-cli`/`qe-runtime` — can call it too. Re-exported here so the QE-253 call sites
// (`qe_cli::jobs::ingest::{coverage, CoverageRow}`, including `tests/ingest_job.rs`) keep working.
pub use qe_storage::{coverage, CoverageRow};

/// Everything [`run_ingest`] needs. Built by `main` from a parsed `Command::Ingest` (+ the store path
/// from config), and directly in tests (pointing at a scratch store).
#[derive(Debug, Clone)]
pub struct IngestParams {
    /// Path to the LMDB `MarketStore` to populate.
    pub store_path: PathBuf,
    /// LMDB map size to open the store with.
    pub map_size: usize,
    /// The instrument the fetched window belongs to (the seam's window carries no id).
    pub instrument: String,
}

/// Run the ingest job: fetch one historical window from `source` and persist its bars, funding and
/// premium into the store for `params.instrument`, emitting coarse progress through
/// `progress(pct, stage, msg)`.
///
/// The window carries no instrument id (it is implicitly one instrument's window), so bars are written
/// under `params.instrument` at their own resolution. Open-interest / mark-price are not persisted (the
/// store has no matching single-value slot and the backtest job does not scan them).
///
/// # Errors
/// [`RunError::Instrument`] on an invalid symbol, [`RunError::Ingest`] on a source fetch failure, or
/// [`RunError::Storage`] on a write failure.
pub fn run_ingest(
    params: &IngestParams,
    source: &mut impl HistoricalSource,
    progress: &mut impl FnMut(u8, &str, &str),
) -> Result<(), RunError> {
    progress(10, "load", "opening store");
    let instrument =
        InstrumentId::new(&params.instrument).map_err(|source| RunError::Instrument {
            symbol: params.instrument.clone(),
            source,
        })?;
    let store = MarketStore::open(&params.store_path, params.map_size)?;

    progress(40, "fetch", "fetching historical window");
    let window = source
        .fetch()
        .map_err(|e| RunError::Ingest(e.to_string()))?;

    progress(80, "write", "writing bars, funding and premium");
    store.put_bars(&instrument, &window.bars)?;

    let funding: Vec<FundingRateSample> = window
        .funding
        .iter()
        .map(|&(ts, rate)| FundingRateSample {
            instrument: instrument.clone(),
            time: Timestamp::from_millis(ts),
            rate: FundingRate::new(rate),
        })
        .collect();
    store.put_funding(&funding)?;

    let premium: Vec<PremiumSample> = window
        .premium
        .iter()
        .map(|&(ts, premium)| PremiumSample {
            instrument: instrument.clone(),
            time: Timestamp::from_millis(ts),
            premium,
        })
        .collect();
    store.put_premium(&premium)?;

    progress(
        95,
        "report",
        &format!(
            "ingested {} bars, {} funding, {} premium",
            window.bars.len(),
            funding.len(),
            premium.len()
        ),
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------------
// Offline synthetic market-data generator.
//
// `qe ingest --synthetic` populates the LMDB store locally WITHOUT the unimplemented `http` decoders,
// so a developer can run backtests/train against a store of the shape a real ingest would produce. It
// stays in this CLI composition root (a `qe-cli → qe-runtime` `HistoricalSource` impl, mirroring the
// in-memory source in `tests/ingest_job.rs`) — no new firewall edge — and reuses `run_ingest` verbatim,
// so the store write path (and the coverage query) is exactly the tested one.
//
// The generated bars are GENERATED, NOT real market data. The `--synthetic` run is loudly labelled at
// the CLI (a stderr warning + a `"synthetic":true` marker in the terminal `done` line) so no store can
// be mistaken for real prices.
// ---------------------------------------------------------------------------------------------------

/// Number of bars whose open-time falls in the half-open window `[start_ms, end_ms)` when stepping by
/// `step_ms`. Zero for an empty/reversed window or a non-positive step.
#[must_use]
fn synthetic_bar_count(start_ms: i64, end_ms: i64, step_ms: i64) -> usize {
    if end_ms <= start_ms || step_ms <= 0 {
        return 0;
    }
    // ceil((end - start) / step): the last open-time strictly below `end`.
    let span = end_ms - start_ms;
    usize::try_from((span + step_ms - 1) / step_ms).unwrap_or(0)
}

/// Draw a deterministic uniform `Decimal` in `[0, 1]` (6 decimal places) from `rng`. Decimal (not
/// `f64`) so nothing that reaches the store is ever a float, matching the codebase's money discipline.
fn draw_unit(rng: &mut DetRng) -> Decimal {
    let r = rng.next_u64() % 1_000_001;
    Decimal::from(r) / Decimal::from(1_000_000_u64)
}

/// A deterministic **OFFLINE** synthetic [`HistoricalSource`] for one instrument.
///
/// Generates a window of valid OHLCV [`Bar`]s over `[start_ms, end_ms)` at `resolution` via a **seeded
/// geometric random walk**: `close` compounds a small drift + bounded per-bar shock, `open` is the prior
/// close (the first open is itself seeded), and each `high`/`low` brackets `max`/`min(open, close)` by a
/// non-negative wick — so [`Bar::new`]'s OHLC invariant holds by construction. Volume is strictly
/// positive; all values are [`Decimal`] (never `f64`).
///
/// Reproducibility: the RNG is [`task_rng`]`(master_seed, instrument_index)`, so a given
/// `(config seed, instrument, window, resolution)` always yields byte-identical bars, while distinct
/// instruments get decorrelated streams. The generated data is **NOT real market data**.
#[derive(Debug, Clone)]
pub struct SyntheticSource {
    resolution: Resolution,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
    master_seed: u64,
    instrument_index: u64,
}

impl SyntheticSource {
    /// Build a generator for the instrument at `instrument_index` in the run's universe, seeded from the
    /// run's `master_seed` (`config.determinism.seed`). The window is the half-open `[start_ms, end_ms)`
    /// at `resolution`.
    #[must_use]
    pub fn new(
        resolution: Resolution,
        start_ms: i64,
        end_ms: i64,
        master_seed: u64,
        instrument_index: u64,
    ) -> Self {
        let step_ms = i64::from(resolution.minutes()) * 60_000;
        Self {
            resolution,
            start_ms,
            end_ms,
            step_ms,
            master_seed,
            instrument_index,
        }
    }

    /// The number of bars this source will emit for its window (deterministic; no RNG).
    #[must_use]
    pub fn bar_count(&self) -> usize {
        synthetic_bar_count(self.start_ms, self.end_ms, self.step_ms)
    }
}

impl HistoricalSource for SyntheticSource {
    fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError> {
        let n = self.bar_count();
        let mut rng = task_rng(self.master_seed, self.instrument_index);

        // Walk parameters — small drift, realistic bounded per-bar vol, and a thin high/low wick.
        let drift = Decimal::new(1, 4); // +0.0001 per bar
        let vol = Decimal::new(1, 2); // ±0.01 per-bar return
        let hl_vol = Decimal::new(5, 3); // ≤0.5% wick beyond open/close
        let half = Decimal::new(5, 1); // 0.5
        let two = Decimal::from(2);
        let base_volume = Decimal::from(10);
        let volume_span = Decimal::from(1_000);

        // Seeded first open in [100, 1000], decorrelated per instrument via the task RNG.
        let mut open = (Decimal::from(100) + draw_unit(&mut rng) * Decimal::from(900)).round_dp(2);

        let mut bars = Vec::with_capacity(n);
        for i in 0..n {
            let open_time = Timestamp::from_millis(self.start_ms + (i as i64) * self.step_ms);

            // Geometric step: close = open * (1 + drift + shock), shock ∈ [-vol, vol]. Since
            // drift - vol > -1, the factor stays positive, so close stays strictly positive.
            let shock = (draw_unit(&mut rng) - half) * two * vol;
            let ret = drift + shock;
            let close = (open * (Decimal::ONE + ret)).round_dp(8);

            // Additive, non-negative wicks (computed AFTER rounding open/close) guarantee
            // high ≥ max(open, close) and 0 < low ≤ min(open, close) exactly — never broken by rounding.
            let hi_base = open.max(close);
            let lo_base = open.min(close);
            let up = (hi_base * hl_vol * draw_unit(&mut rng)).round_dp(8);
            let down = (lo_base * hl_vol * draw_unit(&mut rng)).round_dp(8);
            let high = hi_base + up;
            let low = lo_base - down;

            let volume = (base_volume + draw_unit(&mut rng) * volume_span).round_dp(4);
            let trades = rng.next_u64() % 1_000 + 1;

            let bar = Bar::new(
                open_time,
                self.resolution,
                Price::new(open).map_err(|e| BootstrapError::Decode(e.to_string()))?,
                Price::new(high).map_err(|e| BootstrapError::Decode(e.to_string()))?,
                Price::new(low).map_err(|e| BootstrapError::Decode(e.to_string()))?,
                Price::new(close).map_err(|e| BootstrapError::Decode(e.to_string()))?,
                Qty::new(volume).map_err(|e| BootstrapError::Decode(e.to_string()))?,
                trades,
            )
            .map_err(|e| BootstrapError::Decode(e.to_string()))?;
            bars.push(bar);

            open = close; // next bar opens at this bar's close
        }

        Ok(HistoricalWindow {
            base: self.resolution,
            bars,
            funding: Vec::new(),
            open_interest: Vec::new(),
            premium: Vec::new(),
            mark_price: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// Real Binance USDT-M `HistoricalSource` (QE-463) — behind the default-off `http` feature.
//
// The composition-root adapter between the `qe-ingest` decoder (`BinanceHistorical`: plan-missing →
// paginate → decode klines+funding → closed-window filter) and the runtime `HistoricalSource` seam
// `run_ingest` writes through. It maps the decoder's `IngestedWindow` onto `HistoricalWindow`, filling
// bars + funding and leaving premium/open-interest/mark-price EMPTY: this is the **klines-only**
// calibration-honest slice (`CalibrationSource::Uncalibrated`) — no premium/impact/ADV inputs are
// fabricated. (`run_ingest` discards open-interest/mark-price anyway.)
//
// All real-network code stays here behind `#[cfg(feature = "http")]`; the default build, the
// `--synthetic` path, and the in-memory test sources above are untouched. Wiring the CLI command /
// trigger to construct this from store coverage is QE-464.
// ---------------------------------------------------------------------------------------------------

/// A live Binance USDT-M `HistoricalSource`: fetches the missing, closed klines + funding for one
/// instrument/window against current store coverage, generic over the REST transport so it is offline
/// testable. `present_bars` / `present_funding` are the open-times the store already covers (supplied by
/// the QE-464 trigger from a `coverage_bounds` scan).
#[cfg(feature = "http")]
pub struct BinanceHistoricalSource<S, Sl = qe_ingest::RealSleeper>
where
    S: qe_ingest::RestSource,
    Sl: qe_ingest::Sleeper,
{
    inner: qe_ingest::BinanceHistorical<S, Sl>,
    request: qe_ingest::WindowRequest,
    present_bars: std::collections::BTreeSet<i64>,
    present_funding: std::collections::BTreeSet<i64>,
}

#[cfg(feature = "http")]
impl<S, Sl> BinanceHistoricalSource<S, Sl>
where
    S: qe_ingest::RestSource,
    Sl: qe_ingest::Sleeper,
{
    /// Wrap a configured [`qe_ingest::BinanceHistorical`] over a window request + the store's current
    /// coverage (the open-times already present, so covered/gap ranges are handled incrementally).
    #[must_use]
    pub fn new(
        inner: qe_ingest::BinanceHistorical<S, Sl>,
        request: qe_ingest::WindowRequest,
        present_bars: std::collections::BTreeSet<i64>,
        present_funding: std::collections::BTreeSet<i64>,
    ) -> Self {
        Self {
            inner,
            request,
            present_bars,
            present_funding,
        }
    }
}

#[cfg(feature = "http")]
impl BinanceHistoricalSource<qe_ingest::HttpRestSource, qe_ingest::RealSleeper> {
    /// A live source against `base` (e.g. [`qe_ingest::DEFAULT_REST_BASE`]) with the default retry policy.
    #[must_use]
    pub fn live(
        base: impl Into<String>,
        request: qe_ingest::WindowRequest,
        present_bars: std::collections::BTreeSet<i64>,
        present_funding: std::collections::BTreeSet<i64>,
    ) -> Self {
        let backfiller = qe_ingest::Backfiller::new(
            qe_ingest::HttpRestSource::new(base),
            qe_ingest::RetryPolicy::default(),
        );
        Self::new(
            qe_ingest::BinanceHistorical::new(backfiller),
            request,
            present_bars,
            present_funding,
        )
    }
}

#[cfg(feature = "http")]
impl<S, Sl> HistoricalSource for BinanceHistoricalSource<S, Sl>
where
    S: qe_ingest::RestSource,
    Sl: qe_ingest::Sleeper,
{
    fn fetch(&mut self) -> Result<HistoricalWindow, BootstrapError> {
        let window = self
            .inner
            .fetch_window(&self.request, &self.present_bars, &self.present_funding)
            .map_err(|e| BootstrapError::Decode(e.to_string()))?;
        // Klines-only calibration slice: premium / open-interest / mark-price stay empty (never faked).
        debug_assert_eq!(
            window.calibration_source,
            qe_ingest::CalibrationSource::Uncalibrated
        );
        Ok(HistoricalWindow {
            base: window.base,
            bars: window.bars,
            funding: window.funding,
            open_interest: Vec::new(),
            premium: Vec::new(),
            mark_price: Vec::new(),
        })
    }
}

#[cfg(all(test, feature = "http"))]
mod http_tests {
    use super::*;
    use qe_ingest::{
        Backfiller, BinanceHistorical, PageRequest, RestEndpoint, RestError, RestSource,
        RetryPolicy, TimedRow, WindowRequest,
    };
    use std::collections::BTreeSet;

    const M5: i64 = 5 * 60_000;

    /// An offline fake USDT-M REST source: serves flat-OHLC kline rows and one funding row, dispatched
    /// by endpoint. No network — proves the adapter + `run_ingest` end-to-end without live calls.
    struct FakeRest {
        klines: Vec<TimedRow>,
        funding: Vec<TimedRow>,
    }
    impl RestSource for FakeRest {
        fn fetch_page(&self, req: &PageRequest) -> Result<Vec<TimedRow>, RestError> {
            let rows = match req.endpoint {
                RestEndpoint::Klines(_) => &self.klines,
                RestEndpoint::FundingRate => &self.funding,
                _ => return Ok(Vec::new()),
            };
            Ok(rows
                .iter()
                .filter(|r| r.open_time_ms >= req.start_ms)
                .take(req.limit as usize)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn binance_source_plugs_into_run_ingest() {
        // Five 5m klines + one funding settlement, all closed relative to `now`.
        let klines: Vec<TimedRow> = (0..5)
            .map(|i| {
                let t = i * M5;
                TimedRow {
                    open_time_ms: t,
                    raw: format!(
                        r#"[{t},"100.0","101.0","99.0","100.5","1.0",{},"100.0",3]"#,
                        t + M5 - 1
                    ),
                }
            })
            .collect();
        let funding = vec![TimedRow {
            open_time_ms: 0,
            raw: r#"{"symbol":"BTCUSDT","fundingTime":0,"fundingRate":"0.00010000"}"#.to_owned(),
        }];

        let source = BinanceHistoricalSource::new(
            BinanceHistorical::new(Backfiller::new(
                FakeRest { klines, funding },
                RetryPolicy::default(),
            )),
            WindowRequest {
                symbol: InstrumentId::new("BTCUSDT").unwrap(),
                resolution: Resolution::M5,
                from_ms: 0,
                to_ms: 4 * M5,
                now_ms: 100 * M5, // everything closed
                limit: 10,
            },
            BTreeSet::new(),
            BTreeSet::new(),
        );

        let dir = tempfile::tempdir().unwrap();
        let params = IngestParams {
            store_path: dir.path().join("store"),
            map_size: 64 * 1024 * 1024,
            instrument: "BTCUSDT".to_owned(),
        };
        let mut src = source;
        let mut progress = |_p: u8, _s: &str, _m: &str| {};
        run_ingest(&params, &mut src, &mut progress).unwrap();

        // The decoded bars + funding landed in the store via the real write path.
        let store = MarketStore::open(&params.store_path, params.map_size).unwrap();
        let inst = InstrumentId::new("BTCUSDT").unwrap();
        let (first, last, bars) = store
            .coverage_bounds(&inst, Resolution::M5)
            .unwrap()
            .expect("bars were written");
        assert_eq!(bars, 5);
        assert_eq!(first.millis(), 0);
        assert_eq!(last.millis(), 4 * M5);
    }
}
