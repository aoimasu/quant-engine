//! The **canonical series set** — fixed by fusion (QE-104), not by the fetchers.
//!
//! QE-101/102 fetchers speak their own source vocabulary (`source::DataKind`, REST endpoints) and
//! stay source-abstract; fusion owns the one canonical list the downstream corpus is built from.

use serde::Serialize;

/// The canonical normalised series produced by fusion. Fixed here so the storage/signal stages see
/// one stable vocabulary regardless of which ingress path supplied the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum CanonicalSeries {
    /// Perpetual-futures OHLCVT klines (the base bar series).
    PerpKlines,
    /// Funding-rate series.
    Funding,
    /// Premium-index series.
    PremiumIndex,
    /// Spot OHLCVT klines (the underlier).
    SpotKlines,
    /// `/futures/data/*` metrics (top-trader L/S, OI, taker volume).
    FuturesMetrics,
    /// Derived perp-minus-spot spread to the underlier.
    SpreadToUnderlier,
}

impl CanonicalSeries {
    /// Every canonical series, in a fixed order (the column order of the fused corpus).
    pub const ALL: [CanonicalSeries; 6] = [
        CanonicalSeries::PerpKlines,
        CanonicalSeries::Funding,
        CanonicalSeries::PremiumIndex,
        CanonicalSeries::SpotKlines,
        CanonicalSeries::FuturesMetrics,
        CanonicalSeries::SpreadToUnderlier,
    ];

    /// A stable lowercase identifier (storage keys, lineage, Arrow column names).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CanonicalSeries::PerpKlines => "perp_klines",
            CanonicalSeries::Funding => "funding",
            CanonicalSeries::PremiumIndex => "premium_index",
            CanonicalSeries::SpotKlines => "spot_klines",
            CanonicalSeries::FuturesMetrics => "futures_metrics",
            CanonicalSeries::SpreadToUnderlier => "spread_to_underlier",
        }
    }
}

impl std::fmt::Display for CanonicalSeries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_complete_and_ordered() {
        // Fixed canonical set of six; the order is load-bearing (it is the fused column order).
        assert_eq!(CanonicalSeries::ALL.len(), 6);
        assert_eq!(CanonicalSeries::ALL[0], CanonicalSeries::PerpKlines);
        assert_eq!(CanonicalSeries::ALL[5], CanonicalSeries::SpreadToUnderlier);
    }

    #[test]
    fn identifiers_are_stable_and_unique() {
        let ids: Vec<&str> = CanonicalSeries::ALL.iter().map(|s| s.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "identifiers must be unique");
        assert_eq!(CanonicalSeries::SpotKlines.to_string(), "spot_klines");
    }
}
