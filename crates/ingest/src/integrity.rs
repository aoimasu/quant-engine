//! Per-series structural integrity checks: gaps, duplicates, and monotonicity.
//!
//! Works on the bare timestamp sequence (value-agnostic) so it covers klines, funding, premium and
//! `/futures/data` series uniformly.

use serde::Serialize;

/// A run of missing slots in a fixed-interval series: the data jumps from `from_ms` to `to_ms`,
/// skipping `missing` expected slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Gap {
    /// Last present timestamp before the gap.
    pub from_ms: i64,
    /// First present timestamp after the gap.
    pub to_ms: i64,
    /// Number of absent slots between them.
    pub missing: u64,
}

impl Gap {
    /// The width of the gap in ms (`to_ms - from_ms`).
    #[must_use]
    pub fn span_ms(self) -> i64 {
        self.to_ms - self.from_ms
    }
}

/// The integrity findings for one fixed-interval series.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeriesIntegrity {
    /// Gaps (consecutive Δ greater than one interval).
    pub gaps: Vec<Gap>,
    /// Timestamps that appear more than once.
    pub duplicates: Vec<i64>,
    /// Timestamps that are strictly less than their predecessor (sequence not sorted).
    pub out_of_order: Vec<i64>,
}

impl SeriesIntegrity {
    /// Whether the series is strictly increasing (no duplicates, no inversions).
    #[must_use]
    pub fn is_monotonic(&self) -> bool {
        self.duplicates.is_empty() && self.out_of_order.is_empty()
    }

    /// Whether nothing at all was flagged.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.gaps.is_empty() && self.is_monotonic()
    }
}

/// Check a fixed-interval series given its raw `timestamps` (in arrival order) and the expected
/// `interval_ms` between consecutive slots.
///
/// Gaps and duplicates are detected on the **sorted, deduplicated** view (so an out-of-order row
/// does not masquerade as a gap), while `out_of_order`/`duplicates` are detected on the raw order.
#[must_use]
pub fn check_series(timestamps: &[i64], interval_ms: i64) -> SeriesIntegrity {
    let mut report = SeriesIntegrity::default();
    if timestamps.is_empty() {
        return report;
    }

    // Raw-order checks: inversions and duplicates.
    let mut seen = std::collections::BTreeSet::new();
    for w in timestamps.windows(2) {
        if w[1] < w[0] {
            report.out_of_order.push(w[1]);
        }
    }
    for &t in timestamps {
        if !seen.insert(t) {
            report.duplicates.push(t);
        }
    }

    // Gap detection on the ordered, unique view.
    let ordered: Vec<i64> = seen.into_iter().collect();
    if interval_ms > 0 {
        for w in ordered.windows(2) {
            let delta = w[1] - w[0];
            if delta > interval_ms {
                // Number of absent slots between the two present ones.
                let missing = (delta / interval_ms - 1).max(0) as u64;
                report.gaps.push(Gap {
                    from_ms: w[0],
                    to_ms: w[1],
                    missing,
                });
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn clean_series_has_no_findings() {
        let ts: Vec<i64> = (0..5).map(|i| i * MIN).collect();
        let r = check_series(&ts, MIN);
        assert!(r.is_clean());
        assert!(r.is_monotonic());
    }

    #[test]
    fn detects_gap_with_correct_missing_count() {
        // 0,1,2, [skip 3,4], 5 → one gap from 2min to 5min, 2 missing.
        let ts = vec![0, MIN, 2 * MIN, 5 * MIN];
        let r = check_series(&ts, MIN);
        assert_eq!(r.gaps.len(), 1);
        assert_eq!(
            r.gaps[0],
            Gap {
                from_ms: 2 * MIN,
                to_ms: 5 * MIN,
                missing: 2
            }
        );
        assert_eq!(r.gaps[0].span_ms(), 3 * MIN);
    }

    #[test]
    fn detects_duplicates() {
        let ts = vec![0, MIN, MIN, 2 * MIN];
        let r = check_series(&ts, MIN);
        assert_eq!(r.duplicates, vec![MIN]);
        assert!(!r.is_monotonic());
        // The duplicate does not create a phantom gap.
        assert!(r.gaps.is_empty());
    }

    #[test]
    fn detects_out_of_order() {
        let ts = vec![0, 2 * MIN, MIN, 3 * MIN];
        let r = check_series(&ts, MIN);
        assert_eq!(r.out_of_order, vec![MIN]);
        assert!(!r.is_monotonic());
    }

    #[test]
    fn empty_series_is_clean() {
        assert!(check_series(&[], MIN).is_clean());
    }
}
