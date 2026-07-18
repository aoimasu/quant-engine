//! qe-validation (QE-131) — the statistical robustness suite.
//!
//! A large quality-diversity archive is a multiple-testing machine: an undeflated out-of-sample Sharpe is
//! the *expected* output of the search, not evidence of edge. This crate computes the published
//! data-snooping diagnostics for a vintage and bundles them for gate G1 (QE-134):
//!
//! - [`dsr`] — the **Deflated Sharpe Ratio** with effective trials = archive cells × generations ×
//!   windows.
//! - [`pbo`] — **Probability of Backtest Overfitting** via Combinatorially Symmetric Cross-Validation.
//! - [`spa`] — **White's Reality Check / SPA-lower** vs a best-of-N null (recentres all `k` models;
//!   the conservative variant — it omits Hansen's power-recovering model-omission recentring, QE-448).
//! - [`nulls`] — **BTC-HODL** and **turnover-matched random-entry** benchmark nulls.
//! - [`ic`] — **per-indicator rank-IC / information-horizon screening** (QE-434): a catalogue-admission
//!   pre-filter that classifies each factor Admit/Flag/Drop out-of-fold, filtering **compute** the
//!   search roams over — never the hypothesis/trial count the DSR deflates against.
//!
//! It is downstream validation: pure statistics over return matrices + trial counts. It depends only on
//! `qe-determinism` (reproducible bootstrap/null RNG, QE-006) — **no `qe-wfo`/`qe-ensemble`**, so it never
//! touches the search⟂portfolio firewall.
//!
//! # What deflation corrects for — and what it cannot see (QE-448)
//!
//! DSR / PSR / PBO / SPA all correct for **SELECTION**: the multiple-testing / best-of-N inflation from
//! searching a large quality-diversity archive. They are computed **on the return series they are
//! given** and say nothing about whether that series is itself honest. They **cannot** remove per-trade
//! optimistic bias:
//!
//! - **Transaction-cost bias** — under-charged slippage/impact inflates every trade uniformly. The DSR
//!   is *absolute* (vs a noise ceiling), so a systematic cost error flows through **undeflated**.
//!   Corrected upstream by net-of-cost truth (QE-403) and cost calibration (QE-431/QE-440), not here.
//! - **Adverse-selection bias** — maker-fill markout; a rebate that loses to adverse selection reads as
//!   free edge (QE-449).
//! - **Survivorship bias** — backtesting on a today's-membership universe that silently dropped
//!   delisted blow-ups. Corrected upstream by the **point-in-time universe** (QE-012), whose exact
//!   roster + `[listed, delisted)` windows ride the vintage lineage SHA via `Config::content_hash`
//!   (see `qe_determinism::Lineage`), so a vintage is traceable to a survivorship-safe universe.
//!
//! A clean DSR/PBO/SPA is evidence the *selection* was honest — never proof the *inputs* were.

pub mod dsr;
pub mod ic;
pub mod nulls;
pub mod pbo;
pub mod spa;
pub mod stats;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use dsr::{
    deflated_sharpe_ratio, effective_trials, expected_max_sharpe, expected_max_sharpe_ln,
    probabilistic_sharpe_ratio, trial_sharpe_variance,
};
pub use ic::{
    benjamini_hochberg, forward_returns, rank_ic, screen_catalogue, spearman_pvalue, HorizonIc,
    IcScreenConfig, IcScreenReport, IndicatorScreen, IndicatorSignals, Verdict,
};
pub use nulls::{buy_and_hold_returns, random_entry_returns, realised_turnover};
pub use pbo::{pbo_cscv, PboReport};
pub use spa::{reality_check_pvalue, SpaConfig};
pub use stats::{
    kurtosis, mean, normal_cdf, normal_ppf, sharpe_ratio, skewness, std_dev, variance,
    EULER_MASCHERONI,
};

/// Errors from the robustness suite.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// CSCV requires an even block count `≥ 2`.
    #[error("CSCV block count must be even and >= 2, got {0}")]
    OddBlockCount(usize),
    /// The return matrix is empty or has fewer rows than blocks.
    #[error("return matrix is empty or has fewer rows than blocks")]
    EmptyMatrix,
}

/// The inputs needed to assess one vintage (the caller extracts these from the search/portfolio outputs).
#[derive(Debug, Clone)]
pub struct VintageStats<'a> {
    /// The candidate ensemble's per-period net-of-cost returns.
    pub candidate_returns: &'a [f64],
    /// The per-trial return series that form the CSCV columns (e.g. the elite pool). `trial_returns[k]`
    /// is trial `k`'s series. This is the *portfolio-overfitting* population, which may be a censored
    /// (top-N) sample — keep it distinct from `variance_returns`.
    pub trial_returns: &'a [Vec<f64>],
    /// The **uncensored** trial population whose cross-trial Sharpe *dispersion* sets the DSR deflation
    /// bar (QE-414): the Sharpes of every occupied archive cell, not just the top-N by fitness. Kept
    /// separate from `trial_returns` because a censored survivor sample under-estimates dispersion and
    /// inflates the DSR. `variance_returns[k]` is trial `k`'s series.
    pub variance_returns: &'a [Vec<f64>],
    /// Per-period performance of each trial **relative to the benchmark** (for the Reality Check / SPA).
    pub excess_over_benchmark: &'a [Vec<f64>],
    /// Effective number of trials = cells × generations × windows ([`effective_trials`]).
    pub n_trials: usize,
    /// CSCV block count (even, `≥ 2`).
    pub cscv_blocks: usize,
}

/// The robustness diagnostics for a vintage — `serde` so gate G1 (QE-134) can consume/record it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobustnessReport {
    /// The candidate's raw (undeflated) in-sample Sharpe.
    pub observed_sharpe: f64,
    /// Deflated Sharpe Ratio — `P(true Sharpe > best-of-N noise)`.
    pub dsr: f64,
    /// Probability of Backtest Overfitting (CSCV).
    pub pbo: f64,
    /// White's Reality Check / SPA data-snooping p-value.
    pub spa_pvalue: f64,
    /// Effective number of trials the DSR deflated against.
    pub n_trials: usize,
    /// The cross-trial Sharpe **variance** that set the deflation bar `E[max SR]` (QE-414). Recorded so
    /// the deflation basis is auditable alongside `n_trials`.
    pub trial_variance: f64,
    /// The number of trial Sharpes `trial_variance` was estimated from — the size of the uncensored
    /// variance population (QE-414). Paired with `n_trials`, this makes the deflation basis auditable:
    /// the dispersion and the trial count both derive from the same (full-cell) population.
    pub variance_trials: usize,
}

/// Compute DSR / PBO / SPA for a vintage (QE-131/D6, the AC entry point). The bootstrap p-value is seeded
/// by `seed` for reproducibility (QE-006).
///
/// # Errors
/// Propagates [`ValidationError`] from the CSCV stage (odd block count, empty matrix).
pub fn assess(
    stats: &VintageStats,
    cfg: &SpaConfig,
    seed: u64,
) -> Result<RobustnessReport, ValidationError> {
    let pbo_report = pbo_cscv(stats.trial_returns, stats.cscv_blocks)?;
    // QE-414: the deflation bar's dispersion comes from the uncensored full-cell population, not the
    // (possibly top-N) CSCV `trial_returns`. Both are recorded so the basis is auditable.
    let trial_variance = trial_sharpe_variance(stats.variance_returns);
    Ok(RobustnessReport {
        observed_sharpe: sharpe_ratio(stats.candidate_returns),
        dsr: deflated_sharpe_ratio(stats.candidate_returns, trial_variance, stats.n_trials),
        pbo: pbo_report.pbo,
        spa_pvalue: reality_check_pvalue(stats.excess_over_benchmark, cfg, seed),
        n_trials: stats.n_trials,
        trial_variance,
        variance_trials: stats.variance_returns.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assess_produces_a_full_report_and_round_trips() {
        // A genuinely strong candidate, a small trial population, and excess-vs-benchmark series.
        let candidate: Vec<f64> = (0..240)
            .map(|i| 0.015 + 0.002 * ((i % 5) as f64 - 2.0))
            .collect();
        let trials: Vec<Vec<f64>> = (0..6)
            .map(|k| {
                (0..240)
                    .map(|i| 0.01 + 0.001 * (k as f64) + 0.002 * ((i % 4) as f64 - 1.5))
                    .collect()
            })
            .collect();
        let excess: Vec<Vec<f64>> = trials
            .iter()
            .map(|t| t.iter().map(|x| x - 0.008).collect())
            .collect();

        let stats = VintageStats {
            candidate_returns: &candidate,
            trial_returns: &trials,
            variance_returns: &trials,
            excess_over_benchmark: &excess,
            n_trials: effective_trials(64, 30, 4),
            cscv_blocks: 6,
        };
        let report = assess(&stats, &SpaConfig::with_defaults(), 2024).unwrap();

        // All three diagnostics are populated and in-range.
        assert!((0.0..=1.0).contains(&report.dsr));
        assert!((0.0..=1.0).contains(&report.pbo));
        assert!((0.0..=1.0).contains(&report.spa_pvalue));
        assert_eq!(report.n_trials, 64 * 30 * 4);
        assert!(report.observed_sharpe > 0.0);
        // QE-414: the deflation basis is recorded — the variance and the population it was estimated from.
        assert_eq!(report.variance_trials, trials.len());
        assert!((report.trial_variance - trial_sharpe_variance(&trials)).abs() < 1e-12);

        // The report round-trips through serde (so G1 can record it).
        let json = serde_json::to_string(&report).unwrap();
        let back: RobustnessReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn assess_propagates_cscv_errors() {
        let trials = vec![vec![0.01, 0.02]; 8];
        let stats = VintageStats {
            candidate_returns: &[0.01, 0.02],
            trial_returns: &trials,
            variance_returns: &trials,
            excess_over_benchmark: &trials,
            n_trials: 100,
            cscv_blocks: 3, // odd
        };
        assert!(matches!(
            assess(&stats, &SpaConfig::with_defaults(), 1),
            Err(ValidationError::OddBlockCount(3))
        ));
    }
}
