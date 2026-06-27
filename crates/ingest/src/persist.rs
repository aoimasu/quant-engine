//! Persist the fused market corpus into the QE-010 LMDB [`MarketStore`] (QE-105).
//!
//! The store schema is **typed** (`Bar`/funding/premium/futures), so this bridge persists the typed
//! [`FusedMarket`] bundle — the coalesced + adjusted bars (QE-104 [`coalesce_bars`]/[`adjust_bar`])
//! plus the typed scalar samples — not the scalar/Arrow [`crate::fuse::FusedCorpus`] analytical view.
//!
//! Writes are **idempotent keyed by lineage**: [`persist_fused`] skips entirely when the vintage's
//! lineage id is already recorded in the store's ledger, so a repeated ingest→fuse→persist run is a
//! clean no-op and the store carries an auditable record of the vintages it holds.

use qe_domain::{Bar, DomainError, FundingRateSample, InstrumentId};
use qe_storage::{FuturesMetrics, MarketStore, PremiumSample, StorageError};
use thiserror::Error;

use crate::coalesce::coalesce_bars;
use crate::derive::{adjust_bar, Adjustment};

/// The typed, fused market records for one instrument, ready to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FusedMarket {
    /// The instrument these records belong to.
    pub instrument: InstrumentId,
    /// Coalesced + adjusted OHLCVT bars (each carries its own resolution + open time).
    pub bars: Vec<Bar>,
    /// Funding-rate samples.
    pub funding: Vec<FundingRateSample>,
    /// Premium / spread-to-underlier samples.
    pub premium: Vec<PremiumSample>,
    /// Futures positioning/liquidity metrics.
    pub futures: Vec<FuturesMetrics>,
}

/// Coalesce daily/monthly bar partitions and apply the split/contract `adjustment` — the "fuse"
/// step for the bar series (QE-104), producing the deterministic typed bars to persist.
///
/// # Errors
/// [`DomainError`] if an adjustment produces an invalid bar (e.g. a negative price factor).
pub fn fused_bars(
    perp_partitions: &[Vec<Bar>],
    adjustment: Adjustment,
) -> Result<Vec<Bar>, DomainError> {
    coalesce_bars(perp_partitions)
        .iter()
        .map(|bar| adjust_bar(bar, adjustment))
        .collect()
}

/// Whether a persist actually wrote, or skipped because the lineage was already present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistStatus {
    /// The records were written and the lineage newly recorded.
    Persisted,
    /// The lineage was already recorded; nothing was written (idempotent no-op).
    AlreadyPersisted,
}

/// The outcome of a [`persist_fused`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistReport {
    /// The vintage lineage id this persist was keyed by.
    pub lineage_id: String,
    /// Whether records were written or the call was an idempotent skip.
    pub status: PersistStatus,
    /// Bars written (0 on a skip).
    pub bars: usize,
    /// Funding samples written (0 on a skip).
    pub funding: usize,
    /// Premium samples written (0 on a skip).
    pub premium: usize,
    /// Futures-metrics samples written (0 on a skip).
    pub futures: usize,
}

/// Errors from persisting the fused corpus.
#[derive(Debug, Error)]
pub enum PersistError {
    /// A failure in the underlying market store.
    #[error("market store error: {0}")]
    Storage(#[from] StorageError),
}

/// Persist `market` into `store`, keyed by `lineage_id` (QE-105).
///
/// If `lineage_id` is already recorded in the store's ledger, this is an idempotent **no-op**
/// (returns [`PersistStatus::AlreadyPersisted`] with zero counts). Otherwise it writes every record
/// kind — each `put_*` is an upsert, so even a forced re-key is safe — then records the lineage so
/// subsequent runs skip.
///
/// **Atomicity / crash-safety.** This spans **five separate write transactions** (one per `put_*`
/// plus the ledger), not one atomic transaction. Two consequences, both deliberate and bounded by
/// the lineage-last ordering:
/// - The lineage is recorded **last**, so a crash mid-persist leaves the vintage *unrecorded* — the
///   next run re-enters the write path (not the skip path) and, because every `put_*` is an upsert,
///   re-writing is idempotent and **self-heals** the partial state.
/// - A reader concurrent with an in-progress persist may observe a **partially-written** vintage
///   (e.g. bars present, futures not yet). For the QE-105 AC (a completed run is reproducible +
///   range-queryable) this is sufficient. If atomic *vintage visibility* is needed later, expose a
///   `MarketStore` API that accepts an external `RwTxn` and write all kinds + the ledger in one txn.
///
/// # Errors
/// [`PersistError`] on any market-store failure.
pub fn persist_fused(
    store: &MarketStore,
    lineage_id: &str,
    market: &FusedMarket,
) -> Result<PersistReport, PersistError> {
    if store.has_lineage(lineage_id)? {
        return Ok(PersistReport {
            lineage_id: lineage_id.to_owned(),
            status: PersistStatus::AlreadyPersisted,
            bars: 0,
            funding: 0,
            premium: 0,
            futures: 0,
        });
    }

    store.put_bars(&market.instrument, &market.bars)?;
    store.put_funding(&market.funding)?;
    store.put_premium(&market.premium)?;
    store.put_futures(&market.futures)?;
    store.record_lineage(lineage_id)?;

    Ok(PersistReport {
        lineage_id: lineage_id.to_owned(),
        status: PersistStatus::Persisted,
        bars: market.bars.len(),
        funding: market.funding.len(),
        premium: market.premium.len(),
        futures: market.futures.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{FundingRate, Price, Qty, Resolution, Timestamp};
    use rust_decimal::Decimal;

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

    fn store(dir: &std::path::Path) -> MarketStore {
        MarketStore::open(dir, 10 * 1024 * 1024).unwrap()
    }

    fn sample_market() -> FusedMarket {
        FusedMarket {
            instrument: inst(),
            bars: vec![bar_at(0, 100), bar_at(300_000, 101)],
            funding: vec![FundingRateSample {
                instrument: inst(),
                time: Timestamp::from_millis(0),
                rate: FundingRate::new(Decimal::new(1, 4)),
            }],
            premium: vec![PremiumSample {
                instrument: inst(),
                time: Timestamp::from_millis(0),
                premium: Decimal::new(2, 4),
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
    fn fused_bars_coalesces_and_adjusts() {
        // Two partitions, the second overriding the first at t=0 (last-wins), 2x price adjustment.
        let parts = vec![
            vec![bar_at(0, 50)],
            vec![bar_at(0, 100), bar_at(300_000, 101)],
        ];
        let adj = Adjustment {
            price_factor: Decimal::from(2),
            qty_factor: Decimal::ONE,
        };
        let bars = fused_bars(&parts, adj).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].close().get(), Decimal::from(200)); // 100 (last-wins) * 2
        assert_eq!(bars[1].close().get(), Decimal::from(202)); // 101 * 2
    }

    #[test]
    fn persist_writes_then_skips_same_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let market = sample_market();

        let first = persist_fused(&store, "vintage-1", &market).unwrap();
        assert_eq!(first.status, PersistStatus::Persisted);
        assert_eq!(first.bars, 2);
        assert_eq!(first.funding, 1);
        assert!(store.has_lineage("vintage-1").unwrap());

        // Same lineage → idempotent no-op.
        let second = persist_fused(&store, "vintage-1", &market).unwrap();
        assert_eq!(second.status, PersistStatus::AlreadyPersisted);
        assert_eq!(second.bars, 0);
    }
}
