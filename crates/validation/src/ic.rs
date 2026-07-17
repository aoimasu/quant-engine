//! Per-indicator IC / information-horizon screening (QE-434) — a catalogue-admission **pre-filter**.
//!
//! Dama §4.8/§4.10: before a factor is handed to the search, ask the table-stakes question — *does the
//! signal even predict forward return, and at what horizon?* This module computes **rank-IC** (Spearman
//! of a per-bar indicator signal vs forward **net** returns), **out-of-fold**, on the training/CV span,
//! reports **IC-by-horizon**, and classifies each indicator **Admit / Flag / Drop**:
//!
//! - **Admit** only if a *second fold* shows **same-sign** IC of **comparable magnitude** AND the factor
//!   clears a **Benjamini–Hochberg FDR** bar across all indicators screened.
//! - **Drop** a zero-IC (noise) factor; **Flag** one with signal that fails the second-fold or FDR bar.
//!
//! **The screen filters COMPUTE, never the hypothesis count** — it decides which factors are worth the
//! search's cells·gens·windows budget; it does not, and must not, alter the DSR trial count
//! ([`crate::dsr::effective_trials`]).
//!
//! **Firewall / decoupling.** This is *pure numeric*: it operates on `f64` signal columns + a net-return
//! series + fold **index sets**, taking no dependency on `qe-signal` or `qe-wfo`. The integration caller
//! (the train job) maps each catalogue indicator's per-bar `QState::index()` into a column, sizes the
//! forward horizon to the label horizon, and derives the two out-of-fold index sets from
//! `qe-wfo`'s purged/embargoed `PurgedKFold` (`crates/wfo/src/cv.rs`) so the pairing is leakage-safe.

use serde::{Deserialize, Serialize};

use crate::stats::normal_cdf;

/// Tie-corrected **Spearman rank correlation** (the rank-IC) between a per-bar `signal` and aligned
/// `forward` returns.
///
/// Only positions where **both** entries are finite are paired (so `NaN` warm-up / undefined-horizon
/// slots drop out). Returns `None` when fewer than two finite pairs remain or either ranked side is
/// dispersionless (a constant column has no defined correlation). Implemented as the Pearson correlation
/// of the **average-rank** transforms, so ties are handled correctly.
#[must_use]
pub fn rank_ic(signal: &[f64], forward: &[f64]) -> Option<f64> {
    let n = signal.len().min(forward.len());
    let mut xs = Vec::with_capacity(n);
    let mut ys = Vec::with_capacity(n);
    for i in 0..n {
        let (a, b) = (signal[i], forward[i]);
        if a.is_finite() && b.is_finite() {
            xs.push(a);
            ys.push(b);
        }
    }
    if xs.len() < 2 {
        return None;
    }
    let rx = average_ranks(&xs);
    let ry = average_ranks(&ys);
    pearson(&rx, &ry)
}

/// Two-sided p-value for a rank-IC of magnitude `ic` observed over `n` paired samples, under the
/// no-correlation null, via the large-sample normal approximation `z = ic·√(n−1)`,
/// `p = 2·(1 − Φ(|z|))`. Adequate for a screen (`n` is the CV-span bar count). Returns `1.0` for
/// `n < 2` (no evidence).
#[must_use]
pub fn spearman_pvalue(ic: f64, n: usize) -> f64 {
    if n < 2 || !ic.is_finite() {
        return 1.0;
    }
    let z = ic.abs() * ((n - 1) as f64).sqrt();
    let p = 2.0 * (1.0 - normal_cdf(z));
    p.clamp(0.0, 1.0)
}

/// **Benjamini–Hochberg** step-up at false-discovery level `q`: the largest rank `k` (ascending
/// p-value) with `p_(k) ≤ (k/m)·q` rejects hypotheses of ranks `1..=k`. Returns an admit-mask aligned to
/// the input order (`true` = rejected null = discovery). An empty input yields an empty mask.
#[must_use]
pub fn benjamini_hochberg(pvalues: &[f64], q: f64) -> Vec<bool> {
    let m = pvalues.len();
    if m == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&i, &j| {
        pvalues[i]
            .partial_cmp(&pvalues[j])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Largest k with p_(k) ≤ (k/m)·q.
    let mut k_max = 0usize;
    for (rank0, &idx) in order.iter().enumerate() {
        let k = rank0 + 1;
        let threshold = (k as f64 / m as f64) * q;
        if pvalues[idx] <= threshold {
            k_max = k;
        }
    }
    let mut mask = vec![false; m];
    for &idx in order.iter().take(k_max) {
        mask[idx] = true;
    }
    mask
}

/// Causal **forward net return** at each bar: the sum of the next `horizon` per-bar net returns
/// (`net_per_bar[t+1] + … + net_per_bar[t+horizon]`). The last `horizon` bars have no complete forward
/// window and are set to `f64::NAN` (dropped from any rank-IC pairing). `horizon = 0` yields all-`NaN`
/// (no forward information).
#[must_use]
pub fn forward_returns(net_per_bar: &[f64], horizon: usize) -> Vec<f64> {
    let n = net_per_bar.len();
    let mut out = vec![f64::NAN; n];
    if horizon == 0 {
        return out;
    }
    for (t, slot) in out.iter_mut().enumerate() {
        // The forward window t+1..=t+horizon must lie inside the series.
        if t + horizon < n {
            *slot = net_per_bar[t + 1..=t + horizon].iter().sum();
        }
    }
    out
}

/// One catalogue indicator's per-bar ordinal signal column: `id` plus the `QState::index() as f64` for
/// each bar (the caller maps a not-yet-warm `None` slot to `f64::NAN`).
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorSignals {
    /// Stable catalogue id, e.g. `"rsi_14"`.
    pub id: String,
    /// Per-bar ordinal signal (`NaN` where the indicator is not warm), aligned to the net-return series.
    pub values: Vec<f64>,
}

/// Screening configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct IcScreenConfig {
    /// Forward horizons (bars) to compute IC over — the "information horizon" sweep.
    pub horizons: Vec<usize>,
    /// Benjamini–Hochberg false-discovery level.
    pub fdr_q: f64,
    /// A factor whose best-horizon **replicated** |OOF IC| — `min(|ic_a|,|ic_b|)`, the magnitude that
    /// survives in *both* folds — is below this is treated as **zero-IC** → `Drop`.
    pub min_abs_ic: f64,
    /// Comparable-magnitude bar for the two-fold check: `min(|a|,|b|)/max(|a|,|b|) ≥ magnitude_ratio`.
    pub magnitude_ratio: f64,
}

impl Default for IcScreenConfig {
    fn default() -> Self {
        IcScreenConfig {
            horizons: vec![1, 4, 12],
            fdr_q: 0.05,
            min_abs_ic: 0.02,
            magnitude_ratio: 0.5,
        }
    }
}

/// The admission verdict for one indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// Second-fold sign-consistent **and** clears BH-FDR — worth the search's compute.
    Admit,
    /// Has non-trivial IC but fails the second-fold sign/magnitude check or the FDR bar.
    Flag,
    /// Zero-IC (noise) — best-horizon replicated |IC| `min(|ic_a|,|ic_b|)` below `min_abs_ic`.
    Drop,
}

/// Rank-IC at one horizon, split by fold plus the pooled (both-fold) estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonIc {
    /// Forward horizon (bars).
    pub horizon: usize,
    /// Out-of-fold rank-IC on fold A (`None` if undefined for that fold).
    pub ic_fold_a: Option<f64>,
    /// Out-of-fold rank-IC on fold B.
    pub ic_fold_b: Option<f64>,
    /// Rank-IC over the union of both folds (used for the p-value / BH).
    pub ic_pooled: Option<f64>,
}

/// The full screen for one indicator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndicatorScreen {
    /// Catalogue id.
    pub id: String,
    /// IC-by-horizon (both folds + pooled) — the reported information-horizon profile.
    pub horizons: Vec<HorizonIc>,
    /// The horizon maximising mean |fold-A, fold-B IC| (the factor's information horizon).
    pub primary_horizon: usize,
    /// Fold-A / fold-B IC at the primary horizon.
    pub ic_fold_a: Option<f64>,
    /// Fold-B IC at the primary horizon.
    pub ic_fold_b: Option<f64>,
    /// Whether the two folds agree in sign and are of comparable magnitude at the primary horizon.
    pub sign_consistent: bool,
    /// Pooled-IC p-value at the primary horizon (large-sample normal approximation).
    pub pvalue: f64,
    /// Whether the factor cleared the Benjamini–Hochberg bar across all screened indicators.
    pub passes_fdr: bool,
    /// The admission verdict.
    pub verdict: Verdict,
}

/// The catalogue screening report — one [`IndicatorScreen`] per indicator plus the config bars used.
/// `serde` so a later reporting artefact / opt-in admission step can persist it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IcScreenReport {
    /// Per-indicator screens, in the input (catalogue) order.
    pub indicators: Vec<IndicatorScreen>,
    /// The FDR level used.
    pub fdr_q: f64,
    /// The zero-IC drop bar used.
    pub min_abs_ic: f64,
    /// The comparable-magnitude ratio used.
    pub magnitude_ratio: f64,
}

impl IcScreenReport {
    /// Ids of the admitted indicators (in catalogue order).
    #[must_use]
    pub fn admitted_ids(&self) -> Vec<&str> {
        self.ids_where(Verdict::Admit)
    }

    /// Ids of the flagged indicators.
    #[must_use]
    pub fn flagged_ids(&self) -> Vec<&str> {
        self.ids_where(Verdict::Flag)
    }

    /// Ids of the dropped (zero-IC) indicators.
    #[must_use]
    pub fn dropped_ids(&self) -> Vec<&str> {
        self.ids_where(Verdict::Drop)
    }

    fn ids_where(&self, v: Verdict) -> Vec<&str> {
        self.indicators
            .iter()
            .filter(|s| s.verdict == v)
            .map(|s| s.id.as_str())
            .collect()
    }
}

/// Screen every catalogue indicator's forward-predictiveness out-of-fold, by horizon, and classify each
/// **Admit / Flag / Drop** (see the module docs). `net_returns` is the per-bar net-of-cost return series
/// aligned to every `signals[k].values`; `fold_a` / `fold_b` are disjoint out-of-fold bar-index sets on
/// the CV span (from `qe-wfo`'s purged/embargoed folds).
#[must_use]
pub fn screen_catalogue(
    signals: &[IndicatorSignals],
    net_returns: &[f64],
    fold_a: &[usize],
    fold_b: &[usize],
    cfg: &IcScreenConfig,
) -> IcScreenReport {
    // Precompute forward-return series per horizon once (shared across indicators).
    let forwards: Vec<(usize, Vec<f64>)> = cfg
        .horizons
        .iter()
        .map(|&h| (h, forward_returns(net_returns, h)))
        .collect();

    // First pass: per-indicator IC-by-horizon, primary horizon, sign-consistency, pooled p-value.
    struct Pre {
        id: String,
        horizons: Vec<HorizonIc>,
        primary_horizon: usize,
        ic_fold_a: Option<f64>,
        ic_fold_b: Option<f64>,
        sign_consistent: bool,
        replicated_magnitude: f64,
        pvalue: f64,
    }

    let mut pre: Vec<Pre> = Vec::with_capacity(signals.len());
    for sig in signals {
        let mut horizons = Vec::with_capacity(forwards.len());
        for (h, fwd) in &forwards {
            let ic_a = ic_on_fold(&sig.values, fwd, fold_a);
            let ic_b = ic_on_fold(&sig.values, fwd, fold_b);
            let ic_pooled = ic_on_union(&sig.values, fwd, fold_a, fold_b);
            horizons.push(HorizonIc {
                horizon: *h,
                ic_fold_a: ic_a,
                ic_fold_b: ic_b,
                ic_pooled,
            });
        }

        // Primary horizon = the horizon with the greatest **replicated** magnitude
        // `min(|ic_a|,|ic_b|)` — the information horizon at which *both* folds show signal. Selecting on
        // the cross-fold minimum (not a single fold's |IC|) avoids cherry-picking a horizon where one
        // fold spuriously spiked, the failure mode that lets pure noise masquerade as edge.
        let mut primary_idx = 0usize;
        let mut best_score = -1.0f64;
        for (i, hz) in horizons.iter().enumerate() {
            let score = replicated_magnitude(hz.ic_fold_a, hz.ic_fold_b);
            if score > best_score {
                best_score = score;
                primary_idx = i;
            }
        }
        let primary = horizons.get(primary_idx);
        let ic_a = primary.and_then(|h| h.ic_fold_a);
        let ic_b = primary.and_then(|h| h.ic_fold_b);
        let ic_pooled = primary.and_then(|h| h.ic_pooled);
        let primary_horizon = primary.map_or(0, |h| h.horizon);

        let sign_consistent = folds_sign_consistent(ic_a, ic_b, cfg.magnitude_ratio);
        let replicated = replicated_magnitude(ic_a, ic_b);

        // Pooled sample size for the p-value = paired finite observations over the fold union.
        let pooled_n = if primary_idx < forwards.len() {
            union_pairs(&sig.values, &forwards[primary_idx].1, fold_a, fold_b)
        } else {
            0
        };
        let pvalue = ic_pooled.map_or(1.0, |ic| spearman_pvalue(ic, pooled_n));

        pre.push(Pre {
            id: sig.id.clone(),
            horizons,
            primary_horizon,
            ic_fold_a: ic_a,
            ic_fold_b: ic_b,
            sign_consistent,
            replicated_magnitude: replicated,
            pvalue,
        });
    }

    // Second pass: BH-FDR across all screened indicators, then the final verdicts.
    let pvalues: Vec<f64> = pre.iter().map(|p| p.pvalue).collect();
    let fdr_mask = benjamini_hochberg(&pvalues, cfg.fdr_q);

    let indicators = pre
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let passes_fdr = fdr_mask.get(i).copied().unwrap_or(false);
            let verdict = if p.replicated_magnitude < cfg.min_abs_ic {
                Verdict::Drop
            } else if p.sign_consistent && passes_fdr {
                Verdict::Admit
            } else {
                Verdict::Flag
            };
            IndicatorScreen {
                id: p.id,
                horizons: p.horizons,
                primary_horizon: p.primary_horizon,
                ic_fold_a: p.ic_fold_a,
                ic_fold_b: p.ic_fold_b,
                sign_consistent: p.sign_consistent,
                pvalue: p.pvalue,
                passes_fdr,
                verdict,
            }
        })
        .collect();

    IcScreenReport {
        indicators,
        fdr_q: cfg.fdr_q,
        min_abs_ic: cfg.min_abs_ic,
        magnitude_ratio: cfg.magnitude_ratio,
    }
}

/// Rank-IC of `signal` vs `forward` restricted to the bar indices in `fold` (out-of-fold estimate).
fn ic_on_fold(signal: &[f64], forward: &[f64], fold: &[usize]) -> Option<f64> {
    let (s, f) = gather(signal, forward, fold.iter().copied());
    rank_ic(&s, &f)
}

/// Rank-IC over the union of two disjoint folds.
fn ic_on_union(signal: &[f64], forward: &[f64], a: &[usize], b: &[usize]) -> Option<f64> {
    let (s, f) = gather(signal, forward, a.iter().chain(b.iter()).copied());
    rank_ic(&s, &f)
}

/// Number of finite paired observations over the fold union (the p-value sample size).
fn union_pairs(signal: &[f64], forward: &[f64], a: &[usize], b: &[usize]) -> usize {
    let (s, _) = gather(signal, forward, a.iter().chain(b.iter()).copied());
    s.len()
}

/// Gather `(signal, forward)` at the given bar indices, keeping only in-range, finite pairs.
fn gather(
    signal: &[f64],
    forward: &[f64],
    idx: impl Iterator<Item = usize>,
) -> (Vec<f64>, Vec<f64>) {
    let mut s = Vec::new();
    let mut f = Vec::new();
    for i in idx {
        if let (Some(&a), Some(&b)) = (signal.get(i), forward.get(i)) {
            if a.is_finite() && b.is_finite() {
                s.push(a);
                f.push(b);
            }
        }
    }
    (s, f)
}

/// The **replicated** magnitude `min(|ic_a|, |ic_b|)` — the |IC| that survives in *both* folds (a missing
/// fold IC counts as `0`, so a factor defined in only one fold has no replicated signal).
fn replicated_magnitude(a: Option<f64>, b: Option<f64>) -> f64 {
    a.map_or(0.0, f64::abs).min(b.map_or(0.0, f64::abs))
}

/// Whether two fold ICs share a non-zero sign and are of comparable magnitude.
fn folds_sign_consistent(a: Option<f64>, b: Option<f64>, magnitude_ratio: f64) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let same_sign = a != 0.0 && b != 0.0 && (a > 0.0) == (b > 0.0);
            let (lo, hi) = (a.abs().min(b.abs()), a.abs().max(b.abs()));
            let comparable = hi > 0.0 && lo / hi >= magnitude_ratio;
            same_sign && comparable
        }
        _ => false,
    }
}

/// Average ranks (1-based mean rank per tie group; here 0-based positions, which is fine because Pearson
/// is invariant to a constant offset).
fn average_ranks(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&i, &j| {
        xs[i]
            .partial_cmp(&xs[j])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ranks = vec![0.0f64; n];
    let mut i = 0usize;
    while i < n {
        let mut j = i + 1;
        while j < n && xs[idx[j]] == xs[idx[i]] {
            j += 1;
        }
        let avg = (i + j - 1) as f64 / 2.0; // mean 0-based position of the tie group
        for &k in &idx[i..j] {
            ranks[k] = avg;
        }
        i = j;
    }
    ranks
}

/// Pearson correlation of two equal-length series; `None` if either is dispersionless.
fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let n = a.len();
    if n < 2 || b.len() != n {
        return None;
    }
    let inv = 1.0 / n as f64;
    let ma = a.iter().sum::<f64>() * inv;
    let mb = b.iter().sum::<f64>() * inv;
    let mut cov = 0.0;
    let mut va = 0.0;
    let mut vb = 0.0;
    for i in 0..n {
        let da = a[i] - ma;
        let db = b[i] - mb;
        cov += da * db;
        va += da * da;
        vb += db * db;
    }
    if va <= 0.0 || vb <= 0.0 {
        return None;
    }
    Some(cov / (va.sqrt() * vb.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::seed_rng;
    use rand_core::RngCore;

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} !~ {b} (tol {tol})");
    }

    /// A uniform-ish f64 in [0,1) from the seeded portable RNG (reproducible).
    fn unit(rng: &mut impl RngCore) -> f64 {
        (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    // ---- rank-IC correctness on known-correlated synthetic series (AC) ----

    #[test]
    fn rank_ic_perfect_monotone_is_plus_one() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = (0..50).map(|i| (i as f64).powi(3) + 1.0).collect(); // monotone, non-linear
        approx(rank_ic(&x, &y).unwrap(), 1.0, 1e-12);
    }

    #[test]
    fn rank_ic_perfect_anti_monotone_is_minus_one() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = (0..50).map(|i| -(i as f64)).collect();
        approx(rank_ic(&x, &y).unwrap(), -1.0, 1e-12);
    }

    #[test]
    fn rank_ic_handles_ties_via_average_ranks() {
        // Quantised (tied) signal vs a return that increases with the bucket → strong positive rank-IC.
        let x = [0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0];
        let y = [0.1, 0.2, 0.9, 1.1, 1.8, 2.2, 2.9, 3.1];
        let ic = rank_ic(&x, &y).unwrap();
        assert!(
            ic > 0.95,
            "tie-corrected rank-IC should be strongly positive, got {ic}"
        );
        // Matches a hand path: ranks of x are the tie-group means; correlation with monotone y is ~1.
    }

    #[test]
    fn rank_ic_drops_non_finite_and_needs_dispersion() {
        // NaN pairs are dropped; a constant signal has no defined correlation.
        let x = [1.0, f64::NAN, 3.0, 4.0];
        let y = [10.0, 20.0, f64::NAN, 40.0];
        // Only (1,10) and (4,40) survive → 2 pairs, monotone → +1.
        approx(rank_ic(&x, &y).unwrap(), 1.0, 1e-12);
        assert!(rank_ic(&[2.0, 2.0, 2.0], &[1.0, 2.0, 3.0]).is_none()); // dispersionless signal
        assert!(rank_ic(&[1.0], &[1.0]).is_none()); // too short
    }

    #[test]
    fn rank_ic_independent_noise_is_near_zero() {
        let mut rng = seed_rng(20260717);
        let x: Vec<f64> = (0..2000).map(|_| unit(&mut rng)).collect();
        let y: Vec<f64> = (0..2000).map(|_| unit(&mut rng)).collect();
        approx(rank_ic(&x, &y).unwrap(), 0.0, 0.05);
    }

    // ---- forward_returns causal alignment (AC: leakage-safe) ----

    #[test]
    fn forward_returns_are_causal_next_h_sum() {
        let net = [0.0, 1.0, 2.0, 3.0, 4.0];
        let f1 = forward_returns(&net, 1);
        // f1[t] = net[t+1]; last is NaN.
        approx(f1[0], 1.0, 1e-12);
        approx(f1[3], 4.0, 1e-12);
        assert!(f1[4].is_nan());
        let f2 = forward_returns(&net, 2);
        approx(f2[0], 1.0 + 2.0, 1e-12);
        approx(f2[2], 3.0 + 4.0, 1e-12);
        assert!(f2[3].is_nan() && f2[4].is_nan());
        assert!(forward_returns(&net, 0).iter().all(|v| v.is_nan()));
    }

    // ---- Benjamini–Hochberg admits/rejects correctly (AC) ----

    #[test]
    fn bh_all_reject_when_all_below_line() {
        // p_(k) = 0.01·k, m=4, q=0.05 → threshold k/m·q = 0.0125·k ≥ 0.01·k for all k → all rejected.
        let p = [0.01, 0.02, 0.03, 0.04];
        assert_eq!(benjamini_hochberg(&p, 0.05), vec![true, true, true, true]);
    }

    #[test]
    fn bh_single_reject_and_input_order_preserved() {
        // Only the smallest clears; the mask is aligned to input order (smallest is at index 2 here).
        let p = [0.7, 0.8, 0.001, 0.9];
        assert_eq!(
            benjamini_hochberg(&p, 0.05),
            vec![false, false, true, false]
        );
    }

    #[test]
    fn bh_step_up_rejects_a_middling_p_below_a_larger_one() {
        // Classic step-up: even though p_(2)=0.02 < 0.025 and p_(3)=0.049 > 0.0375, the largest passing
        // k governs — here k=2 rejects ranks 1..2. Then a case where a later k passes lifts earlier ones.
        let p = [0.001, 0.02, 0.049, 0.5];
        // thresholds: 0.0125,0.025,0.0375,0.05. k=1:0.001≤0.0125 T; k=2:0.02≤0.025 T; k=3:0.049≤0.0375 F;
        // k=4:0.5≤0.05 F → k_max=2 → two smallest rejected.
        assert_eq!(benjamini_hochberg(&p, 0.05), vec![true, true, false, false]);
        assert!(benjamini_hochberg(&[], 0.05).is_empty());
    }

    #[test]
    fn spearman_pvalue_small_for_strong_ic_large_for_zero() {
        assert!(spearman_pvalue(0.9, 500) < 1e-6);
        approx(spearman_pvalue(0.0, 500), 1.0, 1e-6); // ~1 (erf approx ⇒ Φ(0) is 0.5±1e-7)
        approx(spearman_pvalue(0.5, 1), 1.0, 1e-12); // too short → no evidence
    }

    // ---- two-fold sign-consistency (AC) ----

    #[test]
    fn sign_consistency_same_sign_comparable_passes_flip_fails() {
        assert!(folds_sign_consistent(Some(0.20), Some(0.16), 0.5)); // same sign, ratio 0.8
        assert!(!folds_sign_consistent(Some(0.20), Some(-0.18), 0.5)); // sign flip
        assert!(!folds_sign_consistent(Some(0.20), Some(0.05), 0.5)); // ratio 0.25 < 0.5
        assert!(!folds_sign_consistent(Some(0.20), None, 0.5)); // missing fold
    }

    // ---- end-to-end screen: predictive admitted, sign-flip flagged, noise dropped (AC) ----

    /// Build a signal column and a matching net-return series with a controllable relationship, over
    /// `n` bars split into two contiguous folds `[0,half)` and `[half,n)`.
    fn folds(n: usize) -> (Vec<usize>, Vec<usize>) {
        let half = n / 2;
        ((0..half).collect(), (half..n).collect())
    }

    #[test]
    fn screen_admits_predictive_flags_signflip_drops_noise() {
        // Large span so the noise factor's spurious per-fold |IC| (~1/√n_fold) stays well under the
        // drop bar, and both real folds have ample sample.
        let n = 4000usize;
        let (fold_a, fold_b) = folds(n);
        let mut rng = seed_rng(42);

        // A net-return series driven by a hidden factor plus noise.
        let hidden: Vec<f64> = (0..n).map(|_| unit(&mut rng) - 0.5).collect();
        // Per-bar net return realised at t+1 depends on the hidden factor at t.
        let mut net = vec![0.0f64; n];
        for t in 1..n {
            net[t] = 0.9 * hidden[t - 1] + 0.1 * (unit(&mut rng) - 0.5);
        }

        // Predictive indicator: quantised (5 buckets) monotone in the hidden factor → real forward IC.
        let predictive = IndicatorSignals {
            id: "predictive".into(),
            values: (0..n)
                .map(|t| ((hidden[t] + 0.5) * 5.0).floor().clamp(0.0, 4.0))
                .collect(),
        };
        // Sign-flip indicator: predictive in fold A, anti-predictive in fold B (unstable) → Flag.
        let half = n / 2;
        let signflip = IndicatorSignals {
            id: "signflip".into(),
            values: (0..n)
                .map(|t| {
                    let q = ((hidden[t] + 0.5) * 5.0).floor().clamp(0.0, 4.0);
                    if t < half {
                        q
                    } else {
                        4.0 - q
                    }
                })
                .collect(),
        };
        // Pure-noise indicator: independent of returns → Drop.
        let noise = IndicatorSignals {
            id: "noise".into(),
            values: (0..n).map(|_| (unit(&mut rng) * 5.0).floor()).collect(),
        };

        let cfg = IcScreenConfig {
            horizons: vec![1, 4],
            min_abs_ic: 0.05, // comfortably above noise's replicated |IC| at this n, below real signal
            ..IcScreenConfig::default()
        };
        let report = screen_catalogue(
            &[predictive.clone(), signflip, noise],
            &net,
            &fold_a,
            &fold_b,
            &cfg,
        );

        let by = |id: &str| {
            report
                .indicators
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            by("predictive").verdict,
            Verdict::Admit,
            "{:?}",
            by("predictive")
        );
        assert_eq!(
            by("signflip").verdict,
            Verdict::Flag,
            "{:?}",
            by("signflip")
        );
        assert_eq!(by("noise").verdict, Verdict::Drop, "{:?}", by("noise"));

        // The predictive factor is sign-consistent and clears FDR; the noise factor does not.
        assert!(by("predictive").sign_consistent && by("predictive").passes_fdr);
        assert!(!by("noise").passes_fdr);
        // IC-by-horizon is reported for every indicator.
        assert_eq!(by("predictive").horizons.len(), 2);
        assert!(by("predictive").primary_horizon == 1 || by("predictive").primary_horizon == 4);

        // Report helpers reflect the verdicts.
        assert_eq!(report.admitted_ids(), vec!["predictive"]);
        assert_eq!(report.flagged_ids(), vec!["signflip"]);
        assert_eq!(report.dropped_ids(), vec!["noise"]);
    }

    #[test]
    fn report_round_trips_through_serde() {
        let n = 200usize;
        let (fa, fb) = folds(n);
        let net: Vec<f64> = (0..n).map(|t| ((t % 7) as f64 - 3.0) / 100.0).collect();
        let sig = IndicatorSignals {
            id: "x".into(),
            values: (0..n).map(|t| (t % 5) as f64).collect(),
        };
        let report = screen_catalogue(&[sig], &net, &fa, &fb, &IcScreenConfig::default());
        let json = serde_json::to_string(&report).unwrap();
        let back: IcScreenReport = serde_json::from_str(&json).unwrap();
        // The report persists and decodes back to the same structure/verdicts. (Exact f64 bit-equality
        // is not asserted: this workspace's serde_json is built without the `float_roundtrip` feature,
        // so its float parser may differ by ≤1 ULP — irrelevant for a diagnostic artefact.)
        assert_eq!(back.indicators.len(), report.indicators.len());
        let (a, b) = (&back.indicators[0], &report.indicators[0]);
        assert_eq!(a.id, b.id);
        assert_eq!(a.verdict, b.verdict);
        assert_eq!(a.primary_horizon, b.primary_horizon);
        assert_eq!(a.sign_consistent, b.sign_consistent);
        assert_eq!(a.passes_fdr, b.passes_fdr);
        assert_eq!(a.horizons.len(), b.horizons.len());
        approx(a.pvalue, b.pvalue, 1e-9);
        approx(a.ic_fold_a.unwrap(), b.ic_fold_a.unwrap(), 1e-9);
        approx(back.fdr_q, report.fdr_q, 1e-12);
    }
}
