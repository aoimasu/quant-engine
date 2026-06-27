//! Pure `f64` statistics primitives for the robustness suite (QE-131/D1).
//!
//! No reusable equivalents exist in the workspace (the signal indicators are `Decimal`-based rolling
//! windows), so the suite carries its own moment/Sharpe/normal helpers. All are deterministic and pure.

/// The Euler–Mascheroni constant γ — used by the deflated-Sharpe expected-maximum (QE-131/D2).
pub const EULER_MASCHERONI: f64 = 0.577_215_664_901_532_9;

/// Arithmetic mean of `xs` (`0.0` for an empty slice).
#[must_use]
pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Variance of `xs` with `ddof` delta degrees of freedom (`ddof = 1` ⇒ sample variance). Returns `0.0`
/// when there are not enough observations (`len <= ddof`).
#[must_use]
pub fn variance(xs: &[f64], ddof: usize) -> f64 {
    if xs.len() <= ddof {
        return 0.0;
    }
    let m = mean(xs);
    let ss: f64 = xs.iter().map(|x| (x - m).powi(2)).sum();
    ss / (xs.len() - ddof) as f64
}

/// Sample standard deviation (`ddof = 1`).
#[must_use]
pub fn std_dev(xs: &[f64]) -> f64 {
    variance(xs, 1).sqrt()
}

/// Population skewness `m3 / m2^{3/2}` (`0.0` if dispersionless or too short).
#[must_use]
pub fn skewness(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 3 {
        return 0.0;
    }
    let m = mean(xs);
    let m2 = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n as f64;
    if m2 <= 0.0 {
        return 0.0;
    }
    let m3 = xs.iter().map(|x| (x - m).powi(3)).sum::<f64>() / n as f64;
    m3 / m2.powf(1.5)
}

/// Population **non-excess** kurtosis `m4 / m2^2` (a normal distribution ⇒ `3.0`; `3.0` if dispersionless
/// or too short).
#[must_use]
pub fn kurtosis(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 4 {
        return 3.0;
    }
    let m = mean(xs);
    let m2 = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n as f64;
    if m2 <= 0.0 {
        return 3.0;
    }
    let m4 = xs.iter().map(|x| (x - m).powi(4)).sum::<f64>() / n as f64;
    m4 / (m2 * m2)
}

/// Per-period Sharpe ratio `mean / sample-std` (`0.0` if there is no dispersion). Not annualised — the
/// deflation works in the same per-period units throughout.
#[must_use]
pub fn sharpe_ratio(returns: &[f64]) -> f64 {
    let sd = std_dev(returns);
    if sd <= 0.0 {
        return 0.0;
    }
    mean(returns) / sd
}

/// The error function via Abramowitz & Stegun 7.1.26 (max abs error ≈ 1.5e-7).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    sign * y
}

/// Standard-normal CDF `Φ(x)`.
#[must_use]
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Standard-normal inverse CDF `Φ⁻¹(p)` (Acklam's rational approximation, abs error ≈ 1.15e-9 in the
/// central region). Clamps `p` to the open interval `(0, 1)`; returns `±∞` at the bounds.
#[must_use]
pub fn normal_ppf(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    // Coefficients for Acklam's algorithm.
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239e0,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
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
    const P_LOW: f64 = 0.024_25;
    const P_HIGH: f64 = 1.0 - P_LOW;

    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= P_HIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} !~ {b} (tol {tol})");
    }

    #[test]
    fn normal_cdf_and_ppf_are_consistent() {
        approx(normal_cdf(0.0), 0.5, 1e-9);
        approx(normal_cdf(1.96), 0.975, 1e-3);
        approx(normal_cdf(-1.96), 0.025, 1e-3);
        // ppf inverts cdf.
        approx(normal_ppf(0.5), 0.0, 1e-6);
        approx(normal_ppf(0.975), 1.96, 1e-3);
        approx(normal_cdf(normal_ppf(0.83)), 0.83, 1e-6);
    }

    #[test]
    fn moments_match_known_shapes() {
        // Symmetric set ⇒ ~zero skew, ~normal-ish kurtosis is not asserted tightly (small n).
        let sym = [-2.0, -1.0, 0.0, 1.0, 2.0];
        approx(skewness(&sym), 0.0, 1e-9);
        // Right-skewed set ⇒ positive skew.
        let right = [0.0, 0.0, 0.0, 0.0, 10.0];
        assert!(skewness(&right) > 0.0);
        approx(mean(&[1.0, 2.0, 3.0]), 2.0, 1e-12);
        approx(std_dev(&[1.0, 2.0, 3.0]), 1.0, 1e-12); // sample std of {1,2,3}
    }

    #[test]
    fn sharpe_is_mean_over_std() {
        let r = [0.01, 0.02, 0.015, 0.005, 0.012];
        approx(sharpe_ratio(&r), mean(&r) / std_dev(&r), 1e-12);
        // No dispersion ⇒ 0 (guard, not +inf).
        approx(sharpe_ratio(&[0.01, 0.01, 0.01]), 0.0, 1e-12);
    }
}
