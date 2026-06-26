//! Bar resolution — the single shared definition used by both pipelines (AC #2).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A bar/candle resolution. Defined **once** here so training and runtime agree exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Resolution {
    /// 1 minute.
    M1,
    /// 5 minutes.
    M5,
    /// 15 minutes.
    M15,
    /// 30 minutes.
    M30,
    /// 1 hour.
    H1,
    /// 4 hours.
    H4,
    /// 12 hours.
    H12,
    /// 1 day.
    D1,
}

impl Resolution {
    /// Every resolution, ascending by duration. Useful for reconstruction ordering.
    pub const ALL: [Resolution; 8] = [
        Resolution::M1,
        Resolution::M5,
        Resolution::M15,
        Resolution::M30,
        Resolution::H1,
        Resolution::H4,
        Resolution::H12,
        Resolution::D1,
    ];

    /// Length in whole minutes.
    #[must_use]
    pub fn minutes(self) -> u32 {
        match self {
            Resolution::M1 => 1,
            Resolution::M5 => 5,
            Resolution::M15 => 15,
            Resolution::M30 => 30,
            Resolution::H1 => 60,
            Resolution::H4 => 240,
            Resolution::H12 => 720,
            Resolution::D1 => 1_440,
        }
    }

    /// The canonical short code (`"5m"`, `"4h"`, `"1d"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::M1 => "1m",
            Resolution::M5 => "5m",
            Resolution::M15 => "15m",
            Resolution::M30 => "30m",
            Resolution::H1 => "1h",
            Resolution::H4 => "4h",
            Resolution::H12 => "12h",
            Resolution::D1 => "1d",
        }
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Resolution {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Resolution::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| DomainError::InvalidResolution(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_round_trips_for_every_variant() {
        for r in Resolution::ALL {
            assert_eq!(Resolution::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn minutes_are_strictly_increasing_in_all_order() {
        for pair in Resolution::ALL.windows(2) {
            assert!(pair[0].minutes() < pair[1].minutes());
        }
    }

    #[test]
    fn unknown_resolution_is_rejected() {
        assert!(matches!(
            Resolution::from_str("7m"),
            Err(DomainError::InvalidResolution(s)) if s == "7m"
        ));
    }
}
