//! UTC time with explicit millisecond precision.
//!
//! [`Timestamp`] is epoch-milliseconds (UTC) — no timezone ambiguity, no sub-millisecond surprises.
//! [`TimeInterval`] is a half-open `[start, end)` range used for windows and bar coverage.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A UTC instant, stored as **milliseconds since the Unix epoch**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(i64);

impl Timestamp {
    /// From epoch-milliseconds.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Timestamp(millis)
    }

    /// From whole epoch-seconds.
    #[must_use]
    pub const fn from_secs(secs: i64) -> Self {
        Timestamp(secs * 1_000)
    }

    /// Epoch-milliseconds.
    #[must_use]
    pub const fn millis(self) -> i64 {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// A half-open time interval `[start, end)`.
///
/// Deserialisation goes through [`TimeInterval::new`] (via [`TryFrom`]), so a reversed `[end <
/// start]` interval cannot enter through serde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "TimeIntervalWire")]
pub struct TimeInterval {
    start: Timestamp,
    end: Timestamp,
}

/// Wire form of [`TimeInterval`] — deserialised then validated through [`TimeInterval::new`].
#[derive(Deserialize)]
struct TimeIntervalWire {
    start: Timestamp,
    end: Timestamp,
}

impl TryFrom<TimeIntervalWire> for TimeInterval {
    type Error = DomainError;

    fn try_from(wire: TimeIntervalWire) -> Result<Self, Self::Error> {
        TimeInterval::new(wire.start, wire.end)
    }
}

impl TimeInterval {
    /// Construct `[start, end)`.
    ///
    /// # Errors
    /// [`DomainError::InvalidInterval`] if `end < start`. An empty interval (`end == start`) is
    /// allowed.
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self, DomainError> {
        if end < start {
            return Err(DomainError::InvalidInterval {
                start: start.millis(),
                end: end.millis(),
            });
        }
        Ok(TimeInterval { start, end })
    }

    /// The start instant (inclusive).
    #[must_use]
    pub fn start(self) -> Timestamp {
        self.start
    }

    /// The end instant (exclusive).
    #[must_use]
    pub fn end(self) -> Timestamp {
        self.end
    }

    /// Duration in milliseconds (`end - start`, always `>= 0`).
    #[must_use]
    pub fn duration_millis(self) -> i64 {
        self.end.millis() - self.start.millis()
    }

    /// Whether `at` falls in `[start, end)`.
    #[must_use]
    pub fn contains(self, at: Timestamp) -> bool {
        self.start <= at && at < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_conversion() {
        assert!(Timestamp::from_millis(1) < Timestamp::from_millis(2));
        assert_eq!(Timestamp::from_secs(3).millis(), 3_000);
    }

    #[test]
    fn interval_rejects_reversed_bounds() {
        let err =
            TimeInterval::new(Timestamp::from_millis(10), Timestamp::from_millis(5)).unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidInterval { start: 10, end: 5 }
        ));
    }

    #[test]
    fn interval_is_half_open() {
        let iv = TimeInterval::new(Timestamp::from_millis(10), Timestamp::from_millis(20)).unwrap();
        assert_eq!(iv.duration_millis(), 10);
        assert!(iv.contains(Timestamp::from_millis(10)));
        assert!(iv.contains(Timestamp::from_millis(19)));
        assert!(!iv.contains(Timestamp::from_millis(20))); // end exclusive
        assert!(!iv.contains(Timestamp::from_millis(9)));
    }

    #[test]
    fn empty_interval_allowed() {
        let iv = TimeInterval::new(Timestamp::from_millis(5), Timestamp::from_millis(5)).unwrap();
        assert_eq!(iv.duration_millis(), 0);
        assert!(!iv.contains(Timestamp::from_millis(5)));
    }

    #[test]
    fn deserialize_rejects_reversed_interval() {
        assert!(serde_json::from_str::<TimeInterval>(r#"{"start":100,"end":50}"#).is_err());
        let iv: TimeInterval = serde_json::from_str(r#"{"start":10,"end":20}"#).unwrap();
        assert_eq!(
            iv,
            TimeInterval::new(Timestamp::from_millis(10), Timestamp::from_millis(20)).unwrap()
        );
    }
}
