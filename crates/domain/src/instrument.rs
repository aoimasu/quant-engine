//! Instrument identity and trading venue.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A canonical instrument symbol, e.g. `BTCUSDT`.
///
/// Stored upper-cased and validated as non-empty ASCII alphanumeric, so the same instrument always
/// has the same id regardless of input casing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstrumentId(String);

impl InstrumentId {
    /// Validate and canonicalise (upper-case) a symbol.
    ///
    /// # Errors
    /// [`DomainError::InvalidInstrument`] if the symbol is empty or contains a non-alphanumeric
    /// character.
    pub fn new(symbol: impl AsRef<str>) -> Result<Self, DomainError> {
        let raw = symbol.as_ref();
        if raw.is_empty() {
            return Err(DomainError::InvalidInstrument {
                value: raw.to_owned(),
                reason: "must not be empty",
            });
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(DomainError::InvalidInstrument {
                value: raw.to_owned(),
                reason: "must be ASCII alphanumeric",
            });
        }
        Ok(InstrumentId(raw.to_ascii_uppercase()))
    }

    /// The canonical symbol string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The trading venue. The platform targets Binance USDT-M perpetual futures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Venue {
    /// Binance USDT-margined perpetual futures.
    BinanceUsdtPerp,
}

impl Venue {
    /// A stable lowercase identifier for storage/lineage.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Venue::BinanceUsdtPerp => "binance-usdt-perp",
        }
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_to_uppercase() {
        let id = InstrumentId::new("btcUSDt").unwrap();
        assert_eq!(id.as_str(), "BTCUSDT");
        assert_eq!(id, InstrumentId::new("BTCUSDT").unwrap());
    }

    #[test]
    fn rejects_empty_and_non_alphanumeric() {
        assert!(InstrumentId::new("").is_err());
        assert!(InstrumentId::new("BTC-USDT").is_err());
        assert!(InstrumentId::new("BTC/USDT").is_err());
    }

    #[test]
    fn venue_round_trips_via_str() {
        assert_eq!(Venue::BinanceUsdtPerp.as_str(), "binance-usdt-perp");
        assert_eq!(Venue::BinanceUsdtPerp.to_string(), "binance-usdt-perp");
    }
}
