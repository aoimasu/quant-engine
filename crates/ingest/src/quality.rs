//! The per-vintage data-quality report and its hard-violation gate.
//!
//! Aggregates the per-series integrity, coverage, and fill plan, plus the cross-source divergences,
//! into one serialisable artefact written per vintage. [`DataQualityReport::evaluate`] turns
//! configured hard violations into a run-failing error (AC #2).

use serde::Serialize;

use crate::coverage::{Coverage, ShortCoverage};
use crate::fill::FillPlan;
use crate::integrity::SeriesIntegrity;
use crate::reconcile::Divergence;

/// The quality findings for one series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesQuality {
    /// Series label (e.g. `"BTCUSDT/klines/5m"`).
    pub series: String,
    /// Structural integrity (gaps / duplicates / order).
    pub integrity: SeriesIntegrity,
    /// Coverage accounting.
    pub coverage: Coverage,
    /// The leakage-safe fill plan (filled slots + over-bound holes).
    pub fill: FillPlan,
}

/// The full per-vintage data-quality report — the artefact written alongside the vintage.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DataQualityReport {
    /// The vintage this report belongs to.
    pub vintage_id: String,
    /// Per-series findings.
    pub series: Vec<SeriesQuality>,
    /// Series flagged as shorter-history than base.
    pub short_history: Vec<ShortCoverage>,
    /// Cross-source (vendor↔REST) divergences.
    pub divergences: Vec<Divergence>,
}

/// Which findings fail the run (configurable per vintage).
#[derive(Debug, Clone, Copy)]
pub struct HardViolationPolicy {
    /// Any gap wider than this (ms) is a hard violation (a genuine outage, not a small fillable gap).
    pub max_gap_ms: i64,
    /// Whether duplicate timestamps are tolerated.
    pub allow_duplicates: bool,
    /// Whether out-of-order timestamps are tolerated.
    pub allow_out_of_order: bool,
    /// Maximum vendor↔REST divergences tolerated before failing.
    pub max_divergences: usize,
}

impl Default for HardViolationPolicy {
    fn default() -> Self {
        Self {
            max_gap_ms: i64::MAX,
            allow_duplicates: false,
            allow_out_of_order: false,
            max_divergences: usize::MAX,
        }
    }
}

/// A single hard violation (why the run failed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    /// The series it concerns, or `None` for a report-wide violation.
    pub series: Option<String>,
    /// A short machine-ish reason code.
    pub kind: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

impl DataQualityReport {
    /// Serialise the report to pretty JSON — the per-vintage artefact.
    ///
    /// # Errors
    /// [`serde_json::Error`] if serialisation fails (it shouldn't for this `Vec`/scalar shape).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Evaluate the report against `policy`, returning every hard violation. A non-empty result
    /// **fails the run** (AC #2).
    ///
    /// # Errors
    /// `Err(violations)` listing each hard violation; `Ok(())` when the corpus is clean enough.
    pub fn evaluate(&self, policy: &HardViolationPolicy) -> Result<(), Vec<Violation>> {
        let mut v = Vec::new();
        for s in &self.series {
            for gap in &s.integrity.gaps {
                if gap.span_ms() > policy.max_gap_ms {
                    v.push(Violation {
                        series: Some(s.series.clone()),
                        kind: "gap_exceeds_bound",
                        detail: format!(
                            "gap {}..{} ({} missing) spans {}ms > bound {}ms",
                            gap.from_ms,
                            gap.to_ms,
                            gap.missing,
                            gap.span_ms(),
                            policy.max_gap_ms
                        ),
                    });
                }
            }
            if !policy.allow_duplicates && !s.integrity.duplicates.is_empty() {
                v.push(Violation {
                    series: Some(s.series.clone()),
                    kind: "duplicates",
                    detail: format!("{} duplicate timestamps", s.integrity.duplicates.len()),
                });
            }
            if !policy.allow_out_of_order && !s.integrity.out_of_order.is_empty() {
                v.push(Violation {
                    series: Some(s.series.clone()),
                    kind: "out_of_order",
                    detail: format!("{} out-of-order timestamps", s.integrity.out_of_order.len()),
                });
            }
        }
        if self.divergences.len() > policy.max_divergences {
            v.push(Violation {
                series: None,
                kind: "too_many_divergences",
                detail: format!(
                    "{} divergences > max {}",
                    self.divergences.len(),
                    policy.max_divergences
                ),
            });
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::coverage;
    use crate::fill::plan_fill;
    use crate::integrity::check_series;

    const MIN: i64 = 60_000;

    fn series_quality(label: &str, ts: &[i64], max_fill_gap: i64) -> SeriesQuality {
        let (start, end) = (
            ts.first().copied().unwrap_or(0),
            ts.last().copied().unwrap_or(0) + MIN,
        );
        SeriesQuality {
            series: label.to_owned(),
            integrity: check_series(ts, MIN),
            coverage: coverage(ts, MIN),
            fill: plan_fill(ts, MIN, start, end, max_fill_gap),
        }
    }

    #[test]
    fn clean_report_passes_and_round_trips_json() {
        let ts: Vec<i64> = (0..6).map(|i| i * MIN).collect();
        let report = DataQualityReport {
            vintage_id: "abc".to_owned(),
            series: vec![series_quality("BTCUSDT/klines/1m", &ts, 2 * MIN)],
            short_history: vec![],
            divergences: vec![],
        };
        assert!(report.evaluate(&HardViolationPolicy::default()).is_ok());

        // The artefact serialises and round-trips structurally.
        let json = report.to_json().unwrap();
        let back: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(back["vintage_id"], "abc");
        assert!(back["series"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn gap_beyond_bound_is_a_hard_violation() {
        // 0,1, [skip to 6min] → gap span 5min > bound 2min.
        let ts = vec![0, MIN, 6 * MIN];
        let report = DataQualityReport {
            vintage_id: "v".to_owned(),
            series: vec![series_quality("BTCUSDT/klines/1m", &ts, 2 * MIN)],
            ..Default::default()
        };
        let policy = HardViolationPolicy {
            max_gap_ms: 2 * MIN,
            ..Default::default()
        };
        let err = report.evaluate(&policy).unwrap_err();
        assert!(err.iter().any(|v| v.kind == "gap_exceeds_bound"));
    }

    #[test]
    fn duplicates_and_disorder_fail_when_disallowed() {
        let ts = vec![0, MIN, MIN, 0]; // duplicate MIN + out-of-order 0
        let report = DataQualityReport {
            vintage_id: "v".to_owned(),
            series: vec![series_quality("s", &ts, i64::MAX)],
            ..Default::default()
        };
        let err = report
            .evaluate(&HardViolationPolicy::default())
            .unwrap_err();
        assert!(err.iter().any(|v| v.kind == "duplicates"));
        assert!(err.iter().any(|v| v.kind == "out_of_order"));

        // Tolerated when the policy allows them.
        let lenient = HardViolationPolicy {
            allow_duplicates: true,
            allow_out_of_order: true,
            ..Default::default()
        };
        assert!(report.evaluate(&lenient).is_ok());
    }

    #[test]
    fn too_many_divergences_fail() {
        let report = DataQualityReport {
            vintage_id: "v".to_owned(),
            divergences: vec![
                Divergence::MissingIn {
                    ts_ms: 1,
                    absent_from: "rest",
                },
                Divergence::MissingIn {
                    ts_ms: 2,
                    absent_from: "rest",
                },
            ],
            ..Default::default()
        };
        let policy = HardViolationPolicy {
            max_divergences: 1,
            ..Default::default()
        };
        assert!(report.evaluate(&policy).is_err());
        // Raising the budget passes.
        let ok_policy = HardViolationPolicy {
            max_divergences: 5,
            ..Default::default()
        };
        assert!(report.evaluate(&ok_policy).is_ok());
    }
}
