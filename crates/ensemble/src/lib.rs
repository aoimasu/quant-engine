//! qe-ensemble — ensemble construction (discrete differential evolution).
//!
//! - [`objective`] (QE-115) — the tail-aware, correlation-penalised portfolio objective: CVaR/CDaR on
//!   the combined net-of-cost returns (with a synthetic stress overlay), a positive-mean-pairwise
//!   return-correlation penalty, and a leave-one-out wide-basin floor.
//! - [`de`] (QE-115) — discrete differential-evolution operators over a binary ensemble mask.
//!
//! Deliberately independent of `qe-wfo`: the search ⟂ portfolio firewall (QE-001/QE-132).

pub mod de;
pub mod objective;

pub use de::{binomial_crossover, de_mutant, EnsembleMask, DEFAULT_CR};
pub use objective::{
    cdar, combined_returns, cvar, leave_one_out_min, objective, pearson,
    positive_mean_pairwise_corr, stress_overlay, ObjectiveConfig, TailRisk, DEFAULT_ALPHA,
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
