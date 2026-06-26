//! Market-data record types not already in `qe-domain`.
//!
//! Decimals serialise as exact strings (no precision loss through the store's JSON value codec).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use qe_domain::{InstrumentId, Timestamp};

/// Premium / spread-to-underlier sample: the perpetual's premium over its underlier, as a signed
/// fraction (positive = perp trades above spot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PremiumSample {
    /// The instrument the sample applies to.
    pub instrument: InstrumentId,
    /// When the sample was taken (UTC).
    pub time: Timestamp,
    /// Premium over the underlier, as a signed fraction.
    #[serde(with = "rust_decimal::serde::str")]
    pub premium: Decimal,
}

/// Futures positioning/liquidity metrics for an instrument at a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuturesMetrics {
    /// The instrument the metrics apply to.
    pub instrument: InstrumentId,
    /// When the metrics were sampled (UTC).
    pub time: Timestamp,
    /// Top-trader long/short account ratio.
    #[serde(with = "rust_decimal::serde::str")]
    pub long_short_ratio: Decimal,
    /// Open interest (in base or contracts, per the feed).
    #[serde(with = "rust_decimal::serde::str")]
    pub open_interest: Decimal,
    /// Taker buy/sell volume ratio.
    #[serde(with = "rust_decimal::serde::str")]
    pub taker_buy_sell_ratio: Decimal,
}
