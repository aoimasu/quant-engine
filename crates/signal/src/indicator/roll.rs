//! A fixed-capacity ring buffer over exact decimals — the substrate for every finite-window (FIR)
//! indicator, so a value depends on **exactly** the last `cap` pushes and nothing older (QE-107
//! AC #2).

use rust_decimal::{Decimal, MathematicalOps};

/// A bounded window of the most recent `cap` values. Pushing beyond `cap` drops the oldest.
#[derive(Debug, Clone)]
pub struct Roll {
    cap: usize,
    buf: std::collections::VecDeque<Decimal>,
}

impl Roll {
    /// A window holding at most `cap` values (`cap` must be ≥ 1).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Roll {
            cap: cap.max(1),
            buf: std::collections::VecDeque::with_capacity(cap.max(1)),
        }
    }

    /// Push a value, evicting the oldest if at capacity.
    pub fn push(&mut self, v: Decimal) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    /// Whether the window holds exactly `cap` values.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.buf.len() == self.cap
    }

    /// The window capacity.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// The most recently pushed value, if any.
    #[must_use]
    pub fn last(&self) -> Option<Decimal> {
        self.buf.back().copied()
    }

    /// The oldest buffered value, if any.
    #[must_use]
    pub fn first(&self) -> Option<Decimal> {
        self.buf.front().copied()
    }

    /// Sum of buffered values.
    #[must_use]
    pub fn sum(&self) -> Decimal {
        self.buf.iter().copied().sum()
    }

    /// Arithmetic mean, or `None` if empty.
    #[must_use]
    pub fn mean(&self) -> Option<Decimal> {
        if self.buf.is_empty() {
            return None;
        }
        Some(self.sum() / Decimal::from(self.buf.len()))
    }

    /// Maximum buffered value, or `None` if empty.
    #[must_use]
    pub fn max(&self) -> Option<Decimal> {
        self.buf.iter().copied().max()
    }

    /// Minimum buffered value, or `None` if empty.
    #[must_use]
    pub fn min(&self) -> Option<Decimal> {
        self.buf.iter().copied().min()
    }

    /// Population standard deviation, or `None` if empty. Exact mean; `sqrt` is the one place a
    /// transcendental appears (deterministic given equal inputs).
    #[must_use]
    pub fn std_pop(&self) -> Option<Decimal> {
        let mean = self.mean()?;
        let n = Decimal::from(self.buf.len());
        let var = self
            .buf
            .iter()
            .map(|&v| {
                let d = v - mean;
                d * d
            })
            .sum::<Decimal>()
            / n;
        var.sqrt()
    }

    /// Mean absolute deviation from the mean, or `None` if empty (used by CCI).
    #[must_use]
    pub fn mean_abs_dev(&self) -> Option<Decimal> {
        let mean = self.mean()?;
        let n = Decimal::from(self.buf.len());
        Some(self.buf.iter().map(|&v| (v - mean).abs()).sum::<Decimal>() / n)
    }

    /// Iterate buffered values oldest → newest.
    pub fn iter(&self) -> impl Iterator<Item = Decimal> + '_ {
        self.buf.iter().copied()
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
    fn evicts_oldest_at_capacity() {
        let mut r = Roll::new(3);
        for v in [1, 2, 3, 4] {
            r.push(Decimal::from(v));
        }
        assert!(r.is_full());
        assert_eq!(r.first(), Some(Decimal::from(2))); // 1 evicted
        assert_eq!(r.last(), Some(Decimal::from(4)));
        assert_eq!(r.sum(), Decimal::from(9)); // 2+3+4
    }

    #[test]
    fn stats_are_exact() {
        let mut r = Roll::new(4);
        for v in [2, 4, 4, 6] {
            r.push(Decimal::from(v));
        }
        assert_eq!(r.sum(), Decimal::from(16));
        assert_eq!(r.mean(), Some(Decimal::from(4)));
        assert_eq!(r.max(), Some(Decimal::from(6)));
        assert_eq!(r.min(), Some(Decimal::from(2)));
        // variance = ((4)+(0)+(0)+(4))/4 = 2 → std = sqrt(2)
        assert_eq!(r.std_pop(), Decimal::from(2).sqrt());
        // mean abs dev = (2+0+0+2)/4 = 1
        assert_eq!(r.mean_abs_dev(), Some(d("1")));
    }
}
