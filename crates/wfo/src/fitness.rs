//! Geometric (time-average) fitness and noise-robust evaluation (QE-113).
//!
//! Fitness is the **ergodic / time-average log-growth** of a genome's **net-of-cost** per-period
//! returns (QE-109 produces the net series). We optimise `mean ln(1+r)`, not the arithmetic mean,
//! because compounding is multiplicative — the time-average is what an investor actually experiences,
//! and `ln` penalises volatility drag and makes **near-ruin absorbing** (a single `r ≤ −1` ⇒ `−∞`).
//!
//! A single backtest number is one draw from a fat-tailed distribution, so genomes are evaluated over
//! several windows ([`NoiseRobustFitness`]) and an elite is only displaced when a challenger clears the
//! **standard-error noise band** ([`should_replace`]) — never on a lucky single improvement.

/// Default replacement threshold, in combined standard errors (QE-113/D4).
pub const DEFAULT_K_SIGMA: f64 = 1.0;

/// Time-average (geometric) **log-growth** of net per-period returns `r` — the fitness optimised by the
/// search (QE-113/D1): `mean_i ln(1 + r_i)`.
///
/// Returns `f64::NEG_INFINITY` if any `r_i ≤ −1` (a total loss — ruin is absorbing and the worst
/// possible fitness), and `0.0` for an empty series (no growth; the minimum-trade gate is QE-120).
#[must_use]
pub fn log_growth(returns: &[f64]) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let mut sum_log = 0.0;
    for &r in returns {
        let g = 1.0 + r;
        if g <= 0.0 {
            return f64::NEG_INFINITY; // ≤ −100% wipes the account: absorbing ruin
        }
        sum_log += g.ln();
    }
    sum_log / returns.len() as f64
}

/// The per-period **compound return** equivalent to [`log_growth`] — `exp(log_growth) − 1` — a
/// human-readable rate. Reports `−1.0` (−100%) on ruin and `0.0` on an empty series.
#[must_use]
pub fn geom_return(returns: &[f64]) -> f64 {
    let g = log_growth(returns);
    if g == f64::NEG_INFINITY {
        return -1.0;
    }
    g.exp() - 1.0
}

/// A genome's fitness as a **distribution** over evaluation windows (QE-113/D3): the mean per-window
/// [`log_growth`] and the standard error of that mean. Noise-robustness needs `n ≥ 2`; at `n = 1` the
/// standard error is `0.0` (no noise estimate available).
///
/// `serde`-serialisable so it can ride a persisted strategy record (QE-123). Persisted records are always
/// finite (the quality gate rejects non-finite means), so JSON round-trips cleanly.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NoiseRobustFitness {
    /// Mean per-window log-growth (`−∞` if ruined in any window).
    pub mean: f64,
    /// Standard error of the mean (`sample_sd / sqrt(n)`).
    pub std_error: f64,
    /// Number of windows evaluated.
    pub n: usize,
}

impl NoiseRobustFitness {
    /// Evaluate the per-window [`log_growth`] of each window's return series, then summarise as
    /// mean ± standard error. An empty set of windows yields `{ mean: 0, std_error: 0, n: 0 }`; any
    /// ruined window drives `mean` to `−∞`.
    #[must_use]
    pub fn from_windows(windows: &[Vec<f64>]) -> Self {
        let n = windows.len();
        if n == 0 {
            return NoiseRobustFitness {
                mean: 0.0,
                std_error: 0.0,
                n: 0,
            };
        }
        let growths: Vec<f64> = windows.iter().map(|w| log_growth(w)).collect();
        let mean = growths.iter().sum::<f64>() / n as f64;
        // Sample standard deviation (n−1); SE = sd / sqrt(n). Undefined/zero for n < 2.
        let std_error = if n < 2 || !mean.is_finite() {
            0.0
        } else {
            let var = growths
                .iter()
                .map(|g| {
                    let d = g - mean;
                    d * d
                })
                .sum::<f64>()
                / (n as f64 - 1.0);
            (var / n as f64).sqrt()
        };
        NoiseRobustFitness { mean, std_error, n }
    }
}

/// Whether `challenger` should displace `incumbent` as the cell elite (QE-113/D4): only if its mean
/// beats the incumbent's by more than `k_sigma` **combined standard errors** — an improvement inside the
/// noise band is rejected, so the archive does not churn elites on a noisy single draw.
///
/// Ruin (`−∞`) never displaces a finite incumbent. With both `n = 1` the combined SE is `0` and the
/// rule degenerates to strict-greater (callers must pass `n ≥ 2` windows for the noise guard to bite).
///
/// This decision is **purely return-driven** and is deliberately kept free of any parsimony / MDL term
/// (QE-436): it is the value that feeds the deflation stage, so a complexity penalty here would distort
/// the DSR-facing fitness. The parsimony tie-break lives in [`should_replace_parsimonious`], which only
/// engages *inside* this rule's noise band.
#[must_use]
pub fn should_replace(
    incumbent: &NoiseRobustFitness,
    challenger: &NoiseRobustFitness,
    k_sigma: f64,
) -> bool {
    if !challenger.mean.is_finite() {
        return false; // ruined / non-finite challenger never replaces
    }
    if !incumbent.mean.is_finite() {
        return true; // any finite challenger beats a ruined/empty incumbent
    }
    let combined_se = (incumbent.std_error.powi(2) + challenger.std_error.powi(2)).sqrt();
    challenger.mean - incumbent.mean > k_sigma * combined_se
}

/// Whether two finite fitnesses are **statistically indistinguishable** — neither clearly beats the other
/// within `k_sigma` combined standard errors (QE-436): `|a.mean − b.mean| ≤ k_sigma·combined_se`. This is
/// the "equal robust fitness" region in which a parsimony tie-break is legitimate. A non-finite (ruined)
/// fitness is never in-band (there is a clear loser), so this returns `false`.
#[must_use]
pub fn within_noise_band(a: &NoiseRobustFitness, b: &NoiseRobustFitness, k_sigma: f64) -> bool {
    if !a.mean.is_finite() || !b.mean.is_finite() {
        return false;
    }
    let combined_se = (a.std_error.powi(2) + b.std_error.powi(2)).sqrt();
    (a.mean - b.mean).abs() <= k_sigma * combined_se
}

/// Like [`should_replace`], but with a **lexicographic parsimony (MDL) tie-break** inside the noise band
/// (QE-436). The decision is fitness-first, complexity-second:
///
/// 1. if `challenger` clearly beats `incumbent` on robust fitness ⇒ replace (regardless of complexity);
/// 2. otherwise if there is a clear loser (incumbent clearly better, or a ruined challenger) ⇒ keep;
/// 3. otherwise the two are within the noise band — an *equal robust fitness* tie — and we break toward
///    parsimony: replace iff the challenger is **strictly simpler** (`chal_complexity < inc_complexity`).
///
/// Complexity is a genotype count (e.g. [`Genome::mdl_complexity`](crate::genome::Genome::mdl_complexity));
/// it is consulted **only** in branch 3, so it can never override a material fitness difference and never
/// enters a fitness value — the MDL term stays out of the DSR-facing fitness. Deterministic (integer
/// comparison), so byte-reproducibility is preserved.
#[must_use]
pub fn should_replace_parsimonious(
    incumbent: &NoiseRobustFitness,
    challenger: &NoiseRobustFitness,
    inc_complexity: u32,
    chal_complexity: u32,
    k_sigma: f64,
) -> bool {
    // Branch 1: a clear fitness win always replaces, no matter the complexity.
    if should_replace(incumbent, challenger, k_sigma) {
        return true;
    }
    // Branch 3: inside the noise band (equal robust fitness) break toward parsimony.
    if within_noise_band(incumbent, challenger, k_sigma) {
        return chal_complexity < inc_complexity;
    }
    // Branch 2: a clear loser (incumbent clearly better, or ruined challenger) never replaces.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    #[test]
    fn log_growth_and_geom_return_show_volatility_drag() {
        // +50% then −50% nets a loss (1.5 × 0.5 = 0.75): geometric mean < arithmetic mean (0).
        let r = [0.5, -0.5];
        let lg = log_growth(&r);
        approx(lg, ((1.5_f64).ln() + (0.5_f64).ln()) / 2.0);
        assert!(lg < 0.0, "round-trip must show drag");
        // Equivalent per-period compound return = sqrt(0.75) − 1 ≈ −0.1340.
        approx(geom_return(&r), (0.75_f64).sqrt() - 1.0);
        // A flat series compounds to 0.
        approx(log_growth(&[0.0, 0.0, 0.0]), 0.0);
        approx(geom_return(&[0.0, 0.0]), 0.0);
    }

    #[test]
    fn near_ruin_is_absorbing() {
        assert_eq!(log_growth(&[0.1, -1.0, 0.2]), f64::NEG_INFINITY); // exactly −100%
        assert_eq!(log_growth(&[0.1, -1.5]), f64::NEG_INFINITY); // worse than −100%
        assert_eq!(geom_return(&[-1.0]), -1.0);
        // Empty series is neutral, not ruin.
        assert_eq!(log_growth(&[]), 0.0);
        assert_eq!(geom_return(&[]), 0.0);
    }

    #[test]
    fn noise_robust_mean_and_se() {
        // Two windows with equal, constant returns → identical log-growths → SE = 0.
        let same = NoiseRobustFitness::from_windows(&[vec![0.1, 0.1], vec![0.1, 0.1]]);
        approx(same.mean, (1.1_f64).ln());
        approx(same.std_error, 0.0);
        assert_eq!(same.n, 2);

        // Two distinct windows: hand-check mean and SE.
        let g1 = log_growth(&[0.2]);
        let g2 = log_growth(&[-0.1]);
        let nf = NoiseRobustFitness::from_windows(&[vec![0.2], vec![-0.1]]);
        approx(nf.mean, (g1 + g2) / 2.0);
        // sample sd with n−1=1: |g1−g2|/2 *... var = ((g1-m)^2+(g2-m)^2)/1; SE = sqrt(var/2).
        let m = (g1 + g2) / 2.0;
        let var = (g1 - m).powi(2) + (g2 - m).powi(2);
        approx(nf.std_error, (var / 2.0).sqrt());

        // A ruined window poisons the mean.
        let ruined = NoiseRobustFitness::from_windows(&[vec![0.1], vec![-1.0]]);
        assert_eq!(ruined.mean, f64::NEG_INFINITY);
        assert_eq!(ruined.std_error, 0.0);

        // No windows → neutral.
        let empty = NoiseRobustFitness::from_windows(&[]);
        assert_eq!((empty.mean, empty.n), (0.0, 0));
    }

    #[test]
    fn replacement_respects_standard_error() {
        let incumbent = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.02,
            n: 5,
        };
        // Challenger better by 0.01 but combined SE ≈ 0.0283 → inside the 1σ band → no replace.
        let noisy = NoiseRobustFitness {
            mean: 0.11,
            std_error: 0.02,
            n: 5,
        };
        assert!(!should_replace(&incumbent, &noisy, DEFAULT_K_SIGMA));
        // Challenger better by 0.10 → well outside the band → replace.
        let robust = NoiseRobustFitness {
            mean: 0.20,
            std_error: 0.02,
            n: 5,
        };
        assert!(should_replace(&incumbent, &robust, DEFAULT_K_SIGMA));
        // Ruined challenger never replaces; any finite challenger beats a ruined incumbent.
        let ruined = NoiseRobustFitness {
            mean: f64::NEG_INFINITY,
            std_error: 0.0,
            n: 3,
        };
        assert!(!should_replace(&incumbent, &ruined, DEFAULT_K_SIGMA));
        assert!(should_replace(&ruined, &incumbent, DEFAULT_K_SIGMA));
    }

    #[test]
    fn single_window_degenerates_to_strict_greater() {
        let a = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.0,
            n: 1,
        };
        let b = NoiseRobustFitness {
            mean: 0.1000001,
            std_error: 0.0,
            n: 1,
        };
        // No noise estimate (n=1) ⇒ any positive improvement replaces.
        assert!(should_replace(&a, &b, DEFAULT_K_SIGMA));
        assert!(!should_replace(&b, &a, DEFAULT_K_SIGMA));
    }

    // --- QE-436 parsimony (MDL) tie-break ----------------------------------------------------

    #[test]
    fn parsimony_breaks_ties_at_equal_robust_fitness() {
        // Two genomes with essentially equal robust fitness (well inside the 1σ band): a 4-clause
        // incumbent (complexity 6) vs a 1-clause challenger (complexity 2). The simpler one wins.
        let incumbent = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.02,
            n: 5,
        };
        let challenger = NoiseRobustFitness {
            mean: 0.1005, // inside the noise band → a statistical tie
            std_error: 0.02,
            n: 5,
        };
        assert!(within_noise_band(&incumbent, &challenger, DEFAULT_K_SIGMA));
        // The 1-clause challenger (complexity 2) displaces the 4-clause incumbent (complexity 6).
        assert!(should_replace_parsimonious(
            &incumbent,
            &challenger,
            6,
            2,
            DEFAULT_K_SIGMA
        ));
        // Symmetric: a *more* complex challenger at the same tie does NOT displace the simpler incumbent.
        assert!(!should_replace_parsimonious(
            &incumbent,
            &challenger,
            2,
            6,
            DEFAULT_K_SIGMA
        ));
        // Equal complexity at a tie ⇒ no churn (falls through to keep the incumbent).
        assert!(!should_replace_parsimonious(
            &incumbent,
            &challenger,
            4,
            4,
            DEFAULT_K_SIGMA
        ));
    }

    #[test]
    fn parsimony_never_overrides_a_material_fitness_difference() {
        let incumbent = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.02,
            n: 5,
        };
        // A materially BETTER but more-complex challenger still replaces (fitness wins over parsimony).
        let much_better = NoiseRobustFitness {
            mean: 0.20,
            std_error: 0.02,
            n: 5,
        };
        assert!(!within_noise_band(
            &incumbent,
            &much_better,
            DEFAULT_K_SIGMA
        ));
        assert!(should_replace_parsimonious(
            &incumbent,
            &much_better,
            2,  // incumbent simple
            10, // challenger far more complex
            DEFAULT_K_SIGMA
        ));
        // A materially WORSE but simpler challenger never replaces (parsimony cannot buy back edge).
        let much_worse = NoiseRobustFitness {
            mean: 0.02,
            std_error: 0.02,
            n: 5,
        };
        assert!(!within_noise_band(&incumbent, &much_worse, DEFAULT_K_SIGMA));
        assert!(!should_replace_parsimonious(
            &incumbent,
            &much_worse,
            10, // incumbent complex
            1,  // challenger simplest possible
            DEFAULT_K_SIGMA
        ));
        // And the plain, DSR-facing `should_replace` is untouched by complexity — it never sees it.
        assert!(should_replace(&incumbent, &much_better, DEFAULT_K_SIGMA));
        assert!(!should_replace(&incumbent, &much_worse, DEFAULT_K_SIGMA));
    }

    #[test]
    fn parsimony_tiebreak_respects_ruin_and_is_deterministic() {
        let finite = NoiseRobustFitness {
            mean: 0.10,
            std_error: 0.02,
            n: 5,
        };
        let ruined = NoiseRobustFitness {
            mean: f64::NEG_INFINITY,
            std_error: 0.0,
            n: 5,
        };
        // A ruined challenger never replaces, however simple (not in-band: there is a clear loser).
        assert!(!within_noise_band(&finite, &ruined, DEFAULT_K_SIGMA));
        assert!(!should_replace_parsimonious(
            &finite,
            &ruined,
            10,
            0,
            DEFAULT_K_SIGMA
        ));
        // Any finite challenger beats a ruined incumbent (branch 1: a clear fitness win).
        assert!(should_replace_parsimonious(
            &ruined,
            &finite,
            0,
            10,
            DEFAULT_K_SIGMA
        ));
        // Deterministic: repeated evaluation is identical.
        let tie = NoiseRobustFitness {
            mean: 0.1004,
            std_error: 0.02,
            n: 5,
        };
        let once = should_replace_parsimonious(&finite, &tie, 6, 2, DEFAULT_K_SIGMA);
        for _ in 0..8 {
            assert_eq!(
                should_replace_parsimonious(&finite, &tie, 6, 2, DEFAULT_K_SIGMA),
                once
            );
        }
        assert!(once);
    }
}
