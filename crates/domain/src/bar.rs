//! OHLCVT bars — the shared candle type for both pipelines.

use serde::{Deserialize, Serialize};

use crate::money::{Price, Qty};
use crate::resolution::Resolution;
use crate::time::Timestamp;
use crate::DomainError;

/// An OHLCVT bar: open/high/low/close prices, volume, and trade count, over one [`Resolution`]
/// window starting at `open_time`.
///
/// `T` is the **trade count** (Binance kline "number of trades"). Construction validates the OHLC
/// invariant `low <= {open, close} <= high`; `volume` is non-negative by [`Qty`]'s type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bar {
    /// Start of the bar's window (UTC).
    pub open_time: Timestamp,
    /// The bar's resolution.
    pub resolution: Resolution,
    /// Open price.
    pub open: Price,
    /// Highest traded price in the window.
    pub high: Price,
    /// Lowest traded price in the window.
    pub low: Price,
    /// Close price.
    pub close: Price,
    /// Traded base-asset volume.
    pub volume: Qty,
    /// Number of trades in the window.
    pub trades: u64,
}

impl Bar {
    /// Construct a bar, validating the OHLC invariant.
    ///
    /// # Errors
    /// [`DomainError::InvalidBar`] if `high < low`, or `open`/`close` falls outside `[low, high]`.
    #[allow(clippy::too_many_arguments)] // an OHLCVT bar simply has this many fields
    pub fn new(
        open_time: Timestamp,
        resolution: Resolution,
        open: Price,
        high: Price,
        low: Price,
        close: Price,
        volume: Qty,
        trades: u64,
    ) -> Result<Self, DomainError> {
        if high < low {
            return Err(DomainError::InvalidBar(format!("high {high} < low {low}")));
        }
        for (name, p) in [("open", open), ("close", close)] {
            if p < low || p > high {
                return Err(DomainError::InvalidBar(format!(
                    "{name} {p} outside [low {low}, high {high}]"
                )));
            }
        }
        Ok(Bar {
            open_time,
            resolution,
            open,
            high,
            low,
            close,
            volume,
            trades,
        })
    }

    /// The bar's price range, `high - low` (always `>= 0`).
    #[must_use]
    pub fn range(&self) -> rust_decimal::Decimal {
        self.high.get() - self.low.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn price(s: &str) -> Price {
        Price::new(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn valid() -> Bar {
        Bar::new(
            Timestamp::from_secs(1_700_000_000),
            Resolution::M5,
            price("100"),
            price("110"),
            price("95"),
            price("105"),
            Qty::new(Decimal::from_str("12.5").unwrap()).unwrap(),
            42,
        )
        .unwrap()
    }

    #[test]
    fn accepts_consistent_ohlc() {
        let bar = valid();
        assert_eq!(bar.range(), Decimal::from_str("15").unwrap());
        assert_eq!(bar.trades, 42);
    }

    #[test]
    fn rejects_high_below_low() {
        let err = Bar::new(
            Timestamp::from_secs(1),
            Resolution::M1,
            price("100"),
            price("90"), // high < low
            price("95"),
            price("96"),
            Qty::ZERO,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, DomainError::InvalidBar(_)));
    }

    #[test]
    fn rejects_close_outside_range() {
        let err = Bar::new(
            Timestamp::from_secs(1),
            Resolution::M1,
            price("100"),
            price("110"),
            price("95"),
            price("120"), // close > high
            Qty::ZERO,
            0,
        )
        .unwrap_err();
        assert!(matches!(err, DomainError::InvalidBar(_)));
    }

    #[test]
    fn serde_round_trips() {
        let bar = valid();
        let json = serde_json::to_string(&bar).unwrap();
        assert_eq!(serde_json::from_str::<Bar>(&json).unwrap(), bar);
    }
}
