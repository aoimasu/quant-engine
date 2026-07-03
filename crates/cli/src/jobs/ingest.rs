//! The ingest job: populate a [`MarketStore`] from the injectable [`HistoricalSource`] seam, plus the
//! read-only [`coverage`] query the admin server's Market-data view (QE-257) calls (QE-253 Task 6).
//!
//! Deterministic: same inputs ⇒ same store contents and the same coverage rows; no wall-clock/RNG in
//! any output. Real Binance decoders live behind the default-off `http` feature (out of scope here);
//! [`run_ingest`] is exercised in tests with an in-memory source, and backtests use the committed
//! sample-store fixture.

use std::path::PathBuf;

use qe_domain::{FundingRate, FundingRateSample, InstrumentId, Timestamp};
use qe_runtime::HistoricalSource;
use qe_storage::{MarketStore, PremiumSample};

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
