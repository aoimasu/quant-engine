//! White's Reality Check / Hansen's SPA (QE-131/D4 — White 2000, Hansen 2005).
//!
//! Given `N` strategies' per-period performance *relative to a benchmark* (`excess[k][t]`), is the best
//! one's edge real, or just the best of `N` draws from a zero-edge null? The test statistic is
//! `V = maxₖ √T·d̄ₖ`; a **stationary bootstrap** (Politis–Romano) of the recentred series gives the null
//! distribution, and the p-value is the share of bootstrap maxima that exceed `V`. A high p-value means
//! the winner is indistinguishable from data-snooping.

use qe_determinism::task_rng;
use rand_core::RngCore;

use crate::stats::{mean, std_dev};

/// Configuration for the bootstrap data-snooping test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaConfig {
    /// Number of bootstrap resamples `B`.
    pub resamples: usize,
    /// Stationary-bootstrap average block length (geometric mean block; `q = 1/avg_block`).
    pub avg_block: f64,
    /// Studentise each strategy's mean by its std (Hansen's SPA refinement) vs raw means (White's RC).
    pub studentize: bool,
}

impl SpaConfig {
    /// Defaults: 1000 resamples, average block length 5, White's Reality Check (no studentisation).
    #[must_use]
    pub fn with_defaults() -> Self {
        SpaConfig {
            resamples: 1_000,
            avg_block: 5.0,
            studentize: false,
        }
    }
}

/// The bootstrap data-snooping p-value for the best of `excess` strategies (`excess[k]` = strategy `k`'s
/// per-period performance minus the benchmark's), seeded deterministically by `seed` (QE-006).
///
/// `p = #{ V*ᵦ ≥ V } / B` where `V = maxₖ √T·s·d̄ₖ`, `V*ᵦ = maxₖ √T·s·(d̄*ₖ − d̄ₖ)`, and `s = 1` (RC) or
/// `1/σₖ` (SPA). Returns `1.0` for an empty/too-short input (no evidence against the null).
#[must_use]
pub fn reality_check_pvalue(excess: &[Vec<f64>], cfg: &SpaConfig, seed: u64) -> f64 {
    let n = excess.len();
    if n == 0 {
        return 1.0;
    }
    let t = excess.iter().map(Vec::len).min().unwrap_or(0);
    if t < 2 {
        return 1.0;
    }
    let sqrt_t = (t as f64).sqrt();

    // Per-strategy mean and (optional) studentising scale.
    let means: Vec<f64> = excess.iter().map(|d| mean(&d[..t])).collect();
    let scale: Vec<f64> = excess
        .iter()
        .map(|d| {
            if cfg.studentize {
                let sd = std_dev(&d[..t]);
                if sd > 0.0 {
                    1.0 / sd
                } else {
                    0.0
                }
            } else {
                1.0
            }
        })
        .collect();

    let observed = (0..n)
        .map(|k| sqrt_t * scale[k] * means[k])
        .fold(f64::NEG_INFINITY, f64::max);

    let q = (1.0 / cfg.avg_block).clamp(f64::MIN_POSITIVE, 1.0);
    let mut exceed = 0usize;
    for b in 0..cfg.resamples {
        let idx = stationary_indices(t, q, seed, b as u64);
        // Recentred bootstrap statistic: maxₖ √T·scale·(mean of resampled dₖ − d̄ₖ).
        let mut v_star = f64::NEG_INFINITY;
        for k in 0..n {
            let series = &excess[k][..t];
            let resampled_mean = idx.iter().map(|&i| series[i]).sum::<f64>() / t as f64;
            let stat = sqrt_t * scale[k] * (resampled_mean - means[k]);
            v_star = v_star.max(stat);
        }
        if v_star >= observed {
            exceed += 1;
        }
    }
    exceed as f64 / cfg.resamples as f64
}

/// Stationary-bootstrap (Politis–Romano) row indices of length `t`: start a new random block with
/// probability `q` each step, otherwise advance within the current block (wrapping). Deterministic via
/// `task_rng(seed, resample)`.
fn stationary_indices(t: usize, q: f64, seed: u64, resample: u64) -> Vec<usize> {
    let mut rng = task_rng(seed, resample);
    let mut out = Vec::with_capacity(t);
    let mut cur = (rng.next_u64() % t as u64) as usize;
    for _ in 0..t {
        out.push(cur);
        if uniform01(&mut rng) < q {
            cur = (rng.next_u64() % t as u64) as usize; // new block start
        } else {
            cur = (cur + 1) % t; // continue the block, wrapping
        }
    }
    out
}

/// A uniform draw in `[0, 1)` from 53 random bits (matches the workspace convention, QE-006).
fn uniform01<R: RngCore>(rng: &mut R) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(offset: f64, n: usize) -> Vec<f64> {
        // A small deterministic oscillation around `offset` (mean ≈ offset, non-zero vol).
        (0..n)
            .map(|i| offset + 0.01 * ((i % 5) as f64 - 2.0))
            .collect()
    }

    #[test]
    fn genuine_edge_has_low_pvalue() {
        // One clearly-positive strategy among zero-mean noise ⇒ the max is real ⇒ low p.
        let mut excess = vec![ramp(0.02, 200)]; // genuine +2%/period edge
        for k in 0..9 {
            excess.push(
                ramp(0.0, 200)
                    .iter()
                    .map(|x| x + 0.0001 * k as f64)
                    .collect(),
            );
        }
        let p = reality_check_pvalue(&excess, &SpaConfig::with_defaults(), 42);
        assert!(p < 0.05, "genuine edge should reject the null, got p={p}");
    }

    #[test]
    fn no_edge_has_high_pvalue() {
        // All strategies are zero-mean noise ⇒ the best is best-of-N selection ⇒ high p.
        let excess: Vec<Vec<f64>> = (0..10).map(|_| ramp(0.0, 200)).collect();
        let p = reality_check_pvalue(&excess, &SpaConfig::with_defaults(), 7);
        assert!(p > 0.10, "pure noise should not reject the null, got p={p}");
    }

    #[test]
    fn pvalue_is_deterministic_per_seed() {
        let excess: Vec<Vec<f64>> = (0..5).map(|k| ramp(0.001 * k as f64, 120)).collect();
        let cfg = SpaConfig::with_defaults();
        assert_eq!(
            reality_check_pvalue(&excess, &cfg, 99),
            reality_check_pvalue(&excess, &cfg, 99)
        );
    }

    #[test]
    fn empty_or_short_input_does_not_reject() {
        assert_eq!(
            reality_check_pvalue(&[], &SpaConfig::with_defaults(), 1),
            1.0
        );
        assert_eq!(
            reality_check_pvalue(&[vec![0.1]], &SpaConfig::with_defaults(), 1),
            1.0
        );
    }
}
