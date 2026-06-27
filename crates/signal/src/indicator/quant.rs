//! Deterministic, point-wise quantisation of a continuous indicator value into a discrete state.
//!
//! Point-wise on purpose: no rolling quantiles, no dataset-wide fit — so quantisation never peeks at
//! future data and is identical batch vs streaming (QE-107 AC #1).

use rust_decimal::Decimal;

/// A discrete indicator state in `0..num_states`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QState(u16);

impl QState {
    /// The 0-based bucket index.
    #[must_use]
    pub fn index(self) -> u16 {
        self.0
    }
}

/// Maps a continuous value to a [`QState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quantiser {
    /// Equal-width bins over `[min, max]`; values are clamped into range. `states ≥ 1`.
    Linear {
        /// Lower bound of the range.
        min: Decimal,
        /// Upper bound of the range.
        max: Decimal,
        /// Number of bins.
        states: u16,
    },
    /// Ascending interior thresholds; bucket = count of `edges` strictly less than the value.
    /// Produces `edges.len() + 1` states (so signed/zone factors get symmetric bands).
    Bands {
        /// Strictly-ascending interior edges.
        edges: Vec<Decimal>,
    },
}

impl Quantiser {
    /// The number of states this quantiser produces.
    #[must_use]
    pub fn states(&self) -> u16 {
        match self {
            Quantiser::Linear { states, .. } => (*states).max(1),
            Quantiser::Bands { edges } => edges.len() as u16 + 1,
        }
    }

    /// Quantise `value` into `0..states()`.
    #[must_use]
    pub fn quantise(&self, value: Decimal) -> QState {
        match self {
            Quantiser::Linear { min, max, states } => {
                let states = (*states).max(1);
                if max <= min {
                    return QState(0);
                }
                if value <= *min {
                    return QState(0);
                }
                if value >= *max {
                    return QState(states - 1);
                }
                let frac = (value - *min) / (*max - *min); // in (0, 1)
                let bucket = (frac * Decimal::from(states))
                    .floor()
                    .try_into()
                    .unwrap_or(0u64);
                QState((bucket as u16).min(states - 1))
            }
            Quantiser::Bands { edges } => {
                let bucket = edges.iter().filter(|&&e| e < value).count();
                QState(bucket as u16)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn linear_bins_and_clamps() {
        let q = Quantiser::Linear {
            min: Decimal::ZERO,
            max: Decimal::from(100),
            states: 4,
        };
        assert_eq!(q.states(), 4);
        assert_eq!(q.quantise(d("-5")).index(), 0); // clamp low
        assert_eq!(q.quantise(d("0")).index(), 0);
        assert_eq!(q.quantise(d("24")).index(), 0); // [0,25)
        assert_eq!(q.quantise(d("25")).index(), 1); // [25,50)
        assert_eq!(q.quantise(d("75")).index(), 3); // [75,100)
        assert_eq!(q.quantise(d("100")).index(), 3); // clamp high
        assert_eq!(q.quantise(d("200")).index(), 3);
    }

    #[test]
    fn bands_count_edges_below() {
        // Symmetric signed bands: <-1 | [-1,0) | [0,1) | >=1  → 4 states.
        let q = Quantiser::Bands {
            edges: vec![d("-1"), d("0"), d("1")],
        };
        assert_eq!(q.states(), 4);
        assert_eq!(q.quantise(d("-2")).index(), 0); // no edge < -2
        assert_eq!(q.quantise(d("-0.5")).index(), 1); // {-1} < -0.5
        assert_eq!(q.quantise(d("0")).index(), 1); // {-1} < 0 (0 not < 0)
        assert_eq!(q.quantise(d("0.5")).index(), 2); // {-1, 0} < 0.5
        assert_eq!(q.quantise(d("5")).index(), 3); // all three < 5
    }
}
