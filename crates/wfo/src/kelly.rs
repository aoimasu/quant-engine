//! Portfolio-level empirical Kelly sizing (QE-433) — the growth-optimal leverage `f*` on a **realised
//! combined net-of-cost** return series, and its fractional (≤½) form.
//!
//! maxdama §6.3 step 2: after the ensemble mask + capacity weights are fixed, the book is a *single*
//! strategy whose realised per-period return is the capacity-weighted combined net series. The
//! growth-optimal leverage on that book is `f* = argmax_f mean ln(1 + f·r_combined)` — the same
//! ergodic [`log_growth`](crate::log_growth) the search already optimises, now maximised **over the
//! leverage scalar** rather than over genomes. Because it reads the realised **joint** path it estimates
//! **no covariance**, so it is correlation-robust by construction (§6.4) and sidesteps QE-430's
//! estimation problem: positively-correlated members inflate the combined variance directly, pulling `f*`
//! down without ever forming a correlation matrix.
//!
//! A **fractional** multiplier `κ ∈ [0.3, 0.5]` (§6.5 half-Kelly robustness) shrinks `f*` for the
//! parameter-uncertainty and fat-tail reasons the book documents. The result is deployed as an
//! **advisory** leverage factor clamped **below** the pretrade cap (QE-215) — it can **cut** as readily
//! as raise, and the hard cap remains the backstop.

use crate::fitness::log_growth;

/// Upper end of the fractional-Kelly range `κ ∈ [0.3, 0.5]` — the canonical **half-Kelly** (§6.5).
pub const DEFAULT_KELLY_FRACTION: f64 = 0.5;
/// Lower end of the fractional-Kelly range `κ ∈ [0.3, 0.5]`.
pub const MIN_KELLY_FRACTION: f64 = 0.3;

/// Feasibility back-off from the ruin boundary: the search upper bound is `(1/|min r|)·(1 − ε)` so the
/// worst realised loss keeps `1 + f·r > 0` (a levered wipe-out is `−∞`, the absorbing ruin state).
const RUIN_BACKOFF: f64 = 1e-9;

/// Search ceiling when the series **never loses** (no ruin boundary exists). Purely defensive — a real
/// combined net series always contains losing periods; the fractional `κ` and the pretrade cap bound any
/// deployed size regardless.
const MAX_LEVERAGE_SEARCH: f64 = 100.0;

/// Golden-section iterations — far more than the ~60 needed to converge `[0, hi]` to `f64` precision.
const GOLDEN_ITERS: usize = 200;

/// The **empirical Kelly** leverage `f* ≥ 0` on a realised net-of-cost return series:
/// `argmax_{f ≥ 0} mean ln(1 + f·r)` (maxdama §6.4 — "empirical", because it is solved on the realised
/// path, not a fitted distribution).
///
/// Reuses [`log_growth`](crate::log_growth) as the objective (the levered series `f·r`), so a levered
/// wipe-out is `−∞` and can never be chosen. The objective is concave in `f` on the feasible interval, so
/// golden-section search finds the interior maximum; a non-positive-drift series optimises at `f → 0` and
/// is returned as exactly `0` (Kelly of a non-edge is no bet). Empty input ⇒ `0`.
#[must_use]
pub fn empirical_kelly(returns: &[f64]) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let min_r = returns.iter().copied().fold(f64::INFINITY, f64::min);
    // Feasible upper bound on f: the worst loss must keep 1 + f·r > 0.
    let hi = if min_r < 0.0 {
        (1.0 / -min_r) * (1.0 - RUIN_BACKOFF)
    } else {
        MAX_LEVERAGE_SEARCH
    };
    if hi <= 0.0 {
        return 0.0;
    }
    maximise_log_growth(returns, hi)
}

/// The **fractional** empirical Kelly `κ·f*` with `κ` clamped into `[0.3, 0.5]` (§6.5) — the advisory
/// leverage factor sealed into the vintage. `κ` outside the range is clamped (not rejected) so a caller
/// cannot accidentally deploy full or super-Kelly leverage.
#[must_use]
pub fn fractional_kelly(returns: &[f64], kappa: f64) -> f64 {
    let k = kappa.clamp(MIN_KELLY_FRACTION, DEFAULT_KELLY_FRACTION);
    k * empirical_kelly(returns)
}

/// Mean log-growth of the series levered by `f` — `log_growth(f·r)`, reusing the search's own fitness.
fn objective(returns: &[f64], f: f64) -> f64 {
    let scaled: Vec<f64> = returns.iter().map(|&r| f * r).collect();
    log_growth(&scaled)
}

/// Golden-section maximisation of the concave [`objective`] on `[0, hi]`. Deterministic (fixed iteration
/// count, no RNG), so a fixed input yields a byte-identical result. Returns `0` whenever no positive
/// leverage beats sitting flat (non-positive drift).
fn maximise_log_growth(returns: &[f64], hi: f64) -> f64 {
    // 1/φ = (√5 − 1)/2 ≈ 0.618 — the golden ratio's reciprocal.
    let inv_phi = (5.0_f64.sqrt() - 1.0) / 2.0;
    let (mut a, mut b) = (0.0_f64, hi);
    let mut c = b - inv_phi * (b - a);
    let mut d = a + inv_phi * (b - a);
    let mut fc = objective(returns, c);
    let mut fd = objective(returns, d);
    for _ in 0..GOLDEN_ITERS {
        if fc < fd {
            // Maximum is in [c, b].
            a = c;
            c = d;
            fc = fd;
            d = a + inv_phi * (b - a);
            fd = objective(returns, d);
        } else {
            // Maximum is in [a, d].
            b = d;
            d = c;
            fd = fc;
            c = b - inv_phi * (b - a);
            fc = objective(returns, c);
        }
        if (b - a).abs() < 1e-12 {
            break;
        }
    }
    let f = 0.5 * (a + b);
    // Kelly is non-negative; never return a positive f that is worse than sitting flat.
    if f > 0.0 && objective(returns, f) > objective(returns, 0.0) {
        f
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} !~ {b} (tol {tol})");
    }

    /// A deterministic xorshift `[−0.5, 0.5)` stream (no `rand` dep, byte-stable) — mirrors the helper the
    /// ensemble objective tests use, for building correlated/independent members.
    fn xorshift(mut state: u64) -> impl FnMut() -> f64 {
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        }
    }

    /// The solved f keeps the levered series feasible (finite log-growth — never the ruin `−∞`), for a
    /// spread of series including a deep single loss.
    #[test]
    fn respects_ruin_feasibility() {
        for series in [
            vec![0.05, -0.10, 0.04, -0.02, 0.06, -0.08, 0.05],
            vec![0.2, -0.5, 0.3, -0.1],
            vec![0.01, 0.02, -0.9, 0.03], // a near-wipe single bar
        ] {
            let f = empirical_kelly(&series);
            assert!(f >= 0.0, "kelly is non-negative, got {f}");
            let scaled: Vec<f64> = series.iter().map(|&r| f * r).collect();
            assert!(
                log_growth(&scaled).is_finite(),
                "solved f={f} must keep the levered series feasible"
            );
        }
    }

    /// A non-positive-drift series is not bet at all: f* = 0.
    #[test]
    fn no_edge_is_zero() {
        assert_eq!(empirical_kelly(&[]), 0.0);
        // Symmetric round-trip: geometric drift is negative ⇒ no positive leverage helps.
        assert_eq!(empirical_kelly(&[0.1, -0.1, 0.1, -0.1]), 0.0);
        // Strictly losing.
        assert_eq!(empirical_kelly(&[-0.01, -0.02, -0.03]), 0.0);
    }

    /// A positive edge is levered up (f* > 0), and known small-edge Kelly is near the closed-form
    /// `≈ μ/σ²` for a symmetric two-outcome bet.
    #[test]
    fn positive_edge_is_levered() {
        // A favourable coin: +0.06 / −0.05 with equal frequency. Positive geometric drift ⇒ f* > 0.
        let r = [0.06, -0.05, 0.06, -0.05, 0.06, -0.05];
        let f = empirical_kelly(&r);
        assert!(f > 0.0, "a positive-edge series must be levered, got {f}");
    }

    /// AC1 (solver root): fractional Kelly on a **fat-left-tail** combined series comes out **below** the
    /// size for a thin-tail series of the **same mean** — the sizer cuts on tail risk (§6.4). And the
    /// fractional (×κ) size is strictly below full Kelly.
    #[test]
    fn cuts_on_fat_left_tail() {
        // Two series with the SAME arithmetic mean (0.01) but different left tails.
        // Thin tail: small symmetric moves around the mean.
        let thin = vec![0.03, -0.01, 0.03, -0.01, 0.03, -0.01];
        // Fat left tail: same mean, but one deep loss the thin series never takes.
        let fat = vec![0.05, 0.05, 0.05, 0.05, 0.05, -0.19];
        approx(
            thin.iter().sum::<f64>() / thin.len() as f64,
            fat.iter().sum::<f64>() / fat.len() as f64,
            1e-12,
        );

        let f_thin = empirical_kelly(&thin);
        let f_fat = empirical_kelly(&fat);
        assert!(
            f_fat < f_thin,
            "the fat-left-tail series must be sized below the thin-tail one: fat={f_fat} thin={f_thin}"
        );

        // The fractional multiplier cuts full Kelly further (κ = 0.5 ⇒ exactly half).
        let full = empirical_kelly(&fat);
        let frac = fractional_kelly(&fat, DEFAULT_KELLY_FRACTION);
        approx(frac, 0.5 * full, 1e-9);
        assert!(
            frac < full || full == 0.0,
            "fractional Kelly must not exceed full Kelly"
        );
    }

    /// AC2: two **positively-correlated** members are **down-weighted** vs summing standalone Kellys — the
    /// portfolio Kelly on the joint path is strictly below `kelly(A)+kelly(B)`, while for **independent**
    /// members of the same marginals it is ≈ the sum. Isolates correlation (not the averaging) as the
    /// cause.
    #[test]
    fn positively_correlated_downweighted_vs_summed_standalone() {
        // Build a base positive-edge stream and a small idiosyncratic stream.
        let mut g = xorshift(0xDEADBEEF);
        let n = 400;
        let base: Vec<f64> = (0..n).map(|_| 0.01 + 0.04 * g()).collect();
        let idio_b: Vec<f64> = (0..n).map(|_| 0.01 + 0.04 * g()).collect(); // independent of base
        let idio_c: Vec<f64> = (0..n).map(|_| 0.01 + 0.04 * g()).collect();

        // A is `base`. B is highly correlated with A (mostly base + a little idiosyncratic noise).
        let a: Vec<f64> = base.clone();
        let b_corr: Vec<f64> = base
            .iter()
            .zip(&idio_b)
            .map(|(&x, &e)| 0.85 * x + 0.15 * e)
            .collect();
        // C is an INDEPENDENT member with the same marginal construction as B (same weights, but on an
        // independent base so it does not track A).
        let indep_base: Vec<f64> = (0..n).map(|_| 0.01 + 0.04 * g()).collect();
        let c_indep: Vec<f64> = indep_base
            .iter()
            .zip(&idio_c)
            .map(|(&x, &e)| 0.85 * x + 0.15 * e)
            .collect();

        // Average books (weights fixed, as after the capacity pass): the realised joint path.
        let combined_corr: Vec<f64> = a
            .iter()
            .zip(&b_corr)
            .map(|(&x, &y)| 0.5 * x + 0.5 * y)
            .collect();
        let combined_indep: Vec<f64> = a
            .iter()
            .zip(&c_indep)
            .map(|(&x, &y)| 0.5 * x + 0.5 * y)
            .collect();

        let kelly_a = empirical_kelly(&a);
        let kelly_b = empirical_kelly(&b_corr);
        let kelly_c = empirical_kelly(&c_indep);
        let summed_corr = kelly_a + kelly_b;
        let summed_indep = kelly_a + kelly_c;

        let portfolio_corr = empirical_kelly(&combined_corr);
        let portfolio_indep = empirical_kelly(&combined_indep);

        // Down-weighted: the correlated pair's portfolio Kelly is strictly below the naive summed size.
        assert!(
            portfolio_corr < summed_corr,
            "positively-correlated members must be down-weighted vs summed standalone Kellys: \
             portfolio={portfolio_corr} summed={summed_corr}"
        );
        // And it is the correlation, not the averaging: the independent pair keeps far more of its summed
        // size than the correlated pair does.
        let corr_ratio = portfolio_corr / summed_corr;
        let indep_ratio = portfolio_indep / summed_indep;
        assert!(
            corr_ratio < indep_ratio,
            "positive correlation must cut a larger fraction than independence: \
             corr_ratio={corr_ratio} indep_ratio={indep_ratio}"
        );
    }

    /// `κ` is clamped into `[0.3, 0.5]` — a caller cannot deploy full or super-Kelly leverage.
    #[test]
    fn fractional_clamps_kappa() {
        let r = [0.06, -0.05, 0.06, -0.05, 0.06, -0.05];
        let full = empirical_kelly(&r);
        // Above the range clamps to 0.5.
        approx(fractional_kelly(&r, 1.0), 0.5 * full, 1e-9);
        approx(fractional_kelly(&r, 0.9), 0.5 * full, 1e-9);
        // Below the range clamps to 0.3.
        approx(fractional_kelly(&r, 0.0), 0.3 * full, 1e-9);
        // In range passes through.
        approx(fractional_kelly(&r, 0.4), 0.4 * full, 1e-9);
    }

    /// Deterministic: the same input yields a **bit-identical** result across calls (byte-reproducible
    /// sealing).
    #[test]
    fn deterministic_bit_for_bit() {
        let r = vec![0.03, -0.02, 0.05, -0.01, 0.04, -0.06, 0.02];
        assert_eq!(empirical_kelly(&r).to_bits(), empirical_kelly(&r).to_bits());
        assert_eq!(
            fractional_kelly(&r, DEFAULT_KELLY_FRACTION).to_bits(),
            fractional_kelly(&r, DEFAULT_KELLY_FRACTION).to_bits()
        );
    }
}
