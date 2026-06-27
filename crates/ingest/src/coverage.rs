//! Coverage maps and short-history flags.
//!
//! Funding / premium-index / `/futures/data` series typically start later and have coarser cadence
//! than base klines. Surfacing that — rather than silently padding it — is the point: a series with
//! shorter history is flagged so downstream stages don't infer a phantom edge from absent data.

use serde::Serialize;

/// The covered range and slot accounting for one fixed-interval series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Coverage {
    /// First present timestamp (epoch ms), or `None` if the series is empty.
    pub first_ms: Option<i64>,
    /// Last present timestamp (epoch ms), or `None` if empty.
    pub last_ms: Option<i64>,
    /// Distinct present slots.
    pub present: u64,
    /// Slots expected across `[first, last]` at `interval_ms`.
    pub expected: u64,
    /// `expected - present` (absent slots inside the covered range).
    pub missing: u64,
}

/// Compute the coverage of a series from its `timestamps` and `interval_ms`.
#[must_use]
pub fn coverage(timestamps: &[i64], interval_ms: i64) -> Coverage {
    let set: std::collections::BTreeSet<i64> = timestamps.iter().copied().collect();
    let first = set.iter().next().copied();
    let last = set.iter().next_back().copied();
    let present = set.len() as u64;
    let expected = match (first, last) {
        (Some(f), Some(l)) if interval_ms > 0 => ((l - f) / interval_ms + 1) as u64,
        _ => present,
    };
    Coverage {
        first_ms: first,
        last_ms: last,
        present,
        expected,
        missing: expected.saturating_sub(present),
    }
}

/// A named series whose history is shorter than the base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortCoverage {
    /// The series label (e.g. `"fundingRate"`).
    pub series: String,
    /// `true` if it starts strictly later than the base.
    pub starts_late: bool,
    /// `true` if it ends strictly earlier than the base.
    pub ends_early: bool,
}

/// Flag any `(label, coverage)` whose `[first, last]` is strictly inside the base's — i.e. it starts
/// later and/or ends earlier, so its history is shorter and must not be silently extrapolated.
#[must_use]
pub fn flag_short_history(base: &Coverage, others: &[(String, Coverage)]) -> Vec<ShortCoverage> {
    let mut flags = Vec::new();
    for (label, cov) in others {
        let starts_late = match (base.first_ms, cov.first_ms) {
            (Some(b), Some(c)) => c > b,
            _ => false,
        };
        let ends_early = match (base.last_ms, cov.last_ms) {
            (Some(b), Some(c)) => c < b,
            _ => false,
        };
        if starts_late || ends_early {
            flags.push(ShortCoverage {
                series: label.clone(),
                starts_late,
                ends_early,
            });
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn coverage_counts_expected_present_missing() {
        // 0,1,2, [skip 3], 4 → first 0, last 4min, present 4, expected 5, missing 1.
        let cov = coverage(&[0, MIN, 2 * MIN, 4 * MIN], MIN);
        assert_eq!(cov.first_ms, Some(0));
        assert_eq!(cov.last_ms, Some(4 * MIN));
        assert_eq!(cov.present, 4);
        assert_eq!(cov.expected, 5);
        assert_eq!(cov.missing, 1);
    }

    #[test]
    fn empty_series_coverage() {
        let cov = coverage(&[], MIN);
        assert_eq!(cov.first_ms, None);
        assert_eq!(cov.present, 0);
        assert_eq!(cov.missing, 0);
    }

    #[test]
    fn flags_series_that_start_late_or_end_early() {
        let base = coverage(&(0..=10).map(|i| i * MIN).collect::<Vec<_>>(), MIN);
        // Funding starts at 4min (late), ends at 10min (same).
        let funding = coverage(&(4..=10).map(|i| i * MIN).collect::<Vec<_>>(), MIN);
        // Premium ends at 8min (early), starts at 0 (same).
        let premium = coverage(&(0..=8).map(|i| i * MIN).collect::<Vec<_>>(), MIN);

        let flags = flag_short_history(
            &base,
            &[
                ("fundingRate".to_owned(), funding),
                ("premiumIndex".to_owned(), premium),
            ],
        );
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_late && !flags[0].ends_early);
        assert!(flags[1].ends_early && !flags[1].starts_late);
    }

    #[test]
    fn equal_coverage_is_not_flagged() {
        let base = coverage(&(0..=5).map(|i| i * MIN).collect::<Vec<_>>(), MIN);
        let same = coverage(&(0..=5).map(|i| i * MIN).collect::<Vec<_>>(), MIN);
        assert!(flag_short_history(&base, &[("x".to_owned(), same)]).is_empty());
    }
}
