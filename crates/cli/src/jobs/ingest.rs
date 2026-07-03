//! The ingest job: populate a [`MarketStore`] from the injectable [`HistoricalSource`] seam, plus the
//! read-only [`coverage`] query the admin server's Market-data view (QE-257) calls (QE-253 Task 6).
//!
//! Deterministic: same inputs ⇒ same store contents and the same coverage rows; no wall-clock/RNG in
//! any output. Real Binance decoders live behind the default-off `http` feature (out of scope here);
//! [`run_ingest`] is exercised in tests with an in-memory source, and backtests use the committed
//! sample-store fixture.

use std::path::PathBuf;

use qe_domain::{FundingRate, FundingRateSample, InstrumentId, Resolution, Timestamp};
use qe_runtime::HistoricalSource;
use qe_storage::{MarketStore, PremiumSample};

use super::RunError;

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

/// One row of the read-only market-data coverage query: the stored range + bar count for an
/// (instrument, resolution) pair. `from`/`to` are the **earliest / latest bar `open_time`** in
/// epoch-milliseconds (inclusive; `to` is the last bar's open time, not `open_time + resolution`).
///
/// Lives here (a `std`/`serde`-only struct) so the server crate (QE-257) can call the lib and reuse the
/// exact shape the CLI produces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageRow {
    /// Instrument symbol (e.g. `BTCUSDT`).
    pub symbol: String,
    /// Canonical resolution short code (e.g. `1h`).
    pub resolution: String,
    /// Earliest stored bar `open_time`, epoch-ms (inclusive).
    pub from: i64,
    /// Latest stored bar `open_time`, epoch-ms (inclusive).
    pub to: i64,
    /// Number of stored bars in `[from, to]`.
    pub bars: usize,
}

/// Scan `store` for every `(instrument, resolution)` pair and report the covered range + bar count.
///
/// For each instrument (in caller order) and each [`Resolution::ALL`] (ascending) that has at least one
/// bar, emits one [`CoverageRow`]. Deterministic: order and contents depend only on the store and the
/// `instruments` slice.
///
/// # Errors
/// [`RunError::Storage`] on an LMDB failure while scanning.
pub fn coverage(
    store: &MarketStore,
    instruments: &[InstrumentId],
) -> Result<Vec<CoverageRow>, RunError> {
    let mut rows = Vec::new();
    for instrument in instruments {
        for resolution in Resolution::ALL {
            // Full-range half-open scan `[MIN, MAX)`; bars come back chronological, so the first is the
            // earliest and the last the latest open_time.
            let bars = store.scan_bars(
                instrument,
                resolution,
                Timestamp::from_millis(i64::MIN),
                Timestamp::from_millis(i64::MAX),
            )?;
            let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
                continue;
            };
            rows.push(CoverageRow {
                symbol: instrument.as_str().to_owned(),
                resolution: resolution.as_str().to_owned(),
                from: first.open_time().millis(),
                to: last.open_time().millis(),
                bars: bars.len(),
            });
        }
    }
    Ok(rows)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_row_serialises_expected_shape() {
        let row = CoverageRow {
            symbol: "BTCUSDT".to_owned(),
            resolution: "1h".to_owned(),
            from: 1_609_459_200_000,
            to: 1_609_887_600_000,
            bars: 120,
        };
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(
            json,
            r#"{"symbol":"BTCUSDT","resolution":"1h","from":1609459200000,"to":1609887600000,"bars":120}"#
        );
        let back: CoverageRow = serde_json::from_str(&json).unwrap();
        assert_eq!(back, row);
    }
}
