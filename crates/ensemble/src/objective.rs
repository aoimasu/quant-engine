//! Tail-aware, correlation-penalised ensemble objective (QE-115).
//!
//! The portfolio objective rewards mean growth, penalises the left **tail** (CVaR/CDaR on the combined
//! net-of-cost return series, optionally with a synthetic stress overlay), and explicitly penalises
//! **return correlation** between members — because behavioural diversity (QE-111 descriptors) is *not*
//! return-decorrelation. All math is self-contained `f64`: `qe-ensemble` does **not** depend on `qe-wfo`
//! (search ⟂ portfolio firewall, QE-001/QE-132).

/// Default left-tail fraction for CVaR/CDaR (worst 5%).
pub const DEFAULT_ALPHA: f64 = 0.05;

/// A tail-risk estimate plus the number of tail observations it averaged over — the **standard-error
/// caveat** made explicit (QE-115/D3): a value from few `tail_n` points is noisy and must be
/// down-weighted by the caller (QE-126).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TailRisk {
    /// The tail average (a negative number for a loss tail).
    pub value: f64,
    /// Number of observations in the tail (`⌈alpha·n⌉`).
    pub tail_n: usize,
}

/// Pearson correlation of two equal-length series. Returns `0.0` if the lengths differ, are empty, or
/// either series has zero variance (a flat series is treated as uncorrelated).
#[must_use]
pub fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len();
    if n == 0 || n != b.len() {
        return 0.0;
    }
    let na = n as f64;
    let mean_a = a.iter().sum::<f64>() / na;
    let mean_b = b.iter().sum::<f64>() / na;
    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b.iter()) {
        let (dx, dy) = (x - mean_a, y - mean_b);
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }
    if var_a <= 0.0 || var_b <= 0.0 {
        return 0.0;
    }
    cov / (var_a.sqrt() * var_b.sqrt())
}

/// The mean over all member pairs of `max(pearson, 0)` (QE-115/D5). Negative correlation is a
/// diversification *benefit*, so it is floored at 0 and never reduces the penalty below independence.
/// Fewer than two series ⇒ `0.0`.
#[must_use]
pub fn positive_mean_pairwise_corr(series: &[Vec<f64>]) -> f64 {
    let (mut sum, mut pairs) = (0.0, 0usize);
    for (i, si) in series.iter().enumerate() {
        for sj in &series[i + 1..] {
            sum += pearson(si, sj).max(0.0);
            pairs += 1;
        }
    }
    if pairs == 0 {
        0.0
    } else {
        sum / pairs as f64
    }
}

/// CVaR / Expected Shortfall at level `alpha` (QE-115/D3): the mean of the **worst `⌈alpha·n⌉`**
/// returns. Negative for a loss tail. Empty input ⇒ `{ 0.0, 0 }`.
#[must_use]
pub fn cvar(returns: &[f64], alpha: f64) -> TailRisk {
    let n = returns.len();
    if n == 0 {
        return TailRisk {
            value: 0.0,
            tail_n: 0,
        };
    }
    let alpha = alpha.clamp(f64::MIN_POSITIVE, 1.0);
    let k = ((alpha * n as f64).ceil() as usize).clamp(1, n);
    let mut sorted = returns.to_vec();
    sorted.sort_by(f64::total_cmp); // ascending — worst (most negative) first
    let value = sorted[..k].iter().sum::<f64>() / k as f64;
    TailRisk { value, tail_n: k }
}

/// CDaR (Conditional Drawdown at Risk) at level `alpha` (QE-115/D3): builds the equity curve from
/// `returns`, takes the running-max drawdown series, and returns the mean of the **worst `⌈alpha·n⌉`**
/// drawdowns (≤ 0). The drawdown analogue of [`cvar`].
#[must_use]
pub fn cdar(returns: &[f64], alpha: f64) -> TailRisk {
    if returns.is_empty() {
        return TailRisk {
            value: 0.0,
            tail_n: 0,
        };
    }
    let (mut equity, mut peak) = (1.0_f64, 1.0_f64);
    let mut drawdowns = Vec::with_capacity(returns.len());
    for &r in returns {
        equity *= 1.0 + r;
        peak = peak.max(equity);
        let dd = if peak > 0.0 {
            equity / peak - 1.0
        } else {
            -1.0
        };
        drawdowns.push(dd);
    }
    cvar(&drawdowns, alpha)
}

/// Append synthetic `shocks` to an empirical `returns` series before the tail is taken (QE-115/D4) — so
/// CVaR/CDaR reflect plausible worst-cases the in-sample window never contained, not empirical tails
/// alone.
#[must_use]
pub fn stress_overlay(returns: &[f64], shocks: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(returns.len() + shocks.len());
    out.extend_from_slice(returns);
    out.extend_from_slice(shocks);
    out
}

/// The equal-weight combined per-period return of an ensemble's `members` (indices into `pool`),
/// truncated to the shortest member series. Empty membership ⇒ empty.
#[must_use]
pub fn combined_returns(pool: &[Vec<f64>], members: &[usize]) -> Vec<f64> {
    if members.is_empty() {
        return Vec::new();
    }
    let len = members.iter().map(|&m| pool[m].len()).min().unwrap_or(0);
    let mut out = vec![0.0; len];
    for &m in members {
        for (slot, v) in out.iter_mut().zip(pool[m].iter()) {
            *slot += v;
        }
    }
    let k = members.len() as f64;
    for slot in &mut out {
        *slot /= k;
    }
    out
}

/// Configuration for the ensemble [`objective`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectiveConfig {
    /// Left-tail fraction for CVaR.
    pub alpha: f64,
    /// Weight on the (negative) CVaR term — higher ⇒ more tail-averse.
    pub tail_weight: f64,
    /// Weight on the positive-mean-pairwise-correlation penalty.
    pub corr_weight: f64,
}

impl ObjectiveConfig {
    /// The QE-115 defaults: `alpha = 0.05`, unit tail and correlation weights.
    #[must_use]
    pub fn with_defaults() -> Self {
        ObjectiveConfig {
            alpha: DEFAULT_ALPHA,
            tail_weight: 1.0,
            corr_weight: 1.0,
        }
    }
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        ObjectiveConfig::with_defaults()
    }
}

/// The ensemble objective (QE-115/D3+D5): `mean(combined) + tail_weight·CVaR(combined) −
/// corr_weight·positive_mean_pairwise_corr(members)`, on the **net-of-cost** member return series. An
/// empty ensemble scores `−∞`.
#[must_use]
pub fn objective(pool: &[Vec<f64>], members: &[usize], cfg: &ObjectiveConfig) -> f64 {
    if members.is_empty() {
        return f64::NEG_INFINITY;
    }
    let combined = combined_returns(pool, members);
    let mean = if combined.is_empty() {
        0.0
    } else {
        combined.iter().sum::<f64>() / combined.len() as f64
    };
    let tail = cvar(&combined, cfg.alpha).value;
    let member_series: Vec<Vec<f64>> = members.iter().map(|&m| pool[m].clone()).collect();
    let corr = positive_mean_pairwise_corr(&member_series);
    mean + cfg.tail_weight * tail - cfg.corr_weight * corr
}

/// The worst single-member-removed objective (QE-115/D6 wide-basin floor): an ensemble that depends on
/// one lucky strategy scores a low leave-one-out minimum. Single-member ensembles return their own
/// objective.
#[must_use]
pub fn leave_one_out_min(pool: &[Vec<f64>], members: &[usize], cfg: &ObjectiveConfig) -> f64 {
    if members.len() <= 1 {
        return objective(pool, members, cfg);
    }
    let mut worst = f64::INFINITY;
    for drop in 0..members.len() {
        let reduced: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != drop)
            .map(|(_, &m)| m)
            .collect();
        worst = worst.min(objective(pool, &reduced, cfg));
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    #[test]
    fn pearson_basics() {
        let x = [1.0, 2.0, 3.0, 4.0];
        approx(pearson(&x, &x), 1.0);
        let neg = [4.0, 3.0, 2.0, 1.0];
        approx(pearson(&x, &neg), -1.0);
        // Flat series → zero variance → 0.
        approx(pearson(&x, &[2.0, 2.0, 2.0, 2.0]), 0.0);
        // Mismatched / empty → 0.
        approx(pearson(&x, &[1.0]), 0.0);
    }

    #[test]
    fn positive_corr_floors_negatives() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let anti = vec![4.0, 3.0, 2.0, 1.0]; // corr −1 → floored to 0
        approx(positive_mean_pairwise_corr(&[a.clone(), anti]), 0.0);
        approx(positive_mean_pairwise_corr(&[a.clone(), a]), 1.0);
        approx(positive_mean_pairwise_corr(&[vec![1.0, 2.0]]), 0.0); // <2 series
    }

    #[test]
    fn cvar_is_worst_alpha_mean() {
        let r = [-0.10, -0.05, 0.0, 0.02, 0.03];
        let t = cvar(&r, 0.4); // k = ceil(0.4·5) = 2 → worst two = {−0.10, −0.05}
        approx(t.value, -0.075);
        assert_eq!(t.tail_n, 2);
        // CVaR ≤ mean always.
        let mean = r.iter().sum::<f64>() / 5.0;
        assert!(t.value <= mean);
        assert_eq!(cvar(&[], 0.05).tail_n, 0);
    }

    #[test]
    fn cdar_on_known_path() {
        // equity: 1.1, 0.88, 0.924; peak 1.1 → drawdowns 0, −0.2, −0.16.
        let t = cdar(&[0.1, -0.2, 0.05], 0.5); // k = ceil(0.5·3) = 2 → worst {−0.2, −0.16}
        approx(t.value, (-0.2 + -0.16) / 2.0);
        assert!(t.value < 0.0);
    }

    #[test]
    fn stress_overlay_worsens_the_tail() {
        let empirical = [0.01, -0.01, 0.02, -0.02, 0.0];
        let base = cvar(&empirical, 0.2).value;
        let stressed = stress_overlay(&empirical, &[-0.5]); // a gap shock
        let with_shock = cvar(&stressed, 0.2).value;
        assert!(with_shock < base, "stress overlay must worsen the tail");
    }

    #[test]
    fn correlated_strategies_are_penalised_despite_behavioural_difference() {
        // The AC. A and B are (notionally) behaviourally distinct genomes but produce identical P&L
        // (corr 1); C is uncorrelated with A at a similar return scale. The objective must prefer the
        // decorrelated pair {A,C} over the correlated pair {A,B}.
        let a = vec![0.02, -0.01, 0.03, -0.02, 0.01, -0.03, 0.02, -0.01];
        let b = a.clone(); // return-identical to A despite "behavioural difference"
        let c = vec![-0.01, 0.02, -0.02, 0.03, -0.03, 0.01, -0.01, 0.02];
        let pool = vec![a, b, c];
        let cfg = ObjectiveConfig::with_defaults();

        let corr_ab = positive_mean_pairwise_corr(&[pool[0].clone(), pool[1].clone()]);
        let corr_ac = positive_mean_pairwise_corr(&[pool[0].clone(), pool[2].clone()]);
        approx(corr_ab, 1.0); // identical P&L
        assert!(corr_ac < corr_ab, "A/C must be less correlated than A/B");

        let obj_ab = objective(&pool, &[0, 1], &cfg);
        let obj_ac = objective(&pool, &[0, 2], &cfg);
        assert!(
            obj_ac > obj_ab,
            "decorrelated ensemble must beat the P&L-correlated one ({obj_ac} !> {obj_ab})"
        );
    }

    #[test]
    fn leave_one_out_min_flags_single_strategy_dependence() {
        // An ensemble carried by one strong strategy: dropping the strong member is the binding
        // (worst) case, so the leave-one-out minimum is set by losing it.
        let strong = vec![0.05, 0.04, 0.06, 0.05];
        let weak = vec![-0.04, -0.05, -0.03, -0.04];
        let pool = vec![strong, weak];
        let cfg = ObjectiveConfig::with_defaults();
        let loo = leave_one_out_min(&pool, &[0, 1], &cfg);
        // Dropping the strong member leaves the weak one (the worst drop); dropping the weak member
        // leaves the strong one (a better outcome). The LOO floor is the weak-only objective.
        approx(loo, objective(&pool, &[1], &cfg));
        assert!(
            loo < objective(&pool, &[0], &cfg),
            "LOO-min must be set by losing the strong member"
        );
        // A single-member ensemble's LOO-min is just its own objective.
        approx(
            leave_one_out_min(&pool, &[0], &cfg),
            objective(&pool, &[0], &cfg),
        );
    }
}
