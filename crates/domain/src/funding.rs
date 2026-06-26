//! Perpetual-futures funding rate samples.

use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::instrument::InstrumentId;
use crate::time::Timestamp;

/// A funding rate — a small **signed** decimal fraction (longs pay shorts when positive). Signed,
/// so it is a plain decimal newtype rather than a non-negative money type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FundingRate(#[serde(with = "rust_decimal::serde::str")] Decimal);

impl FundingRate {
    /// Wrap a decimal funding rate (may be negative).
    #[must_use]
    pub fn new(rate: Decimal) -> Self {
        FundingRate(rate)
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }
}

impl fmt::Display for FundingRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A funding rate observed for an instrument at a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingRateSample {
    /// The instrument the rate applies to.
    pub instrument: InstrumentId,
    /// When the rate was sampled (UTC).
    pub time: Timestamp,
    /// The funding rate.
    pub rate: FundingRate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn funding_rate_can_be_negative_and_round_trips() {
        let rate = FundingRate::new(Decimal::from_str("-0.000125").unwrap());
        assert!(rate.get() < Decimal::ZERO);
        let json = serde_json::to_string(&rate).unwrap();
        assert_eq!(json, "\"-0.000125\"");
        assert_eq!(serde_json::from_str::<FundingRate>(&json).unwrap(), rate);
    }

    #[test]
    fn sample_serde_round_trips() {
        let sample = FundingRateSample {
            instrument: InstrumentId::new("BTCUSDT").unwrap(),
            time: Timestamp::from_secs(1_700_000_000),
            rate: FundingRate::new(Decimal::from_str("0.0001").unwrap()),
        };
        let json = serde_json::to_string(&sample).unwrap();
        assert_eq!(
            serde_json::from_str::<FundingRateSample>(&json).unwrap(),
            sample
        );
    }
}
