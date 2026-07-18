//! White's Reality Check / SPA-lower — a best-of-N data-snooping test (QE-131/D4, QE-448 — White 2000,
//! Hansen 2005).
//!
//! Given `N` strategies' per-period performance *relative to a benchmark* (`excess[k][t]`), is the best
//! one's edge real, or just the best of `N` draws from a zero-edge null? The test statistic is
//! `V = maxₖ √T·d̄ₖ`; a **stationary bootstrap** (Politis–Romano) of the recentred series gives the null
//! distribution, and the p-value is the share of bootstrap maxima that exceed `V`. A high p-value means
//! the winner is indistinguishable from data-snooping.
//!
//! # What this is, and what it is NOT (QE-448)
//!
//! This recentres **every** one of the `k` models by its own full-sample mean `d̄ₖ` (see the
//! `for k in 0..n` loop below, `resampled_mean - means[k]` — no per-model gate). That makes it:
//!
//! - **White's Reality Check** (White 2000) with raw means (`studentize=false`, the default), or
//! - **Hansen's "SPA-lower"** bound with studentised means (`studentize=true`).
//!
//! It is the **conservative / under-powered** variant. Recentring *every* model — including strategies
//! far below zero — lets those poor models contribute bootstrap fluctuation to the null max, inflating
//! the null and **raising** the p-value (power loss). This is exactly the effect Hansen (2005)
//! identified.
//!
//! It is **not** Hansen's *consistent* SPA. Hansen's defining contribution is **model-omission
//! recentring** — recentre model `k` only when `d̄ₖ ≥ −(σₖ/√T)·√(2 log log T)`, dropping the too-poor
//! models so they stop polluting the null, which **recovers power**. That `√(2 log log T)` threshold is
//! deliberately absent here. (`studentize` is Hansen's *studentised statistic*, a separate refinement —
//! not the model-omission step, and on its own it does not make this the consistent test.)
//!
//! **Follow-up (option b, QE-448):** implement the model-omission threshold for SPA-consistent to
//! recover power. Deferred because it moves the computed p-value (the null shrinks), which — while it
//! rides the sidecar / `RobustnessReport`, not `content_hash` — churns the gate/train fixtures and can
//! flip G1 promotion, out of proportion to this P3 clarity fix.

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
    /// Studentise each strategy's mean by its std (`1/σₖ`, Hansen's studentised statistic) vs raw
    /// means (`sₖ=1`, White's RC). NB (QE-448): studentising alone does **not** make this Hansen's
    /// *consistent* SPA — both settings recentre **all** `k` models, i.e. White's RC (raw) / SPA-lower
    /// (studentised). The power-recovering model-omission recentring is not applied either way.
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
/// `1/σₖ` (SPA-lower). **Every** model is recentred by its full-sample mean `d̄ₖ` (White's Reality
/// Check / SPA-lower — the conservative variant; see the module doc for the omitted Hansen recentring).
/// Returns `1.0` for an empty/too-short input (no evidence against the null).
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
    fn all_models_recentred_is_conservative_spa_lower() {
        // QE-448 semantics guard: this test recentres EVERY model by its own mean (White's RC /
        // SPA-lower), so an irrelevant high-variance zero-mean decoy still pollutes the null max and
        // COSTS power — the p-value must not fall when the decoy is added (in practice it rises). Under
        // Hansen's *consistent* SPA the decoy would be thresholded out and could not inflate the null.
        let strong = ramp(0.02, 200); // genuine +2%/period edge
        let mut base = vec![strong.clone()];
        for k in 0..9 {
            base.push(
                ramp(0.0, 200)
                    .iter()
                    .map(|x| x + 0.0001 * k as f64)
                    .collect(),
            );
        }
        let cfg = SpaConfig::with_defaults();
        let p_base = reality_check_pvalue(&base, &cfg, 123);

        // A mean-≈0 but very high-variance decoy: (i%5 - 2) averages to 0 over full windows.
        let decoy: Vec<f64> = (0..200).map(|i| 0.5 * ((i % 5) as f64 - 2.0)).collect();
        let mut with_decoy = base.clone();
        with_decoy.push(decoy);
        let p_decoy = reality_check_pvalue(&with_decoy, &cfg, 123);

        assert!(
            p_decoy >= p_base,
            "recentring all models must not gain power from an irrelevant decoy (SPA-lower): \
             p_base={p_base}, p_decoy={p_decoy}"
        );
        assert!(
            p_decoy > p_base,
            "a high-variance zero-mean decoy should inflate the SPA-lower null and raise p: \
             p_base={p_base}, p_decoy={p_decoy}"
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
