//! Benchmark / null comparison series (QE-131/D5).
//!
//! Two reference nulls a vintage's edge must beat to be evidence of skill: **BTC-HODL** (buy-and-hold the
//! benchmark) and a **turnover-matched random-entry** strategy (random long/flat positions trading at the
//! same frequency, capturing only what random timing at that turnover earns).

use qe_determinism::task_rng;
use rand_core::RngCore;

/// Buy-and-hold (HODL) per-period simple returns of a benchmark `prices` series: `pₜ/pₜ₋₁ − 1`. Empty or
/// single-point input ⇒ empty. Non-positive prices yield a `0.0` step (guarded, no div-by-zero).
#[must_use]
pub fn buy_and_hold_returns(prices: &[f64]) -> Vec<f64> {
    if prices.len() < 2 {
        return Vec::new();
    }
    prices
        .windows(2)
        .map(|w| if w[0] > 0.0 { w[1] / w[0] - 1.0 } else { 0.0 })
        .collect()
}

/// A turnover-matched random-entry null's per-period returns: a random long(`1`)/flat(`0`) position whose
/// expected switch frequency equals `target_turnover ∈ [0, 1]`, earning the corresponding
/// `market_returns[t]` while long. Deterministic via `task_rng(seed, ·)` (QE-006).
///
/// The position flips with probability `target_turnover` each step (so the realised fraction of changed
/// periods ≈ `target_turnover`), capturing only the market exposure random timing at that turnover gives.
#[must_use]
pub fn random_entry_returns(market_returns: &[f64], target_turnover: f64, seed: u64) -> Vec<f64> {
    let p = target_turnover.clamp(0.0, 1.0);
    let mut rng = task_rng(seed, 0);
    let mut long = uniform01(&mut rng) < 0.5; // random initial state
    let mut out = Vec::with_capacity(market_returns.len());
    for &r in market_returns {
        if uniform01(&mut rng) < p {
            long = !long; // flip ⇒ a position change this period
        }
        out.push(if long { r } else { 0.0 });
    }
    out
}

/// **Label-shuffle null** (QE-451 Phase 1b, design §5 κ-calibration row): a seeded Fisher–Yates
/// permutation of `returns`, destroying any signal→forward-return alignment while preserving the return
/// **marginal** (same values, same mean/variance/higher moments). A formula's edge on shuffled labels is
/// pure selection noise, so the deflation basis is *calibrated* against it — a champion selected over a
/// shuffled population must show DSR ≈ 0.5 once the trial count reflects how hard the search rummaged.
///
/// Deterministic via `task_rng(seed, ·)` (QE-006); the same `seed` reproduces the same permutation.
#[must_use]
pub fn label_shuffle_returns(returns: &[f64], seed: u64) -> Vec<f64> {
    let mut out = returns.to_vec();
    let mut rng = task_rng(seed, 0);
    // Fisher–Yates from the top; `next_u64 % (i+1)` is a portable, deterministic index draw.
    for i in (1..out.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        out.swap(i, j);
    }
    out
}

/// **Moving-block-bootstrap null** (QE-451 Phase 1b): resample `returns` in overlapping contiguous blocks
/// of length `block_len`, preserving short-range autocorrelation (which a plain shuffle destroys) while
/// still breaking the long-range structure any edge would rest on. The output has the same length as the
/// input: `⌈T/block_len⌉` blocks are drawn (each starting at a uniform in-range index) and concatenated,
/// then truncated to `T`. `block_len == 0` or `≥ T` degenerates to a single whole-series draw.
///
/// Deterministic via `task_rng(seed, ·)` (QE-006).
#[must_use]
pub fn block_bootstrap_returns(returns: &[f64], block_len: usize, seed: u64) -> Vec<f64> {
    let t = returns.len();
    if t == 0 {
        return Vec::new();
    }
    let block = block_len.clamp(1, t);
    let n_starts = t.saturating_sub(block) + 1; // number of valid block start positions
    let mut rng = task_rng(seed, 0);
    let mut out = Vec::with_capacity(t);
    while out.len() < t {
        let start = (rng.next_u64() % n_starts as u64) as usize;
        for &r in &returns[start..start + block] {
            out.push(r);
            if out.len() == t {
                break;
            }
        }
    }
    out
}

/// The realised turnover (fraction of periods whose position changed) of a position series — a test/audit
/// helper to confirm a random-entry null matches its target.
#[must_use]
pub fn realised_turnover(positions: &[bool]) -> f64 {
    if positions.len() < 2 {
        return 0.0;
    }
    let changes = positions.windows(2).filter(|w| w[0] != w[1]).count();
    changes as f64 / (positions.len() - 1) as f64
}

/// A uniform draw in `[0, 1)` from 53 random bits (QE-006 convention).
fn uniform01<R: RngCore>(rng: &mut R) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} !~ {b}");
    }

    #[test]
    fn hodl_returns_match_hand_computation() {
        let prices = [100.0, 110.0, 99.0];
        let r = buy_and_hold_returns(&prices);
        approx(r[0], 0.10, 1e-12); // +10%
        approx(r[1], -0.10, 1e-12); // −10%
        assert!(buy_and_hold_returns(&[100.0]).is_empty());
    }

    #[test]
    fn random_entry_is_deterministic_per_seed() {
        let market: Vec<f64> = (0..200).map(|i| 0.001 * ((i % 7) as f64 - 3.0)).collect();
        let a = random_entry_returns(&market, 0.2, 5);
        let b = random_entry_returns(&market, 0.2, 5);
        assert_eq!(a, b);
        // A different seed gives a different path.
        assert_ne!(a, random_entry_returns(&market, 0.2, 6));
    }

    #[test]
    fn label_shuffle_is_a_seeded_permutation_preserving_the_marginal() {
        let returns: Vec<f64> = (0..200).map(|i| 0.001 * ((i % 13) as f64 - 6.0)).collect();
        let a = label_shuffle_returns(&returns, 42);
        let b = label_shuffle_returns(&returns, 42);
        assert_eq!(a, b, "same seed reproduces the permutation");
        assert_eq!(a.len(), returns.len());
        // It is a permutation: same multiset ⇒ identical sum and sum-of-squares (marginal preserved).
        let sum = |v: &[f64]| v.iter().sum::<f64>();
        let sq = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>();
        approx(sum(&a), sum(&returns), 1e-9);
        approx(sq(&a), sq(&returns), 1e-9);
        // A different seed gives a different order (with overwhelming probability at T=200).
        assert_ne!(a, label_shuffle_returns(&returns, 43));
        // The order actually changed (not the identity permutation).
        assert_ne!(a, returns);
    }

    #[test]
    fn block_bootstrap_is_seeded_and_length_preserving() {
        let returns: Vec<f64> = (0..100).map(|i| 0.01 * (i as f64)).collect();
        let a = block_bootstrap_returns(&returns, 10, 7);
        let b = block_bootstrap_returns(&returns, 10, 7);
        assert_eq!(a, b, "same seed reproduces the resample");
        assert_eq!(a.len(), returns.len(), "length preserved");
        // Every value is drawn from the input population.
        assert!(a.iter().all(|x| returns.contains(x)));
        // A ramp series: within-block consecutive differences are +0.01 (autocorrelation preserved),
        // which a plain shuffle would not keep — count the fraction of +0.01 steps and require it high.
        let consec = a
            .windows(2)
            .filter(|w| (w[1] - w[0] - 0.01).abs() < 1e-9)
            .count();
        assert!(
            consec as f64 / (a.len() - 1) as f64 > 0.7,
            "moving blocks must preserve short-range structure: {consec}/{}",
            a.len() - 1
        );
        // Degenerate block sizes fall back to a whole-series draw of the right length.
        assert_eq!(block_bootstrap_returns(&returns, 0, 1).len(), returns.len());
        assert_eq!(
            block_bootstrap_returns(&returns, 999, 1).len(),
            returns.len()
        );
        assert!(block_bootstrap_returns(&[], 5, 1).is_empty());
    }

    #[test]
    fn random_entry_turnover_tracks_target() {
        // Reconstruct the position series with the same RNG stream and check realised ≈ target.
        let market: Vec<f64> = (0..5_000).map(|i| 0.001 * ((i % 3) as f64)).collect();
        let target = 0.3;
        // Rebuild positions deterministically the same way random_entry_returns does.
        let mut rng = task_rng(11, 0);
        let mut long = uniform01(&mut rng) < 0.5;
        let mut positions = Vec::with_capacity(market.len());
        for _ in &market {
            if uniform01(&mut rng) < target {
                long = !long;
            }
            positions.push(long);
        }
        let realised = realised_turnover(&positions);
        approx(realised, target, 0.03);
        // sanity: the public function ran on the same seed produces returns of the right length.
        assert_eq!(
            random_entry_returns(&market, target, 11).len(),
            market.len()
        );
    }
}
