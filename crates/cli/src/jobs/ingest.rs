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
