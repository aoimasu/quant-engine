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
