//! qe-report (QE-133) — the per-vintage validation evidence pack.
//!
//! A single human-readable report that bundles everything the G1 decision (QE-134) needs about a vintage:
//! net-of-cost performance, a cost-sensitivity (1×/2×) sweep, the DSR/PBO/SPA robustness diagnostics
//! (QE-131), per-regime expectancy (QE-125), the pairwise return-correlation distribution (QE-115),
//! capacity at target AUM (QE-128), and worst-case loss (QE-130). It *aggregates and renders* finished
//! artefacts — it does not recompute them — so it is the most-downstream consumer and is unconstrained by
//! the search⟂portfolio⟂live firewall (QE-132). Reproducibility is two guarantees: the report is `serde`
//! (round-trips) and [`VintageReport::render_markdown`] is a pure function of the inputs.
//!
//! An interactive viewer is out of scope (QE-136); this is the text pack + its machine-readable form.

use std::fmt::Write as _;

use qe_validation::RobustnessReport;
use serde::{Deserialize, Serialize};

/// Net-of-cost performance of the candidate ensemble.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerformanceSummary {
    /// Total compounded return over the evaluation window.
    pub total_return: f64,
    /// Mean per-period return.
    pub mean_return: f64,
    /// Per-period Sharpe ratio.
    pub sharpe: f64,
    /// Worst peak-to-trough drawdown (a positive fraction).
    pub max_drawdown: f64,
    /// Number of return periods.
    pub n_periods: usize,
}

/// One row of the cost-sensitivity sweep: performance at a given multiple of the modelled costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostScenario {
    /// The cost multiple applied (e.g. `1.0`, `2.0`).
    pub cost_multiple: f64,
    /// Per-period Sharpe at this cost level.
    pub sharpe: f64,
    /// Total compounded return at this cost level.
    pub total_return: f64,
}

/// One row of the per-regime expectancy table (QE-125).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegimeRow {
    /// The regime label (e.g. `"Calm/Trending"`).
    pub regime: String,
    /// Number of periods in this regime.
    pub count: usize,
    /// Mean per-period return in this regime.
    pub mean_return: f64,
    /// Fraction of positive-return periods in this regime.
    pub win_rate: f64,
}

/// Summary of the pairwise return-correlation distribution across ensemble members (QE-115).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrelationSummary {
    /// Minimum pairwise correlation.
    pub min: f64,
    /// Median pairwise correlation.
    pub median: f64,
    /// Maximum pairwise correlation.
    pub max: f64,
    /// Mean pairwise correlation.
    pub mean: f64,
    /// Number of distinct pairs summarised.
    pub n_pairs: usize,
}

/// Capacity at the target AUM (QE-128).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacitySummary {
    /// The target assets-under-management the capacity is evaluated at.
    pub target_aum: f64,
    /// The binding (smallest) modelled per-strategy capacity, in dollars.
    pub capacity: f64,
    /// The resulting capacity-implied weight cap (`capacity / target_aum`, clamped to `[0,1]`).
    pub weight_cap: f64,
}

/// The per-vintage validation evidence pack (QE-133).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VintageReport {
    /// The vintage this report describes (QE-129 `vintage_id`).
    pub vintage_id: String,
    /// The vintage's content hash, pinning the report to a specific artefact.
    pub content_hash: String,
    /// Net-of-cost performance.
    pub performance: PerformanceSummary,
    /// Cost-sensitivity sweep. **Caller contract:** must include at least the `1.0×` and `2.0×` rows
    /// (QE-133's required sweep); an empty vector renders a header-only table.
    pub cost_sensitivity: Vec<CostScenario>,
    /// DSR / PBO / SPA robustness diagnostics (QE-131).
    pub robustness: RobustnessReport,
    /// Per-regime expectancy (QE-125).
    pub regime_expectancy: Vec<RegimeRow>,
    /// Pairwise return-correlation distribution (QE-115).
    pub correlation: CorrelationSummary,
    /// Capacity at target AUM (QE-128).
    pub capacity: CapacitySummary,
    /// Worst-case capital loss under the stress set (QE-130), a positive fraction.
    pub worst_case_loss: f64,
    /// The stress scenario that produced the worst-case loss.
    pub binding_scenario: String,
}

impl VintageReport {
    /// Render the human-readable markdown evidence pack. A **pure function** of `self` — fixed section
    /// order and numeric formatting, no clock/RNG/map iteration — so two renders are byte-identical
    /// (reproducibility, QE-133/D3).
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Vintage validation report — {}", self.vintage_id);
        let _ = writeln!(s, "\n_Content hash: `{}`_", self.content_hash);

        let p = &self.performance;
        let _ = writeln!(s, "\n## Net-of-cost performance");
        let _ = writeln!(s, "- Total return: {:.4}", p.total_return);
        let _ = writeln!(s, "- Mean per-period return: {:.6}", p.mean_return);
        let _ = writeln!(s, "- Sharpe: {:.4}", p.sharpe);
        let _ = writeln!(s, "- Max drawdown: {:.4}", p.max_drawdown);
        let _ = writeln!(s, "- Periods: {}", p.n_periods);

        let _ = writeln!(s, "\n## Cost sensitivity");
        let _ = writeln!(s, "| Cost × | Sharpe | Total return |");
        let _ = writeln!(s, "|---|---|---|");
        for c in &self.cost_sensitivity {
            let _ = writeln!(
                s,
                "| {:.2} | {:.4} | {:.4} |",
                c.cost_multiple, c.sharpe, c.total_return
            );
        }

        let r = &self.robustness;
        let _ = writeln!(s, "\n## Robustness (DSR / PBO / SPA)");
        let _ = writeln!(s, "- Observed Sharpe: {:.4}", r.observed_sharpe);
        let _ = writeln!(s, "- Deflated Sharpe Ratio (DSR): {:.4}", r.dsr);
        let _ = writeln!(
            s,
            "- Probability of Backtest Overfitting (PBO): {:.4}",
            r.pbo
        );
        let _ = writeln!(s, "- Reality Check / SPA p-value: {:.4}", r.spa_pvalue);
        let _ = writeln!(s, "- Effective trials: {}", r.n_trials);

        let _ = writeln!(s, "\n## Per-regime expectancy");
        let _ = writeln!(s, "| Regime | Count | Mean return | Win rate |");
        let _ = writeln!(s, "|---|---|---|---|");
        for row in &self.regime_expectancy {
            let _ = writeln!(
                s,
                "| {} | {} | {:.6} | {:.4} |",
                row.regime, row.count, row.mean_return, row.win_rate
            );
        }

        let c = &self.correlation;
        let _ = writeln!(s, "\n## Pairwise return-correlation distribution");
        let _ = writeln!(
            s,
            "- min {:.4} · median {:.4} · max {:.4} · mean {:.4} (over {} pairs)",
            c.min, c.median, c.max, c.mean, c.n_pairs
        );

        let cap = &self.capacity;
        let _ = writeln!(s, "\n## Capacity at target AUM");
        let _ = writeln!(s, "- Target AUM: {:.2}", cap.target_aum);
        let _ = writeln!(s, "- Binding capacity: {:.2}", cap.capacity);
        let _ = writeln!(s, "- Capacity-implied weight cap: {:.4}", cap.weight_cap);

        let _ = writeln!(s, "\n## Worst-case loss");
        let _ = writeln!(
            s,
            "- {:.4} (binding scenario: {})",
            self.worst_case_loss, self.binding_scenario
        );

        s
    }
}

/// Summarise the pairwise return-correlation distribution of `series` (one per ensemble member): the min,
/// median, max and mean of all `C(n,2)` pairwise Pearson correlations (QE-133/D-correlation). Fewer than
/// two series ⇒ a zeroed summary. Pearson is computed locally (a few lines) to keep the report dep-light.
#[must_use]
pub fn pairwise_correlation_summary(series: &[Vec<f64>]) -> CorrelationSummary {
    let n = series.len();
    let mut corrs = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            corrs.push(pearson(&series[i], &series[j]));
        }
    }
    if corrs.is_empty() {
        return CorrelationSummary {
            min: 0.0,
            median: 0.0,
            max: 0.0,
            mean: 0.0,
            n_pairs: 0,
        };
    }
    let mean = corrs.iter().sum::<f64>() / corrs.len() as f64;
    let mut sorted = corrs.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    CorrelationSummary {
        min: sorted[0],
        median,
        max: sorted[sorted.len() - 1],
        mean,
        n_pairs: corrs.len(),
    }
}

/// Pearson correlation of two equal-length-truncated series (`0.0` if either has no dispersion).
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().sum::<f64>() / n as f64;
    let mb = b.iter().sum::<f64>() / n as f64;
    let mut cov = 0.0;
    let mut va = 0.0;
    let mut vb = 0.0;
    for k in 0..n {
        let (da, db) = (a[k] - ma, b[k] - mb);
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    if va <= 0.0 || vb <= 0.0 {
        return 0.0;
    }
    cov / (va.sqrt() * vb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    fn sample_report() -> VintageReport {
        VintageReport {
            vintage_id: "2024-06-vintage".to_string(),
            content_hash: "abc123".to_string(),
            performance: PerformanceSummary {
                total_return: 0.42,
                mean_return: 0.0011,
                sharpe: 1.85,
                max_drawdown: 0.18,
                n_periods: 500,
            },
            cost_sensitivity: vec![
                CostScenario {
                    cost_multiple: 1.0,
                    sharpe: 1.85,
                    total_return: 0.42,
                },
                CostScenario {
                    cost_multiple: 2.0,
                    sharpe: 1.10,
                    total_return: 0.21,
                },
            ],
            robustness: RobustnessReport {
                observed_sharpe: 1.85,
                dsr: 0.91,
                pbo: 0.12,
                spa_pvalue: 0.03,
                n_trials: 7680,
                trial_variance: 0.03,
                variance_trials: 240,
            },
            regime_expectancy: vec![
                RegimeRow {
                    regime: "Calm/Trending".to_string(),
                    count: 220,
                    mean_return: 0.0018,
                    win_rate: 0.58,
                },
                RegimeRow {
                    regime: "Volatile/Choppy".to_string(),
                    count: 130,
                    mean_return: 0.0004,
                    win_rate: 0.51,
                },
            ],
            correlation: CorrelationSummary {
                min: -0.20,
                median: 0.05,
                max: 0.30,
                mean: 0.04,
                n_pairs: 10,
            },
            capacity: CapacitySummary {
                target_aum: 1_000_000.0,
                capacity: 100_000.0,
                weight_cap: 0.10,
            },
            worst_case_loss: 0.28,
            binding_scenario: "gap".to_string(),
        }
    }

    #[test]
    fn report_contains_every_required_section() {
        let md = sample_report().render_markdown();
        for needle in [
            "Net-of-cost performance",
            "## Cost sensitivity",
            "| 2.00 |", // the 2× cost row of the sweep
            "Deflated Sharpe Ratio (DSR)",
            "Probability of Backtest Overfitting (PBO)",
            "Reality Check / SPA p-value",
            "Per-regime expectancy",
            "Calm/Trending",
            "Pairwise return-correlation distribution",
            "Capacity at target AUM",
            "Worst-case loss",
            "binding scenario: gap",
        ] {
            assert!(
                md.contains(needle),
                "report missing section/figure: {needle:?}\n{md}"
            );
        }
    }

    #[test]
    fn report_is_reproducible() {
        let r = sample_report();
        // Rendering is a pure function: two renders are byte-identical.
        assert_eq!(r.render_markdown(), r.render_markdown());
        // And the report round-trips through serde.
        let json = serde_json::to_string(&r).unwrap();
        let back: VintageReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn pairwise_correlation_summary_matches_hand_computation() {
        // s0 and s1 are perfectly correlated (+1); s2 is the exact negation of s0 (−1).
        let s0 = vec![1.0, 2.0, 3.0, 4.0];
        let s1 = vec![2.0, 4.0, 6.0, 8.0]; // 2·s0 ⇒ corr +1
        let s2 = vec![-1.0, -2.0, -3.0, -4.0]; // −s0 ⇒ corr −1
        let c = pairwise_correlation_summary(&[s0, s1, s2]);
        assert_eq!(c.n_pairs, 3); // C(3,2)
        approx(c.max, 1.0); // s0~s1
        approx(c.min, -1.0); // s0~s2 and s1~s2
                             // pairs: (s0,s1)=+1, (s0,s2)=−1, (s1,s2)=−1 ⇒ mean = −1/3, median (sorted [−1,−1,1]) = −1.
        approx(c.mean, -1.0 / 3.0);
        approx(c.median, -1.0);

        // Fewer than two series ⇒ zeroed summary.
        let z = pairwise_correlation_summary(&[vec![1.0, 2.0]]);
        assert_eq!(z.n_pairs, 0);
        approx(z.mean, 0.0);
    }
}
