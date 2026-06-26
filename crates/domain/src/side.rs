//! Order side and position direction, with total conversions between them.

use serde::{Deserialize, Serialize};

/// The side of an order or fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    /// Buy / bid.
    Buy,
    /// Sell / ask.
    Sell,
}

/// The direction of a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Long (profits when price rises).
    Long,
    /// Short (profits when price falls).
    Short,
}

impl Side {
    /// The position direction opening with this side.
    #[must_use]
    pub fn direction(self) -> Direction {
        match self {
            Side::Buy => Direction::Long,
            Side::Sell => Direction::Short,
        }
    }

    /// The opposite side.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

impl Direction {
    /// The order side that opens this direction.
    #[must_use]
    pub fn side(self) -> Side {
        match self {
            Direction::Long => Side::Buy,
            Direction::Short => Side::Sell,
        }
    }

    /// The opposite direction.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Direction::Long => Direction::Short,
            Direction::Short => Direction::Long,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_direction_round_trip() {
        for side in [Side::Buy, Side::Sell] {
            assert_eq!(side.direction().side(), side);
        }
        for dir in [Direction::Long, Direction::Short] {
            assert_eq!(dir.side().direction(), dir);
        }
    }

    #[test]
    fn opposites_are_involutive() {
        assert_eq!(Side::Buy.opposite().opposite(), Side::Buy);
        assert_eq!(Direction::Long.opposite().opposite(), Direction::Long);
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Direction::Long.opposite(), Direction::Short);
    }
}
