//! qe-ensemble — ensemble construction (discrete differential evolution).
//!
//! - [`objective`] (QE-115) — the tail-aware, correlation-penalised portfolio objective: CVaR/CDaR on
//!   the combined net-of-cost returns (with a synthetic stress overlay), a positive-mean-pairwise
//!   return-correlation penalty, and a leave-one-out wide-basin floor.
//! - [`de`] (QE-115) — discrete differential-evolution operators over a binary ensemble mask.
//! - [`search`] (QE-126) — the discrete-DE search loop: DE/rand/1/bin over the ensemble mask with
//!   fold-cross-validated, leave-one-out scoring that converges on a robust basin, net-of-cost.
//! - [`regime`] (QE-127) — the correlation penalty + per-regime expectancy constraint: a regime-aware
//!   objective/search that rejects ensembles which are not net-positive in every labelled regime (QE-125).
//! - [`capacity`] (QE-128) — capacity analysis: estimate per-strategy capacity from an impact model ×
//!   turnover × target AUM and cap ensemble weights so none exceeds its modelled capacity at size.
//! - [`stress`] (QE-130) — stress / worst-case-loss: run a candidate ensemble through historical crash
//!   windows + synthetic shocks (gap, funding-spike, ADL) and report the worst peak-to-trough capital
//!   loss that rides the vintage and feeds gate G3 (QE-308).
//!
//! Deliberately independent of `qe-wfo`: the search ⟂ portfolio firewall (QE-001/QE-132).

pub mod capacity;
pub mod de;
pub mod objective;
pub mod regime;
pub mod search;
pub mod stress;

pub use capacity::{cap_weights, capacity, CapacityModel, StrategyProfile, DEFAULT_EDGE_RETENTION};
pub use de::{binomial_crossover, de_mutant, EnsembleMask, DEFAULT_CR};
pub use objective::{
    cdar, combined_returns, combined_returns_weighted, cvar, leave_one_out_min,
    leave_one_out_min_weighted, min_significant_r, objective, objective_weighted,
    pairwise_corr_penalty, pearson, positive_mean_pairwise_corr, stress_overlay, CorrDeflation,
    CorrPenalty, ObjectiveConfig, TailRisk, Weighting, DEFAULT_ALPHA, DEFAULT_FISHER_LAMBDA,
    DEFAULT_SIGNIFICANCE_Z,
};
pub use regime::{
    leave_one_out_min_regime, per_regime_expectancy, regime_aware_cv_score, regime_aware_objective,
    search_portfolio_regime_aware, worst_regime_expectancy, RegimeAwareConfig,
    DEFAULT_REGIME_FLOOR, DEFAULT_REGIME_WEIGHT,
};
pub use search::{
    cross_val_score, cross_val_score_weighted, search_portfolio, search_portfolio_weighted,
    SearchConfig, SearchResult, DEFAULT_FOLDS, DEFAULT_GENERATIONS, DEFAULT_INIT_DENSITY,
    DEFAULT_POP_SIZE,
};
pub use stress::{
    default_synthetic_shocks, max_drawdown, scenario_loss, weighted_combined, worst_case_loss,
    ScenarioLoss, StressReport, StressScenario, DEFAULT_ADL_HAIRCUT, DEFAULT_FUNDING_PERIODS,
    DEFAULT_FUNDING_PER_PERIOD, DEFAULT_GAP_RETURN,
};

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-ensemble"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-ensemble");
    }
}
