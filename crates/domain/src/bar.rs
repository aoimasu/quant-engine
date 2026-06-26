//! OHLCVT bars — the shared candle type for both pipelines.

use serde::{Deserialize, Serialize};

use crate::money::{Price, Qty};
use crate::resolution::Resolution;
use crate::time::Timestamp;
use crate::DomainError;

/// An OHLCVT bar: open/high/low/close prices, volume, and trade count, over one [`Resolution`]
/// window starting at `open_time`.
///
/// `T` is the **trade count** (Binance kline "number of trades"). The OHLC invariant
/// `low <= {open, close} <= high` is enforced by [`Bar::new`] **and** by deserialisation (via a
/// validating [`TryFrom`]). Fields are private with getters, so the only ways to obtain a `Bar` —
/// `new` or serde — both validate; an invariant-violating bar cannot exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "BarWire")]
pub struct Bar {
    open_time: Timestamp,
    resolution: Resolution,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Qty,
    trades: u64,
}

/// Wire form of [`Bar`]: a flat record with the same fields, validated through [`Bar::new`] on the
/// way in and produced verbatim on the way out. Keeps the JSON shape identical to the struct.
#[derive(Serialize, Deserialize)]
struct BarWire {
    open_time: Timestamp,
    resolution: Resolution,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Qty,
    trades: u64,
}

impl<'de> Deserialize<'de> for Bar {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let w = BarWire::deserialize(deserializer)?;
        Bar::new(
            w.open_time,
            w.resolution,
            w.open,
            w.high,
            w.low,
            w.close,
            w.volume,
            w.trades,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl From<Bar> for BarWire {
    fn from(b: Bar) -> Self {
        BarWire {
            open_time: b.open_time,
            resolution: b.resolution,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
            trades: b.trades,
        }
    }
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

    /// Start of the bar's window (UTC).
    #[must_use]
    pub fn open_time(&self) -> Timestamp {
        self.open_time
    }
    /// The bar's resolution.
    #[must_use]
    pub fn resolution(&self) -> Resolution {
        self.resolution
    }
    /// Open price.
    #[must_use]
    pub fn open(&self) -> Price {
        self.open
    }
    /// Highest traded price in the window.
    #[must_use]
    pub fn high(&self) -> Price {
        self.high
    }
    /// Lowest traded price in the window.
    #[must_use]
    pub fn low(&self) -> Price {
        self.low
    }
    /// Close price.
    #[must_use]
    pub fn close(&self) -> Price {
        self.close
    }
    /// Traded base-asset volume.
    #[must_use]
    pub fn volume(&self) -> Qty {
        self.volume
    }
    /// Number of trades in the window.
    #[must_use]
    pub fn trades(&self) -> u64 {
        self.trades
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
        assert_eq!(bar.trades(), 42);
        assert_eq!(bar.resolution(), Resolution::M5);
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
    fn serde_round_trips_valid_bar() {
        let bar = valid();
        let json = serde_json::to_string(&bar).unwrap();
        assert_eq!(serde_json::from_str::<Bar>(&json).unwrap(), bar);
    }

    #[test]
    fn deserialize_rejects_invariant_violating_bar() {
        // high < low and close out of range: must be rejected at the serde boundary.
        let json = r#"{
            "open_time": 1000,
            "resolution": "M1",
            "open": "100",
            "high": "90",
            "low": "95",
            "close": "999",
            "volume": "0",
            "trades": 0
        }"#;
        assert!(serde_json::from_str::<Bar>(json).is_err());
        // A negative price inside a bar is rejected by Price's own validating deserialize.
        let neg = r#"{
            "open_time": 1000,
            "resolution": "M1",
            "open": "-1",
            "high": "110",
            "low": "95",
            "close": "100",
            "volume": "0",
            "trades": 0
        }"#;
        assert!(serde_json::from_str::<Bar>(neg).is_err());
    }
}
