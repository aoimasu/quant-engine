//! Deflated Sharpe Ratio (QE-131/D2 — Bailey & López de Prado 2014).
//!
//! A large QD archive is a multiple-testing machine: the *maximum* in-sample Sharpe over `N` trials is
//! large under a zero-edge null purely by selection. The DSR asks whether the observed Sharpe exceeds the
//! Sharpe that best-of-`N` noise alone would be expected to produce, adjusted for the return series'
//! non-normality (skew/kurtosis) and length.

use crate::stats::{kurtosis, normal_cdf, normal_ppf, sharpe_ratio, skewness, EULER_MASCHERONI};

/// The effective number of independent trials `N = cells · generations · windows` (QE-131) — the
/// **analytic floor** the DSR deflation bar is computed against.
///
/// **Basis coherence (QE-439).** This is the current, deliberately *conservative* independent-trials
/// basis. The **cell** factor is coherent with the trial-Sharpe dispersion `V`: QE-414 estimates `V`
/// from the champion of every occupied cell, so `N`'s cell factor and `V`'s population are the same
/// niches. The **generations** and **windows** factors are *not* independent — serial mutations of one
/// persistent elite and re-evaluations of one strategy over windows are correlated draws — so the
/// product **over-counts** hypotheses. Over-counting raises the noise bar ⇒ **over-deflates** ⇒
/// false-*reject*, which is the safe direction (under-deflation / false-accept is the dangerous one),
/// so the floor is kept as-is. The coherent tightening — `N = max(distinct-canonical formulas ever
/// scored, this floor, a complexity floor)` with a GP evaluation ledger — is deferred to the GP program
/// (QE-451 / see `docs/architecture/qe-439-dsr-trial-basis-design.md`), which has an evaluation count to
/// distil; there is nothing to count on the hand catalogue.
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
///
/// **Log-N numerical fix (QE-439).** The upper-tail argument `1 − 1/N` loses all precision once `1/N`
/// drops below the ULP of `1.0` (`2⁻⁵³ ≈ 1.11e-16`, i.e. `N ≳ 4.5e15`): it rounds to exactly `1.0`,
/// `normal_ppf(1.0)` returns `+∞`, and the bar degenerates to `+∞` (DSR ≡ 0 for every candidate,
/// carrying no information). Below that regime this keeps the **exact** path (byte-identical to the
/// pre-QE-439 behaviour); at or above it, it switches to the numerically-stable [`expected_max_sharpe_ln`]
/// log-space path (`≈ √(2 ln N)`), so the bar stays finite and self-caps near 8–13 even at `N ~ 1e20`.
/// The two paths are continuous: the log path reuses the same Acklam upper-tail rational `normal_ppf`
/// already runs for `p > P_HIGH`, only avoiding the cancellation of forming `1 − 1/N`.
#[must_use]
pub fn expected_max_sharpe(trial_variance: f64, n_trials: usize) -> f64 {
    if n_trials <= 1 || trial_variance <= 0.0 {
        return 0.0;
    }
    let n = n_trials as f64;
    let p1 = 1.0 - 1.0 / n;
    let p2 = 1.0 - 1.0 / (n * std::f64::consts::E);
    // Degenerate regime: forming `1 − 1/N` in f64 lost the tail ⇒ `normal_ppf` would return `+∞`.
    // Take the log-space path, which never forms `1 − 1/N`.
    if p1 >= 1.0 || p2 >= 1.0 {
        return expected_max_sharpe_ln(trial_variance, n.ln());
    }
    let gamma = EULER_MASCHERONI;
    let z1 = normal_ppf(p1);
    let z2 = normal_ppf(p2);
    trial_variance.sqrt() * ((1.0 - gamma) * z1 + gamma * z2)
}

/// The best-of-`N` noise Sharpe bar computed in **log space** from `ln_n = ln N` (QE-439), for the
/// large-`N` regime where forming `1 − 1/N` in `f64` underflows to `1.0` and [`expected_max_sharpe`]'s
/// direct `normal_ppf` path degenerates to `+∞`.
///
/// The two upper-tail quantiles `Φ⁻¹(1 − 1/N)` and `Φ⁻¹(1 − 1/(N·e))` have tail probabilities
/// `p = 1/N` and `1/(N·e)`, so `ln p = −ln N` and `−ln N − 1`. By symmetry `Φ⁻¹(1 − p) = −Φ⁻¹(p)`, and
/// Acklam's lower-tail branch uses `q = √(−2 ln p) = √(2 ln N)` — the classic `E[max SR] ≈ √(2 ln N)`
/// asymptotic — so the bar self-caps (`√(2 ln 1e20) ≈ 9.6`) rather than blowing up. This is the identical
/// rational `normal_ppf` runs in its `p > P_HIGH` branch, evaluated directly from `ln N`.
///
/// Returns `0.0` for non-positive `trial_variance` or non-finite / non-positive `ln_n` (`N ≤ 1`).
#[must_use]
pub fn expected_max_sharpe_ln(trial_variance: f64, ln_n: f64) -> f64 {
    if trial_variance <= 0.0 || !ln_n.is_finite() || ln_n <= 0.0 {
        return 0.0;
    }
    let gamma = EULER_MASCHERONI;
    // Tail log-probabilities: p1 = 1/N ⇒ ln p1 = −ln N; p2 = 1/(N·e) ⇒ ln p2 = −ln N − 1.
    let z1 = normal_ppf_upper_tail_from_ln(-ln_n);
    let z2 = normal_ppf_upper_tail_from_ln(-ln_n - 1.0);
    trial_variance.sqrt() * ((1.0 - gamma) * z1 + gamma * z2)
}

/// `Φ⁻¹(1 − p)` for a small upper-tail probability `p`, computed from `ln_p = ln p` (`≤ 0`) so it never
/// underflows. Acklam's low-`p` rational on `q = √(−2 ln p)`, negated by the symmetry
/// `Φ⁻¹(1 − p) = −Φ⁻¹(p)` — the same coefficients `stats::normal_ppf` uses in its tail branches.
fn normal_ppf_upper_tail_from_ln(ln_p: f64) -> f64 {
    // Acklam's C/D coefficients (tail region), identical to `stats::normal_ppf`.
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838e0,
        -2.549_732_539_343_734e0,
        4.374_664_141_464_968e0,
        2.938_163_982_698_783e0,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996e0,
        3.754_408_661_907_416e0,
    ];
    let q = (-2.0 * ln_p).sqrt();
    let num = ((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5];
    let den = (((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0;
    // Acklam's low branch yields Φ⁻¹(p) (negative for small p); negate for the upper tail Φ⁻¹(1−p).
    -(num / den)
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
    fn expected_max_sharpe_is_finite_and_self_caps_at_gp_scale() {
        // QE-439 headline: the pre-fix path formed `1 − 1/N`, which rounds to 1.0 for N ≳ 4.5e15, so
        // `normal_ppf(1.0) = +∞` degenerated the bar. The fix keeps it finite and self-caps near
        // √(2 ln N) — ≈ 8–13 at unit variance.
        //
        // `usize` tops out at ~1.84e19, so the `usize`-typed entry point is exercised up to the degenerate
        // regime it can represent; the N ~ 1e20 claim runs through the log-space entry point directly.
        for &n in &[
            5_000_000_000_000_000usize, // 5e15, just into the degenerate regime
            1_000_000_000_000_000_000,  // 1e18
            10_000_000_000_000_000_000, // 1e19
            usize::MAX,                 // ~1.84e19, the largest representable trial count
        ] {
            let bar = expected_max_sharpe(1.0, n);
            assert!(bar.is_finite(), "bar must be finite at N={n}, got {bar}");
            assert!(
                (8.0..=13.0).contains(&bar),
                "bar must self-cap in ~[8,13] at unit variance (N={n}), got {bar}"
            );
        }
        // The headline finite-at-1e20 result: √(2 ln 1e20) ≈ 9.6, via the log-space path (1e20 > usize::MAX).
        let ln_1e20 = 1e20_f64.ln();
        let at_1e20 = expected_max_sharpe_ln(1.0, ln_1e20);
        assert!(
            at_1e20.is_finite(),
            "N=1e20 bar must be finite, got {at_1e20}"
        );
        assert!(
            (9.0..=10.5).contains(&at_1e20),
            "N=1e20 bar should sit near √(2 ln N) ≈ 9.6, got {at_1e20}"
        );
        // Variance scales the bar by √V (a pure multiplier), still finite.
        let scaled = expected_max_sharpe_ln(0.04, ln_1e20);
        assert!(
            scaled.is_finite() && (scaled - at_1e20 * 0.2).abs() < 1e-9,
            "√V scaling must hold in the log path: {scaled} vs {}",
            at_1e20 * 0.2
        );
    }

    #[test]
    fn small_n_path_is_unchanged_and_exact() {
        // Below the degenerate threshold the exact `normal_ppf` path is retained byte-for-bit — the fix
        // must not perturb any current DSR value (the fixture / all real runs live here).
        for &n in &[2usize, 10, 41, 42, 1_000, 20_000, 1_000_000_000] {
            let nf = n as f64;
            let gamma = EULER_MASCHERONI;
            let z1 = normal_ppf(1.0 - 1.0 / nf);
            let z2 = normal_ppf(1.0 - 1.0 / (nf * std::f64::consts::E));
            let expected = (0.05_f64).sqrt() * ((1.0 - gamma) * z1 + gamma * z2);
            let got = expected_max_sharpe(0.05, n);
            assert_eq!(got, expected, "small-N path must be bit-identical at N={n}");
            assert!(got.is_finite());
        }
    }

    #[test]
    fn log_path_is_continuous_with_the_exact_path() {
        // Where both are valid, `expected_max_sharpe_ln` tracks the exact `expected_max_sharpe` — the log
        // path is the same Acklam rational, not a divergent approximation. The only residual is the exact
        // path's catastrophic `1 − 1/N` cancellation (which grows with N and the log path avoids), so the
        // agreement is asserted as a small *relative* tolerance — the log path is the more accurate side.
        for &n in &[100usize, 10_000, 1_000_000, 1_000_000_000_000] {
            let exact = expected_max_sharpe(0.03, n);
            let via_ln = expected_max_sharpe_ln(0.03, (n as f64).ln());
            let rel = (exact - via_ln).abs() / exact.abs();
            assert!(
                rel < 1e-5,
                "log path must track exact at N={n} (rel {rel:e}): {exact} vs {via_ln}"
            );
        }
        // Guards: N ≤ 1 and non-positive variance ⇒ 0.
        assert_eq!(expected_max_sharpe_ln(1.0, 0.0), 0.0);
        assert_eq!(expected_max_sharpe_ln(0.0, 10.0), 0.0);
        assert_eq!(expected_max_sharpe_ln(-1.0, 10.0), 0.0);
    }

    #[test]
    fn deflation_bar_is_monotone_across_the_switch_and_stays_bounded() {
        // The bar rises with N through the exact→log switch without a discontinuity or blow-up, and stays
        // below the √(2 ln N) ceiling (14·√V) all the way to N = 1e20.
        let ns: [usize; 6] = [
            1_000,
            1_000_000,
            1_000_000_000_000,          // 1e12, exact path
            10_000_000_000_000_000,     // 1e16, log path
            1_000_000_000_000_000_000,  // 1e18
            10_000_000_000_000_000_000, // 1e19, near usize::MAX
        ];
        let v = 0.02_f64;
        let mut prev = 0.0;
        for &n in &ns {
            let bar = expected_max_sharpe(v, n);
            assert!(bar.is_finite(), "bar must stay finite at N={n}");
            assert!(bar > prev, "bar must rise with N: {prev} !< {bar} at N={n}");
            assert!(
                bar < 14.0 * v.sqrt(),
                "bar must stay under the √(2 ln N) ceiling at N={n}, got {bar}"
            );
            prev = bar;
        }
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
