//! Deflated Sharpe Ratio (QE-131/D2 — Bailey & López de Prado 2014).
//!
//! A large QD archive is a multiple-testing machine: the *maximum* in-sample Sharpe over `N` trials is
//! large under a zero-edge null purely by selection. The DSR asks whether the observed Sharpe exceeds the
//! Sharpe that best-of-`N` noise alone would be expected to produce, adjusted for the return series'
//! non-normality (skew/kurtosis) and length.

use crate::stats::{kurtosis, normal_cdf, normal_ppf, sharpe_ratio, skewness, EULER_MASCHERONI};

/// The effective number of independent trials `N = cells · generations · windows` (QE-131): every
/// archive cell, evolved over every generation, evaluated over every window, is a draw the deflation
/// must account for.
#[must_use]
pub fn effective_trials(cells: usize, generations: usize, windows: usize) -> usize {
    cells.saturating_mul(generations).saturating_mul(windows)
}

/// The Probabilistic Sharpe Ratio `PSR(SR*)` — the probability the strategy's *true* Sharpe exceeds the
/// benchmark `sr_benchmark`, given the sample Sharpe, length, skew and kurtosis of `returns`
/// (Bailey & López de Prado):
///
/// ```text
/// PSR(SR*) = Φ[ (SR − SR*)·√(T−1) / √(1 − γ3·SR + ((γ4−1)/4)·SR²) ]
/// ```
///
/// Returns `0.5` when the series is too short or dispersionless to estimate (no information either way).
#[must_use]
pub fn probabilistic_sharpe_ratio(returns: &[f64], sr_benchmark: f64) -> f64 {
    let t = returns.len();
    if t < 2 {
        return 0.5;
    }
    let sr = sharpe_ratio(returns);
    let g3 = skewness(returns);
    let g4 = kurtosis(returns);
    let denom = 1.0 - g3 * sr + ((g4 - 1.0) / 4.0) * sr * sr;
    if denom <= 0.0 {
        return 0.5; // degenerate higher moments — no usable estimate
    }
    let z = (sr - sr_benchmark) * ((t - 1) as f64).sqrt() / denom.sqrt();
    normal_cdf(z)
}

/// The Sharpe ratio the **best of `n_trials` independent trials** is expected to exhibit under a zero-edge
/// null, given the cross-trial Sharpe variance `trial_variance` (Bailey & López de Prado):
///
/// ```text
/// E[max SR] = √V · [ (1 − γ)·Z⁻¹(1 − 1/N) + γ·Z⁻¹(1 − 1/(N·e)) ]   (γ = Euler–Mascheroni)
/// ```
///
/// This is the "deflation bar": with more trials or more dispersed trial Sharpes, best-of-`N` noise clears
/// a higher bar. Returns `0.0` for `n_trials ≤ 1` or non-positive variance.
#[must_use]
pub fn expected_max_sharpe(trial_variance: f64, n_trials: usize) -> f64 {
    if n_trials <= 1 || trial_variance <= 0.0 {
        return 0.0;
    }
    let n = n_trials as f64;
    let gamma = EULER_MASCHERONI;
    let z1 = normal_ppf(1.0 - 1.0 / n);
    let z2 = normal_ppf(1.0 - 1.0 / (n * std::f64::consts::E));
    trial_variance.sqrt() * ((1.0 - gamma) * z1 + gamma * z2)
}

/// The Deflated Sharpe Ratio: `PSR(E[max SR])` — the probability the strategy's true Sharpe beats what
/// best-of-`n_trials` data-snooping would produce, given the trial-Sharpe dispersion `trial_variance`
/// (QE-131/D2). A DSR near 1 is evidence of edge beyond the search's multiple-testing; near 0.5 (or below)
/// it is indistinguishable from selection noise.
#[must_use]
pub fn deflated_sharpe_ratio(returns: &[f64], trial_variance: f64, n_trials: usize) -> f64 {
    let sr0 = expected_max_sharpe(trial_variance, n_trials);
    probabilistic_sharpe_ratio(returns, sr0)
}

/// The variance of a set of trial Sharpe ratios — the `trial_variance` input to the deflation, computed
/// from the per-trial return series the caller supplies (e.g. one per archive cell). `0.0` if fewer than
/// two trials.
#[must_use]
pub fn trial_sharpe_variance(trial_returns: &[Vec<f64>]) -> f64 {
    if trial_returns.len() < 2 {
        return 0.0;
    }
    let sharpes: Vec<f64> = trial_returns.iter().map(|r| sharpe_ratio(r)).collect();
    crate::stats::variance(&sharpes, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_trials_multiplies_the_three_axes() {
        assert_eq!(effective_trials(100, 50, 4), 20_000);
        assert_eq!(effective_trials(0, 50, 4), 0);
    }

    #[test]
    fn expected_max_sharpe_rises_with_trials_and_dispersion() {
        let few = expected_max_sharpe(0.01, 10);
        let many = expected_max_sharpe(0.01, 10_000);
        assert!(
            many > few,
            "more trials ⇒ higher noise bar: {few} !< {many}"
        );
        let wide = expected_max_sharpe(0.04, 10_000);
        assert!(
            wide > many,
            "more dispersion ⇒ higher bar: {many} !< {wide}"
        );
        assert_eq!(expected_max_sharpe(0.01, 1), 0.0);
    }

    #[test]
    fn psr_is_monotone_in_track_record() {
        // A modest, noisy edge (Sharpe ≈ 0.02/period — well short of saturating PSR): a longer record of
        // the same edge is more convincing ⇒ higher PSR.
        let short: Vec<f64> = (0..30)
            .map(|i| 0.002 + 0.05 * ((i % 7) as f64 - 3.0))
            .collect();
        let long: Vec<f64> = (0..300)
            .map(|i| 0.002 + 0.05 * ((i % 7) as f64 - 3.0))
            .collect();
        let p_short = probabilistic_sharpe_ratio(&short, 0.0);
        let p_long = probabilistic_sharpe_ratio(&long, 0.0);
        assert!(p_long > p_short, "{p_short} !< {p_long}");
        assert!((0.0..=1.0).contains(&p_long) && (0.0..=1.0).contains(&p_short));
    }

    #[test]
    fn full_trial_variance_deflates_at_least_as_hard_as_top10() {
        // QE-414 AC: on a FIXED population of trial Sharpes (the archive's cell champions), the variance
        // estimated from the FULL population is ≥ the variance from the top-10 by Sharpe, so the DSR
        // computed from the full-trial variance is ≤ the DSR from the top-10 variance.
        //
        // 20 "cells": return series with linearly spreading mean (⇒ spreading Sharpe). The 10 highest are
        // a censored, tightly-clustered tail; the full 20 are more dispersed.
        let trial = |mean: f64| -> Vec<f64> {
            (0..240)
                .map(|i| mean + 0.01 * ((i % 5) as f64 - 2.0))
                .collect::<Vec<f64>>()
        };
        let full: Vec<Vec<f64>> = (0..20).map(|k| trial(0.001 * k as f64)).collect();
        // Top-10 by Sharpe = the 10 largest-mean series (Sharpe is monotone in mean here).
        let top10: Vec<Vec<f64>> = full.iter().skip(10).cloned().collect();

        let var_full = trial_sharpe_variance(&full);
        let var_top10 = trial_sharpe_variance(&top10);
        assert!(
            var_full >= var_top10,
            "full-population dispersion must be ≥ the censored top-10: {var_full} !>= {var_top10}"
        );

        let candidate: Vec<f64> = (0..240)
            .map(|i| 0.02 + 0.01 * ((i % 5) as f64 - 2.0))
            .collect();
        let n_trials = 20 * 4 * 2;
        let dsr_full = deflated_sharpe_ratio(&candidate, var_full, n_trials);
        let dsr_top10 = deflated_sharpe_ratio(&candidate, var_top10, n_trials);
        assert!(
            dsr_full <= dsr_top10,
            "full-trial variance must not inflate the DSR: {dsr_full} !<= {dsr_top10}"
        );
        // And the effect is real on this fixture (strictly harder deflation), not a degenerate tie.
        assert!(
            dsr_full < dsr_top10,
            "the censored top-10 should visibly inflate the DSR here: {dsr_full} vs {dsr_top10}"
        );
    }

    #[test]
    fn deflation_lowers_confidence_as_trials_grow() {
        // A modest edge (Sharpe ≈ 0.14/period) so the deflation bar can rise above it as trials grow.
        let returns: Vec<f64> = (0..500)
            .map(|i| 0.01 + 0.05 * ((i % 5) as f64 - 2.0))
            .collect();
        let undeflated = deflated_sharpe_ratio(&returns, 0.05, 2);
        let deflated = deflated_sharpe_ratio(&returns, 0.05, 100_000);
        assert!(
            deflated < undeflated,
            "more trials must lower DSR: {undeflated} -> {deflated}"
        );
        assert!((0.0..=1.0).contains(&deflated));
    }
}
