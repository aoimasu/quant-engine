//! Stress / worst-case-loss scenarios (QE-130).
//!
//! QE-115's tail-aware objective optimises the *average* tail (CVaR/CDaR); it does not bound the single
//! worst capital loss. A pre-declared max-loss needs evidence, so this module runs a candidate ensemble
//! (its capacity-capped weights over the selected strategies' return series) through an explicit stress
//! set — historical crash windows + synthetic shocks (gap, funding-spike, ADL) — and reports the
//! **worst-case loss**: the largest peak-to-trough capital loss any single scenario produces. That figure
//! rides the vintage (QE-129) and feeds gate G3 (QE-308).
//!
//! Worst-case loss is the *single worst trough* (max drawdown), distinct from QE-115's CVaR (an average
//! over the tail). Synthetic shocks are modelled as compounding at the existing worst drawdown — the
//! conservative case where the shock lands while the book is already down — which is what a max-loss
//! *bound* should be evidenced against. Live margin enforcement (QE-215) is out of scope.

/// Default adverse price gap (fraction of notional) — a 10% jump.
pub const DEFAULT_GAP_RETURN: f64 = 0.10;
/// Default per-period funding drag during a funding spike (fraction of notional).
pub const DEFAULT_FUNDING_PER_PERIOD: f64 = 0.005;
/// Default number of periods the funding spike persists.
pub const DEFAULT_FUNDING_PERIODS: usize = 8;
/// Default auto-deleveraging haircut (fraction of notional force-closed at a loss).
pub const DEFAULT_ADL_HAIRCUT: f64 = 0.05;

/// A named stress scenario applied to a candidate ensemble's return path.
#[derive(Debug, Clone, PartialEq)]
pub enum StressScenario {
    /// Replay a known historical crash window — the slice `[start, start + len)` of the base path.
    HistoricalWindow {
        /// Identifier for the binding-scenario report.
        name: String,
        /// Start index of the window in the base return path.
        start: usize,
        /// Window length in periods.
        len: usize,
    },
    /// A sudden adverse price jump of `adverse_return` (fraction of notional), scaled by exposure.
    Gap {
        /// Identifier for the binding-scenario report.
        name: String,
        /// Adverse one-period return magnitude (positive = loss) before exposure scaling.
        adverse_return: f64,
    },
    /// A sustained funding-cost drag of `per_period` over `periods`, scaled by exposure.
    FundingSpike {
        /// Identifier for the binding-scenario report.
        name: String,
        /// Per-period funding drag (fraction of notional) before exposure scaling.
        per_period: f64,
        /// Number of periods the spike persists.
        periods: usize,
    },
    /// Auto-deleveraging: the venue force-closes at a `haircut` (fraction of notional) in the crash.
    Adl {
        /// Identifier for the binding-scenario report.
        name: String,
        /// Forced-close haircut (fraction of notional) before exposure scaling.
        haircut: f64,
    },
}

impl StressScenario {
    /// The scenario's identifier.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            StressScenario::HistoricalWindow { name, .. }
            | StressScenario::Gap { name, .. }
            | StressScenario::FundingSpike { name, .. }
            | StressScenario::Adl { name, .. } => name,
        }
    }
}

/// The loss a single scenario produces (a positive fraction of capital, `0.35` = 35%).
#[derive(Debug, Clone, PartialEq)]
pub struct ScenarioLoss {
    /// The scenario's identifier.
    pub name: String,
    /// Worst-case capital loss under this scenario.
    pub loss: f64,
}

/// The worst-case-loss report for a candidate ensemble over a stress set (QE-130/D4).
#[derive(Debug, Clone, PartialEq)]
pub struct StressReport {
    /// The largest loss across all scenarios — the vintage's worst-case-loss figure.
    pub worst_case_loss: f64,
    /// The scenario that produced [`worst_case_loss`](StressReport::worst_case_loss).
    pub binding_scenario: String,
    /// The per-scenario loss breakdown (for G3/QE-308 audit).
    pub per_scenario: Vec<ScenarioLoss>,
}

/// The default synthetic shock set (gap + funding-spike + ADL) at the documented default magnitudes.
/// Historical windows are caller-supplied (they encode calendar knowledge the engine does not have).
#[must_use]
pub fn default_synthetic_shocks() -> Vec<StressScenario> {
    vec![
        StressScenario::Gap {
            name: "gap".to_string(),
            adverse_return: DEFAULT_GAP_RETURN,
        },
        StressScenario::FundingSpike {
            name: "funding-spike".to_string(),
            per_period: DEFAULT_FUNDING_PER_PERIOD,
            periods: DEFAULT_FUNDING_PERIODS,
        },
        StressScenario::Adl {
            name: "adl".to_string(),
            haircut: DEFAULT_ADL_HAIRCUT,
        },
    ]
}

/// The worst peak-to-trough capital loss of an equity curve built from `returns`, as a **positive**
/// fraction (`0.0` for a monotonically non-decreasing curve). Distinct from CVaR — this is the single
/// worst trough, not the tail average.
#[must_use]
pub fn max_drawdown(returns: &[f64]) -> f64 {
    let (mut equity, mut peak, mut worst) = (1.0_f64, 1.0_f64, 0.0_f64);
    for &r in returns {
        equity *= 1.0 + r;
        peak = peak.max(equity);
        let dd = if peak > 0.0 { 1.0 - equity / peak } else { 1.0 };
        worst = worst.max(dd);
    }
    worst
}

/// The candidate ensemble's actual per-period return path: the `series` (selected strategies' returns)
/// combined by their `weights` (capacity-capped, QE-128), truncated to the shortest member series.
/// Empty/zero-length ⇒ empty.
#[must_use]
pub fn weighted_combined(series: &[Vec<f64>], weights: &[f64]) -> Vec<f64> {
    let n = series.len().min(weights.len());
    if n == 0 {
        return Vec::new();
    }
    let len = (0..n).map(|i| series[i].len()).min().unwrap_or(0);
    let mut out = vec![0.0; len];
    for i in 0..n {
        for (slot, v) in out.iter_mut().zip(series[i].iter()) {
            *slot += weights[i] * v;
        }
    }
    out
}

/// Compound an extra instantaneous loss `e` onto a base drawdown `d0` *at the trough*:
/// `1 − (1 − d0)·(1 − e)` — the conservative worst case where the shock lands while already down.
fn compound(d0: f64, e: f64) -> f64 {
    1.0 - (1.0 - d0) * (1.0 - e)
}

/// The worst-case loss a single `scenario` inflicts on a candidate ensemble whose base return path is
/// `base` and whose gross exposure is `gross = Σ|weights|` (synthetic shocks scale with exposure).
#[must_use]
pub fn scenario_loss(base: &[f64], gross: f64, scenario: &StressScenario) -> f64 {
    let d0 = max_drawdown(base);
    match scenario {
        StressScenario::HistoricalWindow { start, len, .. } => {
            let hi = start.saturating_add(*len).min(base.len());
            let window = base.get((*start).min(base.len())..hi).unwrap_or(&[]);
            max_drawdown(window)
        }
        StressScenario::Gap { adverse_return, .. } => compound(d0, adverse_return.max(0.0) * gross),
        StressScenario::FundingSpike {
            per_period,
            periods,
            ..
        } => compound(d0, per_period.max(0.0) * *periods as f64 * gross),
        StressScenario::Adl { haircut, .. } => compound(d0, haircut.max(0.0) * gross),
    }
}

/// Run a candidate ensemble (`series` = selected strategies' return paths, `weights` = their
/// capacity-capped weights) through the `scenarios` and report the worst-case loss (QE-130/D4): the
/// maximum scenario loss, the binding scenario, and the full per-scenario breakdown.
///
/// An empty scenario set ⇒ a `0.0` figure with an empty breakdown.
#[must_use]
pub fn worst_case_loss(
    series: &[Vec<f64>],
    weights: &[f64],
    scenarios: &[StressScenario],
) -> StressReport {
    let base = weighted_combined(series, weights);
    let n = series.len().min(weights.len());
    let gross: f64 = weights[..n].iter().map(|w| w.abs()).sum();

    let per_scenario: Vec<ScenarioLoss> = scenarios
        .iter()
        .map(|s| ScenarioLoss {
            name: s.name().to_string(),
            loss: scenario_loss(&base, gross, s),
        })
        .collect();

    let (worst_case_loss, binding_scenario) = per_scenario
        .iter()
        .max_by(|a, b| a.loss.total_cmp(&b.loss))
        .map(|s| (s.loss, s.name.clone()))
        .unwrap_or((0.0, String::new()));

    StressReport {
        worst_case_loss,
        binding_scenario,
        per_scenario,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    #[test]
    fn max_drawdown_finds_worst_trough_and_is_zero_when_rising() {
        // Up 10%, down to a trough, partial recovery: peak 1.1, trough 1.1·0.7 = 0.77 ⇒ dd 0.3.
        approx(max_drawdown(&[0.1, -0.3, 0.05]), 0.3);
        // Monotonically non-decreasing ⇒ no drawdown.
        approx(max_drawdown(&[0.0, 0.1, 0.2]), 0.0);
        approx(max_drawdown(&[]), 0.0);
    }

    #[test]
    fn weighted_combined_uses_weights_not_equal_weight() {
        let series = vec![vec![0.10, -0.10], vec![0.00, 0.00]];
        // 70/30 weighting: [0.7·0.1, 0.7·(−0.1)] = [0.07, −0.07].
        let combined = weighted_combined(&series, &[0.7, 0.3]);
        approx(combined[0], 0.07);
        approx(combined[1], -0.07);
        // Skewed weights differ from the equal-weight (0.5/0.5 ⇒ 0.05) path.
        assert!((combined[0] - 0.05).abs() > 1e-6);
    }

    #[test]
    fn synthetic_shocks_compound_at_the_trough_and_are_monotone() {
        // Base path: a 0.2 drawdown (down 20%, flat after).
        let base = vec![-0.2, 0.0];
        let d0 = max_drawdown(&base);
        approx(d0, 0.2);
        let gross = 1.0;

        // Gap of 0.1 at gross 1.0 compounds: 1 − 0.8·0.9 = 0.28.
        let gap = StressScenario::Gap {
            name: "g".into(),
            adverse_return: 0.1,
        };
        approx(scenario_loss(&base, gross, &gap), 0.28);

        // A bigger gap ⇒ a strictly bigger loss.
        let big_gap = StressScenario::Gap {
            name: "g2".into(),
            adverse_return: 0.2,
        };
        assert!(scenario_loss(&base, gross, &big_gap) > scenario_loss(&base, gross, &gap));

        // Funding spike: 0.005 · 8 · 1.0 = 0.04 ⇒ 1 − 0.8·0.96 = 0.232.
        let funding = StressScenario::FundingSpike {
            name: "f".into(),
            per_period: 0.005,
            periods: 8,
        };
        approx(scenario_loss(&base, gross, &funding), 0.232);

        // ADL haircut 0.05 ⇒ 1 − 0.8·0.95 = 0.24.
        let adl = StressScenario::Adl {
            name: "a".into(),
            haircut: 0.05,
        };
        approx(scenario_loss(&base, gross, &adl), 0.24);
    }

    #[test]
    fn historical_window_returns_its_window_drawdown() {
        // A crash only inside [1, 3): the window's own drawdown, not the whole path's.
        let base = vec![0.0, 0.1, -0.5, 0.0];
        let scenario = StressScenario::HistoricalWindow {
            name: "crash".into(),
            start: 1,
            len: 2,
        };
        // Window = [0.1, −0.5]: peak 1.1, trough 0.55 ⇒ dd 0.5.
        approx(scenario_loss(&base, 1.0, &scenario), 0.5);
    }

    #[test]
    fn worst_case_loss_is_the_max_and_names_the_binding_scenario() {
        // Two strategies; a real 0.2 base drawdown driven by member 0.
        let series = vec![vec![0.05, -0.2, 0.0], vec![0.0, 0.0, 0.0]];
        let weights = vec![1.0, 0.0]; // gross 1.0, base path = member 0
        let mut scenarios = default_synthetic_shocks();
        scenarios.push(StressScenario::HistoricalWindow {
            name: "covid".into(),
            start: 1,
            len: 1,
        });

        let report = worst_case_loss(&series, &weights, &scenarios);
        // All scenarios reported.
        assert_eq!(report.per_scenario.len(), scenarios.len());
        // The figure is the max over the breakdown…
        let max = report
            .per_scenario
            .iter()
            .map(|s| s.loss)
            .fold(0.0_f64, f64::max);
        approx(report.worst_case_loss, max);
        // …and the gap (largest default shock, compounding on the 0.2 base) binds over funding/ADL.
        assert_eq!(report.binding_scenario, "gap");
        assert!(report.worst_case_loss > 0.2);
    }

    #[test]
    fn empty_scenarios_yield_zero() {
        let series = vec![vec![-0.5, 0.0]];
        let report = worst_case_loss(&series, &[1.0], &[]);
        approx(report.worst_case_loss, 0.0);
        assert!(report.per_scenario.is_empty());
    }
}
