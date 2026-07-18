//! Tail-aware, correlation-penalised ensemble objective (QE-115).
//!
//! The portfolio objective rewards mean growth, penalises the left **tail** (CVaR/CDaR on the combined
//! net-of-cost return series, optionally with a synthetic stress overlay), and explicitly penalises
//! **return correlation** between members — because behavioural diversity (QE-111 descriptors) is *not*
//! return-decorrelation. All math is self-contained `f64`: `qe-ensemble` does **not** depend on `qe-wfo`
//! (search ⟂ portfolio firewall, QE-001/QE-132).
//!
//! QE-438: membership is scored on the **deployed** weight vector, not equal-weight. The combined return
//! the objective sees is built with the capacity-capped weights the vintage actually deploys (reusing
//! [`weighted_combined`](crate::stress::weighted_combined) and the QE-128 [`cap_weights`](crate::capacity::cap_weights)
//! water-fill), so the DE optimises the same weighted object it deploys — reducing to equal-weight
//! exactly when no capacity cap binds.

use crate::capacity::cap_weights;
use crate::stress::weighted_combined;

/// Default left-tail fraction for CVaR/CDaR (worst 5%).
pub const DEFAULT_ALPHA: f64 = 0.05;

/// Default z-score for the significance floor — `1.96` = the two-sided 5% critical value, so `R(N)` is
/// Dama §6.2's minimum-significant-correlation curve (QE-430).
pub const DEFAULT_SIGNIFICANCE_Z: f64 = 1.96;

/// Default shrinkage strength for Fisher-z mode (QE-430) — matches the significance-floor z so the two
/// modes deflate on the same scale.
pub const DEFAULT_FISHER_LAMBDA: f64 = 1.96;

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

/// A sample-size-deflated pairwise correlation penalty plus the number of observations it rested on —
/// the correlation analogue of [`TailRisk`] (QE-430). A penalty from few `effective_n` points is noisy
/// (a raw sample Pearson over a short CV fold is spurious), so it is deflated toward zero and the
/// caller can flag the small sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorrPenalty {
    /// The deflated positive-mean pairwise correlation (`≥ 0`).
    pub value: f64,
    /// The smallest sample size any pair rested on (`0` when there are `< 2` series).
    pub effective_n: usize,
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

/// The mean over all member pairs of `max(pearson, 0)` (QE-115/D5), on the **raw** sample Pearson with
/// no sample-size deflation. Negative correlation is a diversification *benefit*, so it is floored at 0
/// and never reduces the penalty below independence. Fewer than two series ⇒ `0.0`. This is the
/// undeflated reference the [`CorrDeflation::None`] toggle reproduces (QE-430); the deflated,
/// config-aware entry point is [`pairwise_corr_penalty`].
#[must_use]
pub fn positive_mean_pairwise_corr(series: &[Vec<f64>]) -> f64 {
    pairwise_corr_penalty(series, CorrDeflation::None).value
}

/// How the pairwise correlation penalty is **deflated by sample size** before it drives the DE ensemble
/// selection (QE-430, Dama §6.2 "Spurious Correlation"). A raw sample Pearson on the ≈`t/folds` points
/// of a CV fold slice fluctuates widely; minimising it over `K(K−1)/2` pairs and many masks
/// preferentially admits members whose sample correlation dipped low **by luck** (phantom
/// diversification). Deflation neutralises that: sub-threshold correlations contribute nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CorrDeflation {
    /// No deflation — the raw sample Pearson (the reproducible A/B + golden toggle).
    None,
    /// **Significance floor.** Zero any pair with `|r| < R(N)`, where `R(N) = tanh(z/√(N−3))` is Dama's
    /// minimum-significant-r curve. A supra-threshold correlation is kept at its raw value.
    SignificanceFloor {
        /// Two-sided critical z-score (`1.96` = 5%).
        z: f64,
    },
    /// **Fisher-z shrinkage.** `z = arctanh(r)`, `z' = sign(z)·max(0, |z| − λ/√(N−3))`, `r' = tanh(z')`
    /// — a soft threshold with no cliff, `λ` configurable.
    FisherShrinkage {
        /// Shrinkage strength (larger ⇒ more deflation).
        lambda: f64,
    },
}

impl Default for CorrDeflation {
    /// The QE-430 default: the significance floor at the 5% critical value (the new behaviour, **on**).
    fn default() -> Self {
        CorrDeflation::SignificanceFloor {
            z: DEFAULT_SIGNIFICANCE_Z,
        }
    }
}

/// Dama's minimum-significant sample correlation at sample size `n`: `R(N) = tanh(z/√(N−3))`. For
/// `n ≤ 3` the standard error is undefined, so **no** correlation is distinguishable from zero and the
/// threshold is `1.0` (nothing but a perfect `±1` clears it) — but see [`pairwise_corr_penalty`], which
/// treats `n ≤ 3` as wholly insignificant.
#[must_use]
pub fn min_significant_r(n: usize, z: f64) -> f64 {
    if n <= 3 {
        return 1.0;
    }
    (z / ((n - 3) as f64).sqrt()).tanh()
}

/// Deflate a single sample correlation `r` observed over `n` points under `mode`. Returns the raw `r`
/// for [`CorrDeflation::None`]; `0.0` for a sub-threshold / degenerate-`N` pair; the shrunk value for
/// Fisher-z. `n ≤ 3` ⇒ `0.0` (the `1/√(N−3)` scale is undefined, so the correlation is not
/// distinguishable from zero).
fn deflate_r(r: f64, n: usize, mode: CorrDeflation) -> f64 {
    match mode {
        CorrDeflation::None => r,
        CorrDeflation::SignificanceFloor { z } => {
            if n <= 3 {
                return 0.0;
            }
            if r.abs() < min_significant_r(n, z) {
                0.0
            } else {
                r
            }
        }
        CorrDeflation::FisherShrinkage { lambda } => {
            if n <= 3 {
                return 0.0;
            }
            let zed = r.atanh(); // ±∞ for r = ±1 → tanh recovers ±1 below
            let shrunk = zed.signum() * (zed.abs() - lambda / ((n - 3) as f64).sqrt()).max(0.0);
            shrunk.tanh()
        }
    }
}

/// A sample-size-aware pairwise correlation penalty plus the **effective N** it rested on — the
/// standard-error caveat made explicit, mirroring how [`TailRisk`] surfaces `tail_n` (QE-430). `value`
/// is the mean over all member pairs of `max(deflate(r), 0)` (negative correlation is a diversification
/// benefit ⇒ floored to 0); `effective_n` is the **smallest** sample size any pair rested on (so the
/// score record / G1 can flag a penalty resting on a tiny sample). Fewer than two series ⇒
/// `{ 0.0, 0 }`.
#[must_use]
pub fn pairwise_corr_penalty(series: &[Vec<f64>], mode: CorrDeflation) -> CorrPenalty {
    let (mut sum, mut pairs) = (0.0, 0usize);
    let mut effective_n = usize::MAX;
    for (i, si) in series.iter().enumerate() {
        for sj in &series[i + 1..] {
            let n = si.len().min(sj.len());
            sum += deflate_r(pearson(si, sj), n, mode).max(0.0);
            pairs += 1;
            effective_n = effective_n.min(n);
        }
    }
    if pairs == 0 {
        CorrPenalty {
            value: 0.0,
            effective_n: 0,
        }
    } else {
        CorrPenalty {
            value: sum / pairs as f64,
            effective_n,
        }
    }
}

// ---- QE-451 Phase 1b: provenance-aware ensemble correlation floor + evolved-share cap (QE-450 §5) -----
//
// When an ensemble draws on **evolved** GP members, a large formula pool lets the search find
// in-sample-decorrelated pairs in noise (design §5 QE-430 row). Two additions on top of the merged
// deflated pairwise penalty ([`pairwise_corr_penalty`]) guard that:
//   1. a **leave-one-PROVENANCE-out floor** — cluster members by lineage/provenance, collapse each cluster
//      to its mean series, and take the deflated pairwise penalty over the cluster *representatives*. A
//      lineage's mutually-decorrelated-by-noise members then count **once**, so intra-lineage phantom
//      diversification cannot pull the penalty below the genuine cross-lineage correlation. The full-member
//      penalty is floored by this value.
//   2. a **cap on the evolved-formula share** — the fraction of ensemble members that are evolved.
//
// Pure `f64` on member return series + integer provenance labels — no `qe-wfo` dependency (the evolved
// members cross into `qe-ensemble` as DATA, not a firewall-crossing code edge).

/// The element-wise **mean series of each provenance cluster** (members grouped by their integer
/// provenance/lineage label), returned in ascending label order for determinism. Each cluster's series is
/// truncated to its shortest member. Clusters with no members are omitted. Empty input ⇒ empty.
#[must_use]
pub fn cluster_mean_series(series: &[Vec<f64>], provenance: &[usize]) -> Vec<Vec<f64>> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<usize, Vec<&Vec<f64>>> = BTreeMap::new();
    for (s, p) in series.iter().zip(provenance.iter()) {
        groups.entry(*p).or_default().push(s);
    }
    groups
        .into_values()
        .filter_map(|members| {
            let t = members.iter().map(|m| m.len()).min().unwrap_or(0);
            if t == 0 {
                return None;
            }
            let k = members.len() as f64;
            let mean: Vec<f64> = (0..t)
                .map(|i| members.iter().map(|m| m[i]).sum::<f64>() / k)
                .collect();
            Some(mean)
        })
        .collect()
}

/// The fraction of ensemble members that are **evolved** (design §5 QE-430 row: cap the evolved-formula
/// share of any ensemble). `is_evolved[k]` marks member `k` as a GP-evolved formula (vs a hand catalogue
/// indicator). Empty input ⇒ `0.0`.
#[must_use]
pub fn evolved_share(is_evolved: &[bool]) -> f64 {
    if is_evolved.is_empty() {
        return 0.0;
    }
    is_evolved.iter().filter(|e| **e).count() as f64 / is_evolved.len() as f64
}

/// The provenance-aware ensemble correlation diagnostics (QE-451 Phase 1b).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProvenancePenalty {
    /// The deflated pairwise correlation penalty over all members ([`pairwise_corr_penalty`]).
    pub member_penalty: f64,
    /// The **leave-one-provenance-out floor**: the deflated penalty over the per-lineage mean series.
    pub provenance_floor: f64,
    /// The binding penalty `max(member_penalty, provenance_floor)` — a lineage cannot decorrelate its way
    /// below the genuine cross-lineage correlation.
    pub binding: f64,
    /// The evolved-formula share of the ensemble.
    pub evolved_share: f64,
}

impl ProvenancePenalty {
    /// Whether the evolved share is within `max_share` (design §5: cap the evolved-formula share).
    #[must_use]
    pub fn share_within(&self, max_share: f64) -> bool {
        self.evolved_share <= max_share
    }
}

/// Compute the provenance-aware correlation penalty (QE-451 Phase 1b): the deflated member penalty, the
/// leave-one-provenance-out floor over per-lineage mean series, their binding max, and the evolved share.
/// `provenance[k]` is member `k`'s lineage/cluster label; `is_evolved[k]` marks evolved members. Reuses
/// the merged QE-430 [`pairwise_corr_penalty`] verbatim for both the member and the cluster-representative
/// penalties.
#[must_use]
pub fn provenance_aware_penalty(
    series: &[Vec<f64>],
    provenance: &[usize],
    is_evolved: &[bool],
    mode: CorrDeflation,
) -> ProvenancePenalty {
    let member_penalty = pairwise_corr_penalty(series, mode).value;
    let cluster_means = cluster_mean_series(series, provenance);
    let provenance_floor = pairwise_corr_penalty(&cluster_means, mode).value;
    ProvenancePenalty {
        member_penalty,
        provenance_floor,
        binding: member_penalty.max(provenance_floor),
        evolved_share: evolved_share(is_evolved),
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

/// How the combined per-period return is weighted from an ensemble's members **during scoring** (QE-438).
/// Membership must be optimised on the same weighted object it is deployed under; the deployed book runs
/// the QE-128 capacity-capped weights, so scoring on plain equal-weight makes the selected set no longer
/// provably optimal for the portfolio that runs (the "optimise-X-deploy-Y" gap). This descriptor is a
/// lightweight `Copy` value (a borrowed capacity slice + the target AUM) threaded into the objective; it
/// is **not** stored in the `Copy` [`ObjectiveConfig`] because the per-strategy capacities are a
/// per-pool `Vec`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Weighting<'a> {
    /// The historical `1/N` equal weighting — the reproducible pre-QE-438 scoring object.
    EqualWeight,
    /// The **deployed** capacity-capped weighting (QE-438): the equal-weight budget water-filled by each
    /// member's modelled `capacities` (indexed by **pool** position) at `target_aum`, exactly as the
    /// vintage deploys (`crates/cli/src/jobs/train.rs` `capacity_capped_weights`). Reduces to
    /// [`Weighting::EqualWeight`] when no cap binds (`cap_weights` returns the equal weights).
    CapacityCapped {
        /// Modelled per-strategy capacity in dollars, indexed by **pool** position (`capacities[m]` is
        /// the capacity of pool member `m`). A member index past the slice is treated as uncapped.
        capacities: &'a [f64],
        /// Target book AUM the caps bind against; `≤ 0` disables capping (equal weight).
        target_aum: f64,
    },
}

impl Weighting<'_> {
    /// The deployed per-member weight vector for `members` (indices into the pool): equal `1/k`, then —
    /// for [`Weighting::CapacityCapped`] — the QE-128 [`cap_weights`] water-fill over **those members'**
    /// capacities at `target_aum` (recomputed per member subset, so a leave-one-out drop re-water-fills).
    /// Empty membership ⇒ empty. When no cap binds the result is exactly `[1/k; k]`.
    #[must_use]
    pub fn member_weights(&self, members: &[usize]) -> Vec<f64> {
        let k = members.len();
        if k == 0 {
            return Vec::new();
        }
        let equal = vec![1.0 / k as f64; k];
        match *self {
            Weighting::EqualWeight => equal,
            Weighting::CapacityCapped {
                capacities,
                target_aum,
            } => {
                let caps: Vec<f64> = members
                    .iter()
                    .map(|&m| capacities.get(m).copied().unwrap_or(f64::INFINITY))
                    .collect();
                cap_weights(&equal, &caps, target_aum)
            }
        }
    }
}

/// The combined per-period return of an ensemble's `members` (indices into `pool`) under the deployed
/// `weighting` (QE-438), truncated to the shortest member series. Reuses
/// [`weighted_combined`](crate::stress::weighted_combined) on the members' series and their deployed
/// [`member_weights`](Weighting::member_weights). Empty membership ⇒ empty. Under
/// [`Weighting::EqualWeight`] this is byte-identical to the `Σ r_i / k` equal-weight combination.
#[must_use]
pub fn combined_returns_weighted(
    pool: &[Vec<f64>],
    members: &[usize],
    weighting: Weighting,
) -> Vec<f64> {
    if members.is_empty() {
        return Vec::new();
    }
    let member_series: Vec<Vec<f64>> = members.iter().map(|&m| pool[m].clone()).collect();
    let weights = weighting.member_weights(members);
    weighted_combined(&member_series, &weights)
}

/// The equal-weight combined per-period return of an ensemble's `members` (indices into `pool`),
/// truncated to the shortest member series. Empty membership ⇒ empty. Equivalent to
/// [`combined_returns_weighted`] under [`Weighting::EqualWeight`].
#[must_use]
pub fn combined_returns(pool: &[Vec<f64>], members: &[usize]) -> Vec<f64> {
    combined_returns_weighted(pool, members, Weighting::EqualWeight)
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
    /// How the correlation penalty is deflated by the (fold-slice) sample size (QE-430). Defaults to the
    /// significance floor; set [`CorrDeflation::None`] to reproduce the raw-Pearson pre-QE-430 path.
    pub corr_deflation: CorrDeflation,
}

impl ObjectiveConfig {
    /// The defaults: `alpha = 0.05`, unit tail and correlation weights (QE-115), and the sample-size
    /// significance floor **on** (QE-430).
    #[must_use]
    pub fn with_defaults() -> Self {
        ObjectiveConfig {
            alpha: DEFAULT_ALPHA,
            tail_weight: 1.0,
            corr_weight: 1.0,
            corr_deflation: CorrDeflation::default(),
        }
    }
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        ObjectiveConfig::with_defaults()
    }
}

/// The ensemble objective under the deployed `weighting` (QE-115/D3+D5, QE-438): `mean(combined) +
/// tail_weight·CVaR(combined) − corr_weight·positive_mean_pairwise_corr(members)`, where `combined` is
/// the members' **net-of-cost** returns combined under `weighting` (the capacity-capped weights the
/// vintage deploys). The correlation penalty is a weight-independent property of the member series
/// (scale-invariant), so it stays on the raw member series — QE-430's deflated penalty is untouched. An
/// empty ensemble scores `−∞`.
#[must_use]
pub fn objective_weighted(
    pool: &[Vec<f64>],
    members: &[usize],
    cfg: &ObjectiveConfig,
    weighting: Weighting,
) -> f64 {
    if members.is_empty() {
        return f64::NEG_INFINITY;
    }
    let combined = combined_returns_weighted(pool, members, weighting);
    let mean = if combined.is_empty() {
        0.0
    } else {
        combined.iter().sum::<f64>() / combined.len() as f64
    };
    let tail = cvar(&combined, cfg.alpha).value;
    // The member series are already sliced to the fold's length by the caller (`cross_val_score`), so
    // `pairwise_corr_penalty` sees the **actual fold-slice N** and deflates on it (QE-430).
    let member_series: Vec<Vec<f64>> = members.iter().map(|&m| pool[m].clone()).collect();
    let corr = pairwise_corr_penalty(&member_series, cfg.corr_deflation).value;
    mean + cfg.tail_weight * tail - cfg.corr_weight * corr
}

/// The ensemble objective on the **equal-weight** combined series — [`objective_weighted`] under
/// [`Weighting::EqualWeight`] (byte-identical to the pre-QE-438 behaviour).
#[must_use]
pub fn objective(pool: &[Vec<f64>], members: &[usize], cfg: &ObjectiveConfig) -> f64 {
    objective_weighted(pool, members, cfg, Weighting::EqualWeight)
}

/// The worst single-member-removed objective under the deployed `weighting` (QE-115/D6 wide-basin floor,
/// QE-438): an ensemble that depends on one lucky strategy scores a low leave-one-out minimum. Each
/// leave-one-out subset re-derives its own deployed weights (the water-fill re-runs over the reduced
/// membership). Single-member ensembles return their own objective.
#[must_use]
pub fn leave_one_out_min_weighted(
    pool: &[Vec<f64>],
    members: &[usize],
    cfg: &ObjectiveConfig,
    weighting: Weighting,
) -> f64 {
    if members.len() <= 1 {
        return objective_weighted(pool, members, cfg, weighting);
    }
    let mut worst = f64::INFINITY;
    for drop in 0..members.len() {
        let reduced: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != drop)
            .map(|(_, &m)| m)
            .collect();
        worst = worst.min(objective_weighted(pool, &reduced, cfg, weighting));
    }
    worst
}

/// The wide-basin floor on the **equal-weight** objective — [`leave_one_out_min_weighted`] under
/// [`Weighting::EqualWeight`] (byte-identical to the pre-QE-438 behaviour).
#[must_use]
pub fn leave_one_out_min(pool: &[Vec<f64>], members: &[usize], cfg: &ObjectiveConfig) -> f64 {
    leave_one_out_min_weighted(pool, members, cfg, Weighting::EqualWeight)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    #[test]
    fn provenance_floor_binds_when_a_lineage_fakes_diversification() {
        // Build members where one evolved LINEAGE (provenance 1) has 4 mutually-decorrelated members that
        // are EACH correlated with the catalogue member (provenance 0). The flat member penalty is diluted
        // by the many low intra-lineage pairs, but collapsing the lineage to its MEAN series exposes the
        // genuine cross-lineage correlation ⇒ the provenance floor binds ABOVE the member penalty.
        let n = 400;
        // A common driver the catalogue member and every evolved member share.
        let driver: Vec<f64> = (0..n).map(|i| (i % 11) as f64 - 5.0).collect();
        let catalogue: Vec<f64> = driver.clone();
        // Each evolved member = driver + a large, member-specific orthogonal noise (so intra-lineage pairs
        // are near-decorrelated, but each still shares the driver with the catalogue member).
        let evolved: Vec<Vec<f64>> = (0..4)
            .map(|k| {
                (0..n)
                    .map(|i| driver[i] + 6.0 * (((i * 7 + k * 101) % 13) as f64 - 6.0))
                    .collect()
            })
            .collect();
        let mut series = vec![catalogue];
        series.extend(evolved);
        let provenance = vec![0usize, 1, 1, 1, 1];
        let is_evolved = vec![false, true, true, true, true];

        let report =
            provenance_aware_penalty(&series, &provenance, &is_evolved, CorrDeflation::None);
        assert!(
            report.provenance_floor > report.member_penalty,
            "the lineage floor must bind above the diluted member penalty: floor {} !> member {}",
            report.provenance_floor,
            report.member_penalty
        );
        approx(report.binding, report.provenance_floor);
        approx(report.evolved_share, 0.8);

        // Leave-one-provenance-out: DROP the evolved lineage entirely ⇒ only the catalogue member remains
        // ⇒ no pairs ⇒ the floor collapses to 0. The floor genuinely depends on the lineage clustering.
        let dropped = provenance_aware_penalty(
            &series[..1],
            &provenance[..1],
            &is_evolved[..1],
            CorrDeflation::None,
        );
        assert_eq!(dropped.provenance_floor, 0.0);
        assert!(dropped.provenance_floor < report.provenance_floor);

        // Evolved-share cap: 0.8 exceeds a 0.5 cap, is within a 0.9 cap.
        assert!(!report.share_within(0.5));
        assert!(report.share_within(0.9));
    }

    #[test]
    fn cluster_mean_collapses_lineages_deterministically() {
        let series = vec![vec![1.0, 3.0], vec![3.0, 5.0], vec![10.0, 10.0]];
        // Members 0,1 are lineage 7; member 2 is lineage 2. Output is in ascending label order (2 then 7).
        let means = cluster_mean_series(&series, &[7, 7, 2]);
        assert_eq!(means.len(), 2);
        assert_eq!(means[0], vec![10.0, 10.0]); // lineage 2
        assert_eq!(means[1], vec![2.0, 4.0]); // lineage 7 mean
        assert!(cluster_mean_series(&[], &[]).is_empty());
        approx(evolved_share(&[true, false, true, true]), 0.75);
        approx(evolved_share(&[]), 0.0);
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

    /// A deterministic xorshift `[−0.5, 0.5)` stream — independent draws for the noise property test
    /// (no `rand` dependency, byte-stable).
    fn xorshift_stream(mut state: u64) -> impl FnMut() -> f64 {
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        }
    }

    #[test]
    fn subthreshold_noise_scores_as_independence_not_lower() {
        // AC1. Over many randomised INDEPENDENT member series at small N, the DE search cannot lower the
        // correlation penalty by picking a sub-threshold sample correlation: every pair with |r| < R(N)
        // contributes exactly the independence baseline (0), and the deflated penalty never exceeds the
        // raw one — so a mask "chosen on noise" scores no better than independence.
        let z = DEFAULT_SIGNIFICANCE_Z;
        let mode = CorrDeflation::SignificanceFloor { z };
        let n = 12;
        let rn = min_significant_r(n, z);
        let mut next = xorshift_stream(0x9E37_79B9_7F4A_7C15);

        let mut subthreshold_seen = 0usize;
        let mut suprathreshold_seen = 0usize;
        for _ in 0..600 {
            let a: Vec<f64> = (0..n).map(|_| next()).collect();
            let b: Vec<f64> = (0..n).map(|_| next()).collect();
            let r = pearson(&a, &b);
            let deflated = pairwise_corr_penalty(&[a.clone(), b.clone()], mode).value;
            let raw = pairwise_corr_penalty(&[a, b], CorrDeflation::None).value;

            // Deflation can only *lower* the penalty — the search can never beat independence via noise.
            assert!(deflated <= raw + 1e-12, "deflated {deflated} > raw {raw}");
            if r.abs() < rn {
                subthreshold_seen += 1;
                approx(deflated, 0.0); // == the independence baseline
            } else {
                suprathreshold_seen += 1;
            }
        }
        // The generator actually exercised both sides of the threshold (the test is not vacuous), and the
        // ~5% supra-threshold leak is the false-positive rate the 1.96 curve is defined to admit.
        assert!(
            subthreshold_seen > 400,
            "most independent small-N pairs must be sub-threshold, got {subthreshold_seen}"
        );
        assert!(
            suprathreshold_seen > 0,
            "some independent pairs must cross R(N) (the 5% leak) — else the test is vacuous"
        );
    }

    #[test]
    fn subthreshold_floored_suprathreshold_still_penalised() {
        // AC2. A genuinely correlated pair whose sample r lands below R(N) is floored to 0; a
        // supra-threshold correlation is still penalised (kept at its raw value under the floor).
        let z = DEFAULT_SIGNIFICANCE_Z;
        let mode = CorrDeflation::SignificanceFloor { z };
        let n = 8;
        let rn = min_significant_r(n, z); // ≈ 0.7047

        // A modest positive correlation below R(8): sign-agreeing but noisy.
        let sub_a = vec![0.01, -0.02, 0.03, -0.01, 0.02, -0.03, 0.01, -0.02];
        let sub_b = vec![0.02, 0.01, 0.02, -0.03, -0.01, 0.02, 0.03, -0.01];
        let sub_r = pearson(&sub_a, &sub_b);
        assert!(
            sub_r.abs() < rn,
            "fixture must be sub-threshold: r={sub_r} R(N)={rn}"
        );
        assert!(sub_r > 0.0, "and genuinely positive: r={sub_r}");
        approx(pairwise_corr_penalty(&[sub_a, sub_b], mode).value, 0.0);

        // A strong positive correlation above R(8) is still penalised at its raw value.
        let sup_a = vec![0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08];
        let sup_b = vec![0.011, 0.019, 0.031, 0.041, 0.048, 0.062, 0.069, 0.079];
        let sup_r = pearson(&sup_a, &sup_b);
        assert!(sup_r > rn, "fixture must be supra-threshold: r={sup_r}");
        let sup_pen = pairwise_corr_penalty(&[sup_a, sup_b], mode).value;
        assert!(sup_pen > 0.0, "supra-threshold pair must stay penalised");
        approx(sup_pen, sup_r); // the significance floor keeps the raw value above the threshold
    }

    #[test]
    fn fisher_shrinkage_softens_without_a_cliff() {
        // A supra-threshold correlation under Fisher-z is shrunk toward 0 but stays positive (a softer
        // penalty than the significance floor, which would keep it whole).
        let a = vec![0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08];
        let b = vec![0.011, 0.019, 0.031, 0.041, 0.048, 0.062, 0.069, 0.079];
        let r = pearson(&a, &b);
        let shrunk = pairwise_corr_penalty(
            &[a, b],
            CorrDeflation::FisherShrinkage {
                lambda: DEFAULT_FISHER_LAMBDA,
            },
        )
        .value;
        assert!(
            shrunk > 0.0 && shrunk < r,
            "Fisher shrinkage must soften but not zero a supra-threshold r: shrunk={shrunk} r={r}"
        );
    }

    #[test]
    fn none_mode_reproduces_raw_pearson_penalty() {
        // The reproducible A/B + golden toggle: CorrDeflation::None == the raw positive-mean penalty.
        let a = vec![0.02, -0.01, 0.03, -0.02, 0.01, -0.03, 0.02, -0.01];
        let b = vec![0.021, -0.008, 0.029, -0.021, 0.012, -0.028, 0.019, -0.011];
        let raw = positive_mean_pairwise_corr(&[a.clone(), b.clone()]);
        approx(
            pairwise_corr_penalty(&[a, b], CorrDeflation::None).value,
            raw,
        );
    }

    #[test]
    fn effective_n_is_recorded_alongside_the_penalty() {
        // AC3 (unit level): the penalty surfaces the sample size it rested on, mirroring TailRisk.tail_n.
        let a = vec![0.01; 16];
        let b = vec![0.02; 16];
        let p = pairwise_corr_penalty(&[a, b], CorrDeflation::default());
        assert_eq!(p.effective_n, 16);
        // Uneven lengths ⇒ the smaller (binding) sample; < 2 series ⇒ 0.
        let long = vec![0.01; 20];
        let short = vec![0.02; 9];
        assert_eq!(
            pairwise_corr_penalty(&[long, short], CorrDeflation::default()).effective_n,
            9
        );
        assert_eq!(
            pairwise_corr_penalty(&[vec![0.01; 10]], CorrDeflation::default()).effective_n,
            0
        );
    }

    #[test]
    fn degenerate_small_n_is_treated_as_insignificant() {
        // N ≤ 3: the 1/√(N−3) scale is undefined ⇒ no correlation is distinguishable ⇒ contribution 0
        // (and never NaN), for both deflation modes.
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.1]; // near-perfect raw correlation
        assert!(pearson(&a, &b) > 0.99);
        for mode in [
            CorrDeflation::SignificanceFloor { z: 1.96 },
            CorrDeflation::FisherShrinkage { lambda: 1.96 },
        ] {
            let p = pairwise_corr_penalty(&[a.clone(), b.clone()], mode).value;
            assert!(p.is_finite() && p == 0.0, "N≤3 must floor to 0, got {p}");
        }
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

    // ---- QE-438: deployed (capacity-capped) weight scoring ------------------------------------

    #[test]
    fn member_weights_reduce_to_equal_when_caps_dont_bind() {
        // Non-binding caps (each capacity ≥ its 1/k dollar share at the AUM) ⇒ the deployed weights are
        // exactly the equal-weight budget — the reduction the AC requires.
        let members = [0usize, 2, 5];
        let huge = vec![1e18; 8];
        let w = Weighting::CapacityCapped {
            capacities: &huge,
            target_aum: 1_000_000.0,
        }
        .member_weights(&members);
        for &wi in &w {
            approx(wi, 1.0 / 3.0);
        }
        // A non-positive AUM disables capping ⇒ passthrough equal weights too.
        let w0 = Weighting::CapacityCapped {
            capacities: &[10.0, 10.0, 10.0],
            target_aum: 0.0,
        }
        .member_weights(&[0, 1, 2]);
        for &wi in &w0 {
            approx(wi, 1.0 / 3.0);
        }
        // The EqualWeight variant is 1/k by construction.
        assert_eq!(
            Weighting::EqualWeight.member_weights(&members),
            vec![1.0 / 3.0; 3]
        );
        assert!(Weighting::EqualWeight.member_weights(&[]).is_empty());
    }

    #[test]
    fn deployed_combined_reuses_weighted_combined() {
        // AC: the deployed combined series is exactly `weighted_combined(member_series, member_weights)`
        // — the reuse of stress.rs the ticket mandates.
        use crate::stress::weighted_combined;
        let pool = vec![
            vec![0.02, -0.01, 0.03, -0.02],
            vec![0.00, 0.01, -0.01, 0.02],
            vec![0.01, 0.02, 0.00, -0.03],
        ];
        let members = [0usize, 1, 2];
        // Bind member 0 hard (tiny capacity), leave 1 & 2 uncapped.
        let caps = vec![50_000.0, 1e18, 1e18];
        let weighting = Weighting::CapacityCapped {
            capacities: &caps,
            target_aum: 1_000_000.0,
        };
        let weights = weighting.member_weights(&members);
        let member_series: Vec<Vec<f64>> = members.iter().map(|&m| pool[m].clone()).collect();
        let expected = weighted_combined(&member_series, &weights);
        let got = combined_returns_weighted(&pool, &members, weighting);
        assert_eq!(got, expected);
        // And the capped weights are not the equal 1/3 (a cap really bound).
        assert!(
            (weights[0] - 1.0 / 3.0).abs() > 1e-6,
            "member 0 must be capped"
        );

        // Under EqualWeight it matches the plain equal-weight combination.
        assert_eq!(
            combined_returns_weighted(&pool, &members, Weighting::EqualWeight),
            combined_returns(&pool, &members)
        );
    }

    #[test]
    fn objective_reduces_to_equal_weight_when_caps_dont_bind() {
        // AC: with caps that never bind the deployed-weight objective equals the equal-weight objective.
        let pool = vec![
            vec![0.02, -0.01, 0.03, -0.02, 0.01, -0.03],
            vec![-0.01, 0.02, -0.02, 0.03, -0.03, 0.01],
        ];
        let cfg = ObjectiveConfig::with_defaults();
        let huge = vec![1e18; 2];
        let capped = objective_weighted(
            &pool,
            &[0, 1],
            &cfg,
            Weighting::CapacityCapped {
                capacities: &huge,
                target_aum: 1_000_000.0,
            },
        );
        approx(capped, objective(&pool, &[0, 1], &cfg));
    }

    #[test]
    fn objective_scores_on_deployed_weights_when_caps_bind() {
        // When a cap binds, the scored combined is the deployed (capped, cash-diluted) series, so the
        // objective differs from the equal-weight one — the DE now optimises the object it deploys.
        let pool = vec![vec![0.02; 6], vec![0.01; 6]];
        let cfg = ObjectiveConfig::with_defaults();
        // Both members tiny-capacity at $1M ⇒ both capped to 0.05, 90% cash ⇒ combined scaled ×0.1.
        let caps = vec![50_000.0, 50_000.0];
        let weighting = Weighting::CapacityCapped {
            capacities: &caps,
            target_aum: 1_000_000.0,
        };
        let deployed = objective_weighted(&pool, &[0, 1], &cfg, weighting);
        let equal = objective(&pool, &[0, 1], &cfg);
        assert!(
            deployed < equal,
            "cash-diluted deployed objective must be below equal-weight: deployed={deployed} equal={equal}"
        );
        // Concretely: equal combined = 0.015 constant ⇒ obj = 2·0.015 = 0.03 (constant ⇒ cvar = mean,
        // corr = 0); deployed combined = 0.1·0.015 = 0.0015 ⇒ obj = 0.003.
        approx(equal, 0.03);
        approx(deployed, 0.003);
    }

    #[test]
    fn deployed_weighting_can_flip_the_membership_ranking() {
        // The closed inconsistency: equal-weight scoring prefers {high, mid} but the DEPLOYED
        // capacity-capped weighting prefers {mid, mid2}, because the high-return member is hard-capped
        // and its edge is diluted away in the book that actually runs.
        let pool = vec![
            vec![0.02; 8],  // index 0 — high return, but capacity-constrained
            vec![0.01; 8],  // index 1 — mid
            vec![0.012; 8], // index 2 — mid, slightly better than 1
        ];
        let cfg = ObjectiveConfig::with_defaults();

        // Equal weight: {0,1} beats {1,2} (0.015 > 0.011 combined mean; constant ⇒ obj = 2·mean).
        let eq_01 = objective(&pool, &[0, 1], &cfg);
        let eq_12 = objective(&pool, &[1, 2], &cfg);
        assert!(
            eq_01 > eq_12,
            "equal-weight must prefer {{0,1}}: {eq_01} vs {eq_12}"
        );

        // Deployed weighting: member 0 caps at $50k (→ weight 0.05, member 1 water-filled to 0.95);
        // members 1 & 2 are uncapped (→ equal 0.5/0.5). Now {1,2} beats {0,1}.
        let caps = vec![50_000.0, 1e18, 1e18];
        let w = Weighting::CapacityCapped {
            capacities: &caps,
            target_aum: 1_000_000.0,
        };
        let dep_01 = objective_weighted(&pool, &[0, 1], &cfg, w);
        let dep_12 = objective_weighted(&pool, &[1, 2], &cfg, w);
        assert!(
            dep_12 > dep_01,
            "deployed weighting must flip the preference to {{1,2}}: {dep_12} vs {dep_01}"
        );
    }

    #[test]
    fn leave_one_out_min_rewaterfills_each_dropped_subset() {
        // The LOO floor under a binding weighting is finite and matches the deployed objective of the
        // binding (worst) 2-member drop — proving each subset re-derives its own deployed weights.
        let pool = vec![vec![0.02; 6], vec![0.01; 6], vec![0.012; 6]];
        let cfg = ObjectiveConfig::with_defaults();
        let caps = vec![50_000.0, 1e18, 1e18];
        let w = Weighting::CapacityCapped {
            capacities: &caps,
            target_aum: 1_000_000.0,
        };
        let loo = leave_one_out_min_weighted(&pool, &[0, 1, 2], &cfg, w);
        let worst_drop = objective_weighted(&pool, &[0, 1], &cfg, w)
            .min(objective_weighted(&pool, &[0, 2], &cfg, w))
            .min(objective_weighted(&pool, &[1, 2], &cfg, w));
        approx(loo, worst_drop);
        assert!(loo.is_finite());
    }
}
