//! Vendor↔REST overlap diffing with tolerance.
//!
//! QE-102 retains the overlap region where vendor dumps and venue REST cover the same timestamps.
//! Comparing them catches silent vendor revisions / our own ingestion bugs before fusion. Tolerance
//! diffing is diagnostic, so `f64` is used (no exact-decimal requirement here).

use std::collections::BTreeMap;

use serde::Serialize;

/// Comparison tolerance: values diverge only if they differ by more than **both** an absolute and a
/// relative bound (so tiny values aren't flagged by relative noise, nor large ones by abs noise).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    /// Absolute tolerance.
    pub abs: f64,
    /// Relative tolerance (fraction of `max(|a|, |b|)`).
    pub rel: f64,
}

impl Tolerance {
    /// Whether `a` and `b` are within tolerance.
    #[must_use]
    pub fn within(self, a: f64, b: f64) -> bool {
        let diff = (a - b).abs();
        diff <= self.abs || diff <= self.rel * a.abs().max(b.abs())
    }
}

/// One reconciliation finding at a timestamp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Divergence {
    /// Both sources have the timestamp but the values differ beyond tolerance.
    Value {
        /// The timestamp.
        ts_ms: i64,
        /// Vendor value.
        vendor: f64,
        /// REST value.
        rest: f64,
    },
    /// The timestamp is present in only one source.
    MissingIn {
        /// The timestamp.
        ts_ms: i64,
        /// The source that lacks it (`"vendor"` or `"rest"`).
        absent_from: &'static str,
    },
}

impl Divergence {
    /// The timestamp this finding concerns.
    #[must_use]
    pub fn ts_ms(&self) -> i64 {
        match self {
            Divergence::Value { ts_ms, .. } | Divergence::MissingIn { ts_ms, .. } => *ts_ms,
        }
    }
}

/// Diff the `vendor` and `rest` `(timestamp, value)` series over their union, reporting value
/// divergences beyond `tol` and timestamps present in only one source. Findings are ascending by
/// timestamp.
#[must_use]
pub fn diff_overlap(vendor: &[(i64, f64)], rest: &[(i64, f64)], tol: Tolerance) -> Vec<Divergence> {
    let v: BTreeMap<i64, f64> = vendor.iter().copied().collect();
    let r: BTreeMap<i64, f64> = rest.iter().copied().collect();
    let mut out = Vec::new();
    let all_ts: std::collections::BTreeSet<i64> = v.keys().chain(r.keys()).copied().collect();
    for ts in all_ts {
        match (v.get(&ts), r.get(&ts)) {
            (Some(&a), Some(&b)) => {
                if !tol.within(a, b) {
                    out.push(Divergence::Value {
                        ts_ms: ts,
                        vendor: a,
                        rest: b,
                    });
                }
            }
            (Some(_), None) => out.push(Divergence::MissingIn {
                ts_ms: ts,
                absent_from: "rest",
            }),
            (None, Some(_)) => out.push(Divergence::MissingIn {
                ts_ms: ts,
                absent_from: "vendor",
            }),
            (None, None) => unreachable!("ts came from the union"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: Tolerance = Tolerance {
        abs: 0.01,
        rel: 1e-4,
    };

    #[test]
    fn equal_within_tolerance_has_no_divergence() {
        let vendor = [(1, 42_000.0), (2, 42_010.0)];
        let rest = [(1, 42_000.005), (2, 42_010.0)]; // within abs 0.01
        assert!(diff_overlap(&vendor, &rest, TOL).is_empty());
    }

    #[test]
    fn value_breach_is_reported() {
        let vendor = [(1, 42_000.0)];
        let rest = [(1, 42_100.0)]; // 100 off — beyond both abs and rel
        let d = diff_overlap(&vendor, &rest, TOL);
        assert_eq!(d.len(), 1);
        assert!(matches!(
            d[0],
            Divergence::Value {
                ts_ms: 1,
                vendor: v,
                rest: r
            } if v == 42_000.0 && r == 42_100.0
        ));
    }

    #[test]
    fn missing_in_either_source_is_reported() {
        let vendor = [(1, 1.0), (2, 2.0)];
        let rest = [(2, 2.0), (3, 3.0)];
        let d = diff_overlap(&vendor, &rest, TOL);
        // ts 1 missing in rest; ts 3 missing in vendor.
        assert_eq!(d.len(), 2);
        assert_eq!(
            d[0],
            Divergence::MissingIn {
                ts_ms: 1,
                absent_from: "rest"
            }
        );
        assert_eq!(
            d[1],
            Divergence::MissingIn {
                ts_ms: 3,
                absent_from: "vendor"
            }
        );
    }

    #[test]
    fn relative_tolerance_scales_with_magnitude() {
        // rel 1e-4 of 1_000_000 = 100 → a 50 diff is within tolerance.
        let big = Tolerance {
            abs: 0.0,
            rel: 1e-4,
        };
        assert!(big.within(1_000_000.0, 1_000_050.0));
        assert!(!big.within(1_000_000.0, 1_000_200.0));
    }
}
