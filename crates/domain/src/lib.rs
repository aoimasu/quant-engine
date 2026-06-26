//! `qe-domain` — the shared vocabulary used by both the training and runtime pipelines.
//!
//! One definition of each core concept — instruments, time, bars, money, direction, vintage hash —
//! so training and runtime cannot drift apart (batch/streaming parity rests on this). Every type is
//! total and tested, and **money is exact fixed-point decimal — never a binary float**.
//!
//! Modules:
//! - [`instrument`] — [`InstrumentId`], [`Venue`]
//! - [`time`] — [`Timestamp`], [`TimeInterval`]
//! - [`resolution`] — [`Resolution`] (single shared bar-resolution definition)
//! - [`money`] — [`Price`], [`Qty`], [`Notional`], [`RoundingPolicy`]
//! - [`bar`] — [`Bar`] (OHLCVT)
//! - [`funding`] — [`FundingRate`], [`FundingRateSample`]
//! - [`side`] — [`Side`], [`Direction`]
//! - [`vintage`] — [`VintageHash`]

pub mod bar;
pub mod funding;
pub mod instrument;
pub mod money;
pub mod resolution;
pub mod side;
pub mod time;
pub mod vintage;

pub use bar::Bar;
pub use funding::{FundingRate, FundingRateSample};
pub use instrument::{InstrumentId, Venue};
pub use money::{Notional, Price, Qty, RoundingPolicy};
pub use resolution::Resolution;
pub use side::{Direction, Side};
pub use time::{TimeInterval, Timestamp};
pub use vintage::VintageHash;

use thiserror::Error;

/// Every construction-time validation failure in the domain vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// An instrument id was empty or contained non-alphanumeric characters.
    #[error("invalid instrument id `{value}`: {reason}")]
    InvalidInstrument {
        /// The rejected input.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A `Price` or `Qty` was constructed from a negative value.
    #[error("{kind} must not be negative (got {value})")]
    NegativeMoney {
        /// `"price"` or `"qty"`.
        kind: &'static str,
        /// The rejected value, rendered exactly.
        value: String,
    },

    /// Bar OHLC values violated `low <= {open, close} <= high`.
    #[error("invalid bar: {0}")]
    InvalidBar(String),

    /// A resolution string did not name a known [`Resolution`].
    #[error("invalid resolution `{0}`")]
    InvalidResolution(String),

    /// A vintage hash was not a 64-character lowercase-hex digest.
    #[error("invalid vintage hash: {0}")]
    InvalidVintageHash(&'static str),

    /// A time interval's end preceded its start.
    #[error("invalid time interval: end {end} precedes start {start}")]
    InvalidInterval {
        /// Start epoch-millis.
        start: i64,
        /// End epoch-millis.
        end: i64,
    },
}
