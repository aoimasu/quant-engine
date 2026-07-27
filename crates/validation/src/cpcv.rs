//! Combinatorial Purged Cross-Validation (CPCV) — a **distribution** of held-out Sharpe/DSR (QE-469,
//! López de Prado *Advances in Financial Machine Learning* Ch. 12.4, p. 163).
//!
//! The G1 terminal holdout is a **single** train/holdout split producing a single OOS number
//! (`crates/wfo/src/cv_fitness.rs:34-35`). CPCV replaces that point estimate with a distribution: split
//! the time axis into `S` contiguous blocks, and over every balanced `C(S, S/2)` partition hold out
//! `S/2` blocks — reconstructing, per partition, the candidate's **leak-free** concatenated net-of-cost
//! held-out return series. Reducing the per-partition Sharpe/DSR to median / IQR / percentile + the
//! fraction clearing the DSR floor turns the OOS verdict into a confidence interval.
//!
//! ## Reuse, not reinvention (QE-469 scope)
//!
//! - **Balanced-partition enumeration** reuses [`crate::pbo::combinations`] verbatim (the same CSCV block
//!   enumeration `pbo_cscv` iterates) — same crate, zero reimplementation.
//! - **Purge + embargo** apply the **exact arithmetic** of `qe_wfo::cv::PurgedKFold`
//!   (`purge = lookback + label_horizon`, default `embargo = lookback`, exclusion
//!   `[start − purge, end + purge + embargo)`) per held-out block. `qe-validation` cannot depend on
//!   `qe-wfo` (wfo already depends on validation — that would be a cycle), so the arithmetic is mirrored
//!   here and a **cross-crate equivalence test** in `qe-wfo` pins that this geometry agrees with
//!   `PurgedKFold::folds`. The purge/embargo arithmetic itself is unchanged (QE-469 scope guardrail).
//! - **Statistics** reuse [`crate::stats::sharpe_ratio`] and [`crate::dsr::deflated_sharpe_ratio`].
//!
//! ## Path-count formula (Ch. 12.4)
//!
//! With `S` blocks and `k = S/2` held out per split, the number of balanced splits (the held-out
//! configurations forming the distribution) is `C(S, S/2)`, and the number of distinct full-length
//! backtest paths reconstructable from them is `φ(S) = C(S, S/2)·(S/2)/S = C(S − 1, S/2 − 1)` — because
//! each block is a test block in `C(S−1, S/2−1)` splits. `S ≥ 4 ⇒ φ ≥ 2`.
//!
//! **Fixed-candidate note.** The candidate here is the *already-selected* deployed ensemble (no per-split
//! refit), so a block's held-out return is candidate-fixed and the `φ` reconstructed paths coincide. The
//! informative multiplicity is therefore the `C(S, S/2)` held-out **configurations**; the distribution is
//! taken over their Sharpes/DSRs. Both counts are exported for auditability.
//!
//! ## Determinism (QE-006)
//!
//! Path geometry is RNG-free — lexicographic partition order + arithmetic purge — so two runs with
//! identical inputs produce byte-identical path sets. The `qe_determinism::task_rng(master, index)` scheme
//! is the seeding source for any future per-path stochastic reduction; the analytic DSR reduction here
//! needs none.

use std::ops::Range;

use crate::dsr::deflated_sharpe_ratio;
use crate::pbo::combinations;
use crate::stats::sharpe_ratio;
use crate::ValidationError;

/// The default DSR floor a held-out path must clear (QE-131/QE-439 gate floor), reused as the CPCV
/// distribution's `frac_dsr_ge_floor` threshold and the gate floor.
pub const DEFAULT_DSR_FLOOR: f64 = 0.95;

/// The lower percentile of the held-out DSR distribution the promotion gate decides on (QE-469: the
/// **lower** percentile, not the mean, must clear the floor).
pub const DEFAULT_DSR_PERCENTILE: f64 = 0.05;

/// The minimum held-out configurations required for a powered CPCV verdict — below it the gate fails
/// **closed** (`C(S, S/2) ≥ 6` at `S = 4`, so this rejects the degenerate `S = 2` two-config split).
pub const DEFAULT_MIN_PATHS: usize = 4;

/// Number of **balanced partitions** (held-out configurations) for `S` blocks: `C(S, S/2)`. This is the
/// size of the CPCV distribution's sample. `0` for odd/`< 2` block counts.
#[must_use]
pub fn balanced_partition_count(blocks: usize) -> usize {
    if blocks < 2 || !blocks.is_multiple_of(2) {
        return 0;
    }
    n_choose_k(blocks, blocks / 2)
}

/// Number of distinct full-length backtest **paths** reconstructable from `S` blocks per López de Prado
/// Ch. 12.4: `φ(S) = C(S, S/2)·(S/2)/S = C(S − 1, S/2 − 1)`. `0` for odd/`< 2` block counts.
#[must_use]
pub fn cpcv_path_count(blocks: usize) -> usize {
    if blocks < 2 || !blocks.is_multiple_of(2) {
        return 0;
    }
    n_choose_k(blocks - 1, blocks / 2 - 1)
}

/// Exact `C(n, k)` for the small block counts CPCV uses (multiplicative form; no overflow for `n ≤ 62`).
fn n_choose_k(n: usize, k: usize) -> usize {
    if k > n {
        return 0;
    }
    let k = k.min(n - k);
    let mut num: u128 = 1;
    for i in 0..k {
        num = num * (n - i) as u128 / (i as u128 + 1);
    }
    usize::try_from(num).unwrap_or(usize::MAX)
}

/// One CPCV split: the `S/2` **held-out** (test) block ranges and the purged + embargoed **train** indices
/// — the analogue of `qe_wfo::cv::Fold` for a multi-block held-out group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpcvPath {
    /// The held-out (test) block ranges `[start, end)`, in ascending time order.
    pub test: Vec<Range<usize>>,
    /// Train bar indices, with the purge + embargo zone around **every** held-out block removed.
    pub train: Vec<usize>,
}

impl CpcvPath {
    /// Whether every `(train, test)` pair has `|tr − te| > lookback + label_horizon` — the held-out ranges'
    /// information windows are disjoint from every train bar's, including the lookback. Mirrors
    /// `qe_wfo::cv::Fold::windows_disjoint`: the leakage-free invariant purging guarantees.
    #[must_use]
    pub fn windows_disjoint(&self, lookback: usize, label_horizon: usize) -> bool {
        let span = lookback + label_horizon;
        self.train.iter().all(|&tr| {
            self.test
                .iter()
                .all(|r| r.clone().all(|te| tr.abs_diff(te) > span))
        })
    }

    /// The candidate's concatenated held-out return series: `candidate[r]` over each held-out range `r`,
    /// in time order. Out-of-range indices are skipped (the caller builds paths from `candidate.len()`).
    #[must_use]
    pub fn returns(&self, candidate: &[f64]) -> Vec<f64> {
        let mut out = Vec::new();
        for r in &self.test {
            for i in r.clone() {
                if let Some(&v) = candidate.get(i) {
                    out.push(v);
                }
            }
        }
        out
    }
}

/// Generate the CPCV held-out paths over `0..n_obs`: split into `blocks` (`S`, even `≥ 2`) contiguous
/// blocks, then for every balanced `C(S, S/2)` partition hold out `S/2` blocks and purge+embargo the
/// train side around **every** held-out block (`purge = lookback + label_horizon`, exclusion
/// `[start − purge, end + purge + embargo)`). The partition order is the lexicographic
/// [`combinations`] order — deterministic, RNG-free.
///
/// # Errors
/// [`ValidationError::OddBlockCount`] if `blocks` is odd or `< 2`; [`ValidationError::EmptyMatrix`] if
/// `n_obs < blocks` (a block would be empty).
pub fn cpcv_paths(
    n_obs: usize,
    blocks: usize,
    lookback: usize,
    label_horizon: usize,
    embargo: usize,
) -> Result<Vec<CpcvPath>, ValidationError> {
    if blocks < 2 || !blocks.is_multiple_of(2) {
        return Err(ValidationError::OddBlockCount(blocks));
    }
    if n_obs < blocks {
        return Err(ValidationError::EmptyMatrix);
    }
    // Contiguous block boundaries over the observation axis (same partition as `pbo_cscv`).
    let bounds: Vec<(usize, usize)> = (0..blocks)
        .map(|b| (b * n_obs / blocks, (b + 1) * n_obs / blocks))
        .collect();
    let purge = lookback + label_horizon;

    let mut out = Vec::new();
    for test_blocks in combinations(blocks, blocks / 2) {
        // Held-out ranges, ascending (combinations yields ascending block indices).
        let test: Vec<Range<usize>> = test_blocks
            .iter()
            .map(|&b| bounds[b].0..bounds[b].1)
            .collect();
        // A train bar is excluded if it lands in ANY held-out block's purge+embargo exclusion zone.
        let train: Vec<usize> = (0..n_obs)
            .filter(|&i| {
                test.iter().all(|r| {
                    let excl_lo = r.start.saturating_sub(purge);
                    let excl_hi = (r.end + purge + embargo).min(n_obs);
                    i < excl_lo || i >= excl_hi
                })
            })
            .collect();
        out.push(CpcvPath { test, train });
    }
    Ok(out)
}

/// The CPCV out-of-sample **distribution** summary (QE-469): per-path Sharpe/DSR vectors plus the
/// reduction the promotion gate consumes. This is an in-memory analysis type — it is **not** serialised;
/// persistence goes through the separate `qe_vintage::CpcvSummary` (the CLI maps this into it at seal).
#[derive(Debug, Clone, PartialEq)]
pub struct CpcvDistribution {
    /// Per-held-out-path Sharpe ratios (one per balanced partition), in partition order.
    pub sharpes: Vec<f64>,
    /// Per-held-out-path Deflated Sharpe Ratios, in partition order.
    pub dsrs: Vec<f64>,
    /// Median held-out Sharpe.
    pub median_sharpe: f64,
    /// Interquartile range of held-out Sharpe: `(25th, 75th)` percentile.
    pub sharpe_iqr: (f64, f64),
    /// 5th percentile of held-out Sharpe.
    pub sharpe_p05: f64,
    /// 95th percentile of held-out Sharpe.
    pub sharpe_p95: f64,
    /// Median held-out DSR.
    pub median_dsr: f64,
    /// The lower ([`DEFAULT_DSR_PERCENTILE`]) percentile of held-out DSR — the figure the gate decides on.
    pub dsr_p05: f64,
    /// Fraction of held-out paths whose DSR ≥ the floor (`dsr_floor`).
    pub frac_dsr_ge_floor: f64,
    /// Number of held-out paths (balanced partitions) the distribution was built from.
    pub n_paths: usize,
}

impl CpcvDistribution {
    /// Reduce a set of per-path held-out **return series** to the distribution summary: each path's Sharpe
    /// ([`sharpe_ratio`]) and DSR ([`deflated_sharpe_ratio`], deflated against `trial_variance` /
    /// `n_trials`), then the median / IQR / percentile summary and the `DSR ≥ dsr_floor` fraction.
    ///
    /// An empty input (under-powered / degenerate geometry) yields a neutral all-zero summary with
    /// `n_paths = 0` — the fail-closed marker the gate rejects.
    #[must_use]
    pub fn from_path_returns(
        path_returns: &[Vec<f64>],
        trial_variance: f64,
        n_trials: usize,
        dsr_floor: f64,
    ) -> Self {
        let sharpes: Vec<f64> = path_returns.iter().map(|r| sharpe_ratio(r)).collect();
        let dsrs: Vec<f64> = path_returns
            .iter()
            .map(|r| deflated_sharpe_ratio(r, trial_variance, n_trials))
            .collect();
        let n_paths = path_returns.len();
        let frac_dsr_ge_floor = if n_paths == 0 {
            0.0
        } else {
            dsrs.iter().filter(|&&d| d >= dsr_floor).count() as f64 / n_paths as f64
        };
        CpcvDistribution {
            median_sharpe: percentile(&sharpes, 0.5),
            sharpe_iqr: (percentile(&sharpes, 0.25), percentile(&sharpes, 0.75)),
            sharpe_p05: percentile(&sharpes, 0.05),
            sharpe_p95: percentile(&sharpes, 0.95),
            median_dsr: percentile(&dsrs, 0.5),
            dsr_p05: percentile(&dsrs, DEFAULT_DSR_PERCENTILE),
            frac_dsr_ge_floor,
            n_paths,
            sharpes,
            dsrs,
        }
    }

    /// Build the CPCV distribution end-to-end for a fixed `candidate` return series: generate the leak-free
    /// held-out paths ([`cpcv_paths`]) then reduce ([`from_path_returns`](Self::from_path_returns)). The
    /// single entry point the seal path calls.
    ///
    /// # Errors
    /// Propagates [`cpcv_paths`] errors (odd/`< 2` block count, series shorter than `blocks`).
    #[allow(clippy::too_many_arguments)] // a convenience constructor threading the geometry + deflation basis
    pub fn build(
        candidate: &[f64],
        blocks: usize,
        lookback: usize,
        label_horizon: usize,
        embargo: usize,
        trial_variance: f64,
        n_trials: usize,
        dsr_floor: f64,
    ) -> Result<Self, ValidationError> {
        let paths = cpcv_paths(candidate.len(), blocks, lookback, label_horizon, embargo)?;
        let path_returns: Vec<Vec<f64>> = paths.iter().map(|p| p.returns(candidate)).collect();
        Ok(Self::from_path_returns(
            &path_returns,
            trial_variance,
            n_trials,
            dsr_floor,
        ))
    }
}

/// The fail-closed CPCV promotion gate (QE-469): the OOS verdict clears **iff** the distribution is
/// powered (`n_paths ≥ min_paths`) **and** the **lower** `dsr_percentile` of the held-out DSR distribution
/// clears `dsr_floor`. An under-powered / degenerate distribution (too few paths) **fails closed** —
/// mirrors `qe_wfo::gp::deflation::GpDeflationGate::passes` returning `false` when PBO is absent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpcvGate {
    /// Minimum held-out paths for a powered verdict; below it the gate rejects (fail-closed).
    pub min_paths: usize,
    /// The lower percentile of the held-out DSR distribution the gate reads (∈ `[0, 1]`).
    pub dsr_percentile: f64,
    /// The DSR floor that lower percentile must clear.
    pub dsr_floor: f64,
}

impl Default for CpcvGate {
    fn default() -> Self {
        CpcvGate {
            min_paths: DEFAULT_MIN_PATHS,
            dsr_percentile: DEFAULT_DSR_PERCENTILE,
            dsr_floor: DEFAULT_DSR_FLOOR,
        }
    }
}

impl CpcvGate {
    /// Whether `dist` clears the gate: powered **and** its lower `dsr_percentile` DSR ≥ `dsr_floor`.
    #[must_use]
    pub fn passes(&self, dist: &CpcvDistribution) -> bool {
        if dist.n_paths < self.min_paths {
            return false; // under-powered ⇒ fail closed
        }
        percentile(&dist.dsrs, self.dsr_percentile) >= self.dsr_floor
    }
}

/// The `q`-quantile (`q ∈ [0, 1]`) of `xs` by linear interpolation between order statistics — a
/// deterministic, RNG-free estimator (sort by `total_cmp` so `NaN`s never corrupt the order). Returns
/// `0.0` for an empty input (the neutral value that makes an empty distribution fail the gate closed).
#[must_use]
fn percentile(xs: &[f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = xs.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    if sorted.len() == 1 {
        return sorted[0];
    }
    let q = q.clamp(0.0, 1.0);
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_count_matches_lopez_de_prado_formula() {
        // φ(S) = C(S−1, S/2−1); n_splits = C(S, S/2).
        assert_eq!(balanced_partition_count(2), 2); // C(2,1)
        assert_eq!(cpcv_path_count(2), 1); //           C(1,0)
        assert_eq!(balanced_partition_count(4), 6); // C(4,2)
        assert_eq!(cpcv_path_count(4), 3); //           C(3,1) — ≥ 2 paths at S=4
        assert_eq!(balanced_partition_count(6), 20); // C(6,3)
        assert_eq!(cpcv_path_count(6), 10); //           C(5,2)
        assert_eq!(balanced_partition_count(8), 70); // C(8,4)
        assert_eq!(cpcv_path_count(8), 35); //           C(7,3)
                                            // Odd / tiny counts are rejected (0).
        assert_eq!(balanced_partition_count(3), 0);
        assert_eq!(cpcv_path_count(0), 0);
    }

    #[test]
    fn cpcv_paths_are_window_disjoint_under_purge_embargo() {
        // THE LEAKAGE AC: every held-out path is window-disjoint from its own training blocks under
        // purge + embargo — the `PurgedKFold` invariant, generalised to a multi-block held-out group.
        let lookback = 5;
        let label_horizon = 2;
        let embargo = lookback; // documented PurgedKFold default
        let paths = cpcv_paths(240, 6, lookback, label_horizon, embargo).unwrap();
        assert_eq!(paths.len(), 20, "C(6,3) held-out configurations");
        for p in &paths {
            assert_eq!(p.test.len(), 3, "S/2 = 3 held-out blocks per path");
            assert!(!p.train.is_empty(), "a powered split has a real train set");
            assert!(
                p.windows_disjoint(lookback, label_horizon),
                "held-out ranges {:?} leak within the lookback+horizon span",
                p.test
            );
        }

        // Non-vacuous control: with NO purge/embargo, adjacent train/test bars leak (windows_disjoint
        // false somewhere) — proving the purge is what buys the invariant.
        let naive = cpcv_paths(240, 6, 0, 0, 0).unwrap();
        assert!(
            naive
                .iter()
                .any(|p| !p.windows_disjoint(lookback, label_horizon)),
            "un-purged CPCV must leak under a non-zero lookback"
        );
    }

    #[test]
    fn cpcv_paths_reject_odd_or_too_few_blocks() {
        assert!(matches!(
            cpcv_paths(240, 3, 1, 1, 1),
            Err(ValidationError::OddBlockCount(3))
        ));
        assert!(matches!(
            cpcv_paths(240, 0, 1, 1, 1),
            Err(ValidationError::OddBlockCount(0))
        ));
        // Fewer observations than blocks ⇒ a block would be empty ⇒ fail closed.
        assert!(matches!(
            cpcv_paths(4, 6, 1, 1, 1),
            Err(ValidationError::EmptyMatrix)
        ));
    }

    #[test]
    fn cpcv_path_set_is_deterministic() {
        // Two calls with identical inputs produce byte-identical path sets (RNG-free geometry).
        let a = cpcv_paths(300, 6, 4, 1, 4).unwrap();
        let b = cpcv_paths(300, 6, 4, 1, 4).unwrap();
        assert_eq!(a, b, "CPCV geometry must be deterministic");
    }

    #[test]
    fn distribution_summarises_median_iqr_percentiles_and_dsr_floor_fraction() {
        // A candidate with a modest, consistent per-period edge over a long series: split into 6 blocks
        // and reduce to the distribution. Sharpe/DSR summaries are populated and internally ordered.
        let candidate: Vec<f64> = (0..600)
            .map(|i| 0.01 + 0.02 * ((i % 5) as f64 - 2.0))
            .collect();
        let dist = CpcvDistribution::build(&candidate, 6, 4, 1, 4, 0.02, 20_000, DEFAULT_DSR_FLOOR)
            .unwrap();
        assert_eq!(dist.n_paths, 20, "C(6,3) held-out paths");
        assert_eq!(dist.sharpes.len(), 20);
        assert_eq!(dist.dsrs.len(), 20);
        // Ordering invariants of the summary percentiles.
        assert!(dist.sharpe_p05 <= dist.sharpe_iqr.0);
        assert!(dist.sharpe_iqr.0 <= dist.median_sharpe);
        assert!(dist.median_sharpe <= dist.sharpe_iqr.1);
        assert!(dist.sharpe_iqr.1 <= dist.sharpe_p95);
        assert!(dist.dsr_p05 <= dist.median_dsr);
        assert!((0.0..=1.0).contains(&dist.frac_dsr_ge_floor));

        // Direct reducer check on a hand-built per-path Sharpe set via one-length "series" is not
        // meaningful (Sharpe needs dispersion); instead assert the percentile helper on a known set.
        let known = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&known, 0.5), 3.0);
        assert_eq!(percentile(&known, 0.0), 1.0);
        assert_eq!(percentile(&known, 1.0), 5.0);
        assert_eq!(percentile(&known, 0.25), 2.0);
        assert_eq!(percentile(&[], 0.5), 0.0, "empty ⇒ neutral 0.0");
    }

    #[test]
    fn empty_distribution_is_neutral_and_fails_the_gate_closed() {
        let dist = CpcvDistribution::from_path_returns(&[], 0.02, 100, DEFAULT_DSR_FLOOR);
        assert_eq!(dist.n_paths, 0);
        assert_eq!(dist.frac_dsr_ge_floor, 0.0);
        assert!(
            !CpcvGate::default().passes(&dist),
            "an empty (degenerate) distribution must fail closed"
        );
    }

    #[test]
    fn gate_decides_on_lower_percentile_and_fails_closed() {
        // Construct DSR vectors directly to isolate the gate's percentile decision.
        let gate = CpcvGate {
            min_paths: 4,
            dsr_percentile: 0.05,
            dsr_floor: 0.95,
        };
        let dist = |dsrs: Vec<f64>| CpcvDistribution {
            n_paths: dsrs.len(),
            sharpes: vec![0.0; dsrs.len()],
            median_sharpe: 0.0,
            sharpe_iqr: (0.0, 0.0),
            sharpe_p05: 0.0,
            sharpe_p95: 0.0,
            median_dsr: percentile(&dsrs, 0.5),
            dsr_p05: percentile(&dsrs, 0.05),
            frac_dsr_ge_floor: 0.0,
            dsrs,
        };

        // Uniformly strong ⇒ passes (lower percentile also clears the floor).
        assert!(gate.passes(&dist(vec![0.99; 8])));

        // Median clears 0.95 but the lower tail does NOT ⇒ rejected (the gate reads the lower percentile,
        // not the mean/median).
        let mut skewed = vec![0.99; 8];
        skewed[0] = 0.10; // one poor held-out path drags the 5th percentile below the floor
        let d = dist(skewed);
        assert!(d.median_dsr >= 0.95, "median still clears the floor");
        assert!(
            !gate.passes(&d),
            "but the lower percentile does not ⇒ reject"
        );

        // Too few paths ⇒ fail closed regardless of how strong they are.
        assert!(!gate.passes(&dist(vec![0.999, 0.999, 0.999])));
    }
}
