//! Probability of Backtest Overfitting via CSCV (QE-131/D3 — Bailey, Borwein, López de Prado, Zhu 2017).
//!
//! Combinatorially Symmetric Cross-Validation splits the time axis into `S` blocks, then over every
//! balanced train/test partition asks: does the strategy that looked best **in-sample** still rank above
//! the median **out-of-sample**? PBO is the fraction of partitions where it does *not* — the probability
//! that selecting on in-sample performance is overfitting.

use crate::stats::sharpe_ratio;
use crate::ValidationError;

/// The CSCV result (QE-131/D3).
#[derive(Debug, Clone, PartialEq)]
pub struct PboReport {
    /// Probability of backtest overfitting — fraction of partitions whose IS-best is below the OOS median.
    pub pbo: f64,
    /// Number of balanced partitions evaluated (`C(S, S/2)`).
    pub n_combinations: usize,
    /// The per-partition logit of the IS-best's relative OOS rank (`≤ 0` ⇒ below OOS median).
    pub logits: Vec<f64>,
}

/// Compute PBO via CSCV over a `T × N` return `matrix` (`matrix[t][k]` = strategy `k`'s return at time
/// `t`), splitting the time axis into `blocks` (`S`, must be even and `≥ 2`) contiguous blocks. The
/// performance metric is the per-period Sharpe ratio.
///
/// For each of the `C(S, S/2)` ways to choose the in-sample blocks: find the IS-best strategy `n*`,
/// compute its rank `ω ∈ [1, N]` among all strategies' OOS Sharpe, form the relative rank
/// `ω̄ = ω/(N+1)` and the logit `λ = ln(ω̄/(1−ω̄))`. **PBO = #{λ ≤ 0} / C(S, S/2)**.
///
/// # Errors
/// [`ValidationError::OddBlockCount`] if `blocks` is odd or `< 2`; [`ValidationError::EmptyMatrix`] if the
/// matrix has no rows/columns or fewer rows than blocks.
pub fn pbo_cscv(matrix: &[Vec<f64>], blocks: usize) -> Result<PboReport, ValidationError> {
    if blocks < 2 || !blocks.is_multiple_of(2) {
        return Err(ValidationError::OddBlockCount(blocks));
    }
    let t = matrix.len();
    let n = matrix.first().map_or(0, Vec::len);
    if t < blocks || n == 0 {
        return Err(ValidationError::EmptyMatrix);
    }

    // Contiguous block boundaries over the time axis.
    let bounds: Vec<(usize, usize)> = (0..blocks)
        .map(|b| (b * t / blocks, (b + 1) * t / blocks))
        .collect();

    let mut logits = Vec::new();
    let mut overfit = 0usize;
    for is_blocks in combinations(blocks, blocks / 2) {
        let is_mask = mask(blocks, &is_blocks);
        // IS / OOS performance per strategy.
        let is_perf = perf_per_strategy(matrix, &bounds, &is_mask, true, n);
        let oos_perf = perf_per_strategy(matrix, &bounds, &is_mask, false, n);

        let best = argmax(&is_perf);
        // Ascending OOS rank of the IS-best (1 = OOS-worst, N = OOS-best): 1 + #strategies it beats OOS.
        // A high relative rank ⇒ the IS-best is also OOS-good ⇒ positive logit ⇒ not overfit.
        let below = oos_perf.iter().filter(|&&p| p < oos_perf[best]).count();
        let rank = below + 1;
        let rel = rank as f64 / (n as f64 + 1.0); // ∈ (0,1)
        let logit = (rel / (1.0 - rel)).ln();
        if logit <= 0.0 {
            overfit += 1; // IS-best landed at/below the OOS median ⇒ overfit
        }
        logits.push(logit);
    }

    let n_combinations = logits.len();
    let pbo = overfit as f64 / n_combinations as f64;
    Ok(PboReport {
        pbo,
        n_combinations,
        logits,
    })
}

/// Per-strategy Sharpe over the rows in (or out of) the in-sample blocks.
fn perf_per_strategy(
    matrix: &[Vec<f64>],
    bounds: &[(usize, usize)],
    is_mask: &[bool],
    in_sample: bool,
    n: usize,
) -> Vec<f64> {
    let mut cols: Vec<Vec<f64>> = vec![Vec::new(); n];
    for (b, &(lo, hi)) in bounds.iter().enumerate() {
        if is_mask[b] != in_sample {
            continue;
        }
        for row in &matrix[lo..hi] {
            for (k, col) in cols.iter_mut().enumerate() {
                col.push(row.get(k).copied().unwrap_or(0.0));
            }
        }
    }
    cols.iter().map(|c| sharpe_ratio(c)).collect()
}

/// A length-`blocks` boolean mask, `true` at the chosen in-sample block indices.
fn mask(blocks: usize, chosen: &[usize]) -> Vec<bool> {
    let mut m = vec![false; blocks];
    for &c in chosen {
        m[c] = true;
    }
    m
}

/// Index of the maximum element (first on ties; falls back to index 0 if all equal/NaN).
fn argmax(xs: &[f64]) -> usize {
    xs.iter()
        .enumerate()
        .fold(0usize, |b, (i, &x)| if x > xs[b] { i } else { b })
}

/// All `k`-subsets of `0..n` as ascending index vectors (lexicographic).
fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut idx: Vec<usize> = (0..k).collect();
    if k == 0 || k > n {
        return out;
    }
    loop {
        out.push(idx.clone());
        // Advance like an odometer from the right.
        let mut i = k;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + n - k {
                break;
            }
        }
        idx[i] += 1;
        for j in i + 1..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_are_complete_and_balanced() {
        let c = combinations(4, 2);
        assert_eq!(c.len(), 6); // C(4,2)
        assert!(c.contains(&vec![0, 1]) && c.contains(&vec![2, 3]));
    }

    #[test]
    fn odd_or_tiny_block_count_is_rejected() {
        let m = vec![vec![0.01, 0.02]; 8];
        assert!(matches!(
            pbo_cscv(&m, 3),
            Err(ValidationError::OddBlockCount(3))
        ));
        assert!(matches!(
            pbo_cscv(&m, 0),
            Err(ValidationError::OddBlockCount(0))
        ));
    }

    #[test]
    fn genuinely_robust_matrix_has_low_pbo() {
        // Strategy 0 is uniformly best everywhere; the rest are flat. IS-best is always OOS-best ⇒ PBO 0.
        let t = 40;
        let matrix: Vec<Vec<f64>> = (0..t)
            .map(|i| {
                let good = 0.02 + 0.001 * ((i % 4) as f64); // always positive, low-vol
                vec![good, 0.0, -0.01, 0.001]
            })
            .collect();
        let report = pbo_cscv(&matrix, 6).unwrap();
        assert_eq!(report.pbo, 0.0, "logits: {:?}", report.logits);
    }

    #[test]
    fn overfit_matrix_has_high_pbo() {
        // Each strategy is great in exactly one half of time and terrible in the other, anti-correlated:
        // whoever wins IS loses OOS ⇒ PBO near 1. A within-block oscillation gives every block a genuine
        // (non-degenerate) Sharpe, so the ranking is driven by the anti-correlation, not a tie-break on a
        // zero-dispersion mean.
        let t = 40;
        let matrix: Vec<Vec<f64>> = (0..t)
            .map(|i| {
                let osc = 0.01 * ((i % 3) as f64 - 1.0); // within-block dispersion ⇒ finite Sharpe
                if i < t / 2 {
                    vec![0.05 + osc, -0.05 + osc]
                } else {
                    vec![-0.05 + osc, 0.05 + osc]
                }
            })
            .collect();
        // Use 2 blocks so IS = one half, OOS = the other — the IS winner is the OOS loser.
        let report = pbo_cscv(&matrix, 2).unwrap();
        assert_eq!(report.pbo, 1.0, "logits: {:?}", report.logits);
    }
}
