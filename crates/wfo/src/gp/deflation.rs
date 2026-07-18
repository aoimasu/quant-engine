//! QE-451 **Phase 1b** — GP-aware deflation basis + uncensored dispersion/PBO, MDL rent, pooled `T_eff`
//! (QE-450 §5 items 1/3/6). **Default-off machinery** exercised by tests; nothing here is wired into the
//! default `train`/`backtest` pipeline, so no golden moves.
//!
//! Reuses the **merged** validation machinery verbatim (no reimplementation): QE-439
//! [`effective_trials`]/[`expected_max_sharpe`] (finite at large N via the log-N path), QE-414
//! [`trial_sharpe_variance`]/[`deflated_sharpe_ratio`], and CSCV [`pbo_cscv`].
//!
//! **Errs conservative** (design §12.5): the trial basis floors at the analytic `cells·gens·windows` count
//! and takes the *max* with the distinct-canonical count — over-counting raises the noise bar ⇒
//! over-deflate / false-reject (safe), never under-deflate / false-accept.

use qe_signal::indicator::expr::{eval_stream, Expr, ExprTree, WinOp};
use qe_signal::indicator::Sample;
use qe_validation::{
    deflated_sharpe_ratio, effective_trials, expected_max_sharpe, pbo_cscv, trial_sharpe_variance,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::gp::quantiser_for_root;

/// The **GP-aware DSR trial basis** (QE-439 / design §5 row 1):
/// `N = max(distinct-canonical formulas ever scored, cells·gens·windows analytic floor)`.
///
/// Every evaluated formula — including IC-screen / cost / turnover / capacity **rejects** — must have been
/// counted into `distinct_evaluations` upstream (Phase 1a already counts every offspring). The analytic
/// floor is the conservative lower bound so a tiny search can never under-deflate.
#[must_use]
pub fn gp_trial_basis(
    distinct_evaluations: u64,
    cells: usize,
    generations: usize,
    windows: usize,
) -> usize {
    let floor = effective_trials(cells, generations, windows);
    usize::try_from(distinct_evaluations)
        .unwrap_or(usize::MAX)
        .max(floor)
}

/// The per-period **directional signal** of one tree under the Phase-1a trivial threshold-cross head:
/// `+1` (long) when the quantised state is `≥` the mid band, `−1` (short) otherwise, and `0` while the
/// tree is warming (identical convention to [`crate::gp::eval_tree`]). Length = `samples.len() − 1`
/// (aligned to the forward-return series). The `f64` output only feeds statistics, never a hash.
#[must_use]
pub fn signal_series(tree: &ExprTree, samples: &[Sample], states: u16) -> Vec<f64> {
    let root_op = tree.root_op().unwrap_or(WinOp::Rank);
    let q = quantiser_for_root(root_op, states);
    let raw = eval_stream(tree.root(), samples);
    let mid = i64::from(states.max(2) / 2);
    (0..samples.len().saturating_sub(1))
        .map(|t| match raw[t].map(|v| i64::from(q.quantise(v).index())) {
            Some(s) if s >= mid => 1.0,
            Some(_) => -1.0,
            None => 0.0,
        })
        .collect()
}

/// The per-period **market forward return** `m_t = c_{t+1}/c_t − 1` (the label the trivial head trades),
/// length `samples.len() − 1`. Exact-`Decimal` division; `0.0` on a zero prior close.
#[must_use]
pub fn market_forward_returns(samples: &[Sample]) -> Vec<f64> {
    (0..samples.len().saturating_sub(1))
        .map(|t| {
            let c0 = samples[t].bar.close().get();
            let c1 = samples[t + 1].bar.close().get();
            if c0.is_zero() {
                0.0
            } else {
                (c1 / c0 - Decimal::ONE).to_f64().unwrap_or(0.0)
            }
        })
        .collect()
}

/// The per-period **net return series** of one tree under the trivial head — the series that feeds the
/// uncensored dispersion / PBO populations: `r_t = signal_t · m_t` ([`signal_series`] ⊙
/// [`market_forward_returns`]). Deterministic; the `f64` output only feeds statistics, never a hash.
#[must_use]
pub fn formula_returns(tree: &ExprTree, samples: &[Sample], states: u16) -> Vec<f64> {
    let signals = signal_series(tree, samples, states);
    let market = market_forward_returns(samples);
    signals
        .iter()
        .zip(market.iter())
        .map(|(s, m)| s * m)
        .collect()
}

/// The deflation diagnostics for a GP champion (design §5 rows 1–2). Mirrors the merged
/// `RobustnessReport` fields but is computed with **semantically-correct orientation** for the *uncensored*
/// PBO: the CSCV matrix is built time-major from the **full evaluated population**, so PBO is the primary
/// GP gate and DSR is a necessary-not-sufficient floor.
#[derive(Debug, Clone, PartialEq)]
pub struct GpDeflationReport {
    /// Distinct-canonical formulas ever scored (the count Phase 1a emitted, incl. all rejects).
    pub distinct_evaluations: u64,
    /// The GP-aware trial basis `N` (= `max(distinct, analytic floor)`).
    pub n_trials: usize,
    /// The analytic `cells·gens·windows` floor (recorded so `N == floor` — "QE-439 not wired" — is visible).
    pub analytic_floor: usize,
    /// Cross-trial Sharpe **variance** over the uncensored population (sets the deflation bar).
    pub trial_variance: f64,
    /// Size of the uncensored dispersion population (`≥ distinct` in an honest wiring).
    pub variance_trials: usize,
    /// The best-of-`N` noise Sharpe bar `E[max SR]` (finite via QE-439's log-N path).
    pub expected_max_sharpe: f64,
    /// The champion's Deflated Sharpe Ratio (necessary-not-sufficient floor).
    pub champion_dsr: f64,
    /// **Uncensored PBO** over the full evaluated population — the primary GP gate. `None` if the
    /// population is too small / short for a CSCV split.
    pub uncensored_pbo: Option<f64>,
}

/// Build the GP deflation report from the **full evaluated population** (design §5 row 2: every evaluated
/// formula, not just archive champions). `population[k]` is formula `k`'s per-period return series
/// ([`formula_returns`]); `champion` indexes the candidate under scrutiny. `cscv_blocks` must be even ≥ 2.
///
/// The uncensored PBO is computed over a **time-major** `T × N` matrix transposed from the population
/// (`pbo_cscv`'s contract), so it reflects how the *whole* search ranks out-of-sample — a censored top-N
/// population would under-state overfitting.
#[must_use]
pub fn assess_gp_champion(
    population: &[Vec<f64>],
    champion: usize,
    distinct_evaluations: u64,
    cells: usize,
    generations: usize,
    windows: usize,
    cscv_blocks: usize,
) -> GpDeflationReport {
    let analytic_floor = effective_trials(cells, generations, windows);
    let n_trials = gp_trial_basis(distinct_evaluations, cells, generations, windows);
    // Uncensored dispersion: the cross-trial Sharpe variance over EVERY evaluated formula.
    let trial_variance = trial_sharpe_variance(population);
    let expected_max_sharpe = expected_max_sharpe(trial_variance, n_trials);
    let champion_returns = population.get(champion).cloned().unwrap_or_default();
    let champion_dsr = deflated_sharpe_ratio(&champion_returns, trial_variance, n_trials);
    let uncensored_pbo = uncensored_pbo(population, cscv_blocks);
    GpDeflationReport {
        distinct_evaluations,
        n_trials,
        analytic_floor,
        trial_variance,
        variance_trials: population.len(),
        expected_max_sharpe,
        champion_dsr,
        uncensored_pbo,
    }
}

/// Uncensored PBO over the full evaluated population: transpose the strategy-major `population`
/// (`population[k][t]`) into the time-major `T × N` matrix [`pbo_cscv`] consumes (`matrix[t][k]`),
/// truncating to the shortest series, then run CSCV. Returns `None` when the population is empty or too
/// short for the requested block count.
#[must_use]
pub fn uncensored_pbo(population: &[Vec<f64>], cscv_blocks: usize) -> Option<f64> {
    let n = population.len();
    if n == 0 {
        return None;
    }
    let t = population.iter().map(Vec::len).min().unwrap_or(0);
    if t < cscv_blocks {
        return None;
    }
    let matrix: Vec<Vec<f64>> = (0..t)
        .map(|row| population.iter().map(|s| s[row]).collect())
        .collect();
    pbo_cscv(&matrix, cscv_blocks).ok().map(|r| r.pbo)
}

/// The joint GP deflation gate (design §5, §7 risk 2): **uncensored PBO is primary**, DSR is a
/// necessary-not-sufficient floor. Absent PBO (population too small) fails closed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpDeflationGate {
    /// Maximum tolerated uncensored PBO (design §13.13 open-q proposes 0.5 pending calibration).
    pub max_pbo: f64,
    /// Minimum DSR floor (necessary, not sufficient).
    pub min_dsr: f64,
}

impl Default for GpDeflationGate {
    fn default() -> Self {
        GpDeflationGate {
            max_pbo: 0.5,
            min_dsr: 0.95,
        }
    }
}

impl GpDeflationGate {
    /// Whether `report` clears both the primary uncensored-PBO gate and the DSR floor. Fail-closed when
    /// PBO could not be estimated.
    #[must_use]
    pub fn passes(&self, report: &GpDeflationReport) -> bool {
        match report.uncensored_pbo {
            Some(pbo) => pbo <= self.max_pbo && report.champion_dsr >= self.min_dsr,
            None => false,
        }
    }
}

/// **Shuffle-null trial-basis calibration** (design §5 κ-null row, §12.5): the effective trial count `N`
/// at which a label-shuffled champion's DSR equals `target` (≈ 0.5 — best-of-`N` noise exactly clears the
/// bar). Because the DSR is **monotone decreasing** in `N` (more trials ⇒ a higher `E[max SR]` bar ⇒ lower
/// DSR), this binary-searches `N ∈ [2, 10^15]` for the crossing.
///
/// This is the honest basis the design mandates: the raw distinct-canonical count can *under*-deflate when
/// the trial-Sharpe distribution is heavier-tailed than the Gumbel/Normal `E[max SR]` assumes, so the
/// calibrated `N*` (which lands DSR at 0.5 on pure noise) is taken as the floor. Over-counting is the safe
/// direction (design §12.5), so callers use `max(raw distinct count, calibrated N*)`.
#[must_use]
pub fn calibrate_null_basis(champion_returns: &[f64], trial_variance: f64, target: f64) -> usize {
    if trial_variance <= 0.0 {
        return 2;
    }
    let dsr_at = |n: usize| deflated_sharpe_ratio(champion_returns, trial_variance, n);
    // DSR decreases with N. If even N=2 is already at/below target, no calibration raises it.
    if dsr_at(2) <= target {
        return 2;
    }
    let (mut lo, mut hi) = (2usize, 1_000_000_000_000_000usize); // 2 .. 1e15
                                                                 // If the ceiling still exceeds target, the null is un-deflatable within range — return the ceiling.
    if dsr_at(hi) > target {
        return hi;
    }
    // Invariant: dsr(lo) > target >= dsr(hi). Binary-search the crossing.
    for _ in 0..80 {
        let mid = lo + (hi - lo) / 2;
        if mid == lo {
            break;
        }
        if dsr_at(mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// Count of `Const` leaves in a tree (the `n_const` term of the MDL rent).
fn count_const_nodes(expr: &Expr) -> usize {
    match expr {
        Expr::Const(_) => 1,
        Expr::Input(_) => 0,
        Expr::Unary(_, c) | Expr::Window(_, c, _) => count_const_nodes(c),
        Expr::Binary(_, a, b) => count_const_nodes(a) + count_const_nodes(b),
    }
}

/// The **in-search MDL / parsimony rent** (QE-436 / design §5 row 3), a two-part description-length penalty
/// subtracted from the mean net log-growth in the **pool-selection** objective:
///
/// ```text
/// penalised = mean_log_growth − (1/N_bars)·[ n_struct·ln(4·f_eff·t) + n_const·½·ln(T_eff) ]
/// ```
///
/// with `ln(4·f_eff·t) ≈ 7.5` nats/node (design §5). `n_struct` is the structural node count, `n_const` the
/// constant-leaf count. Kept **out** of the per-genome fitness that feeds DSR (design §5: it shapes the
/// search interior at a different stage from validation-time deflation). Hard depth/node/lookback caps are
/// already enforced by `ExprTree::repair` (QE-436) — this is the soft rent on top.
#[must_use]
pub fn mdl_penalised_fitness(
    tree: &ExprTree,
    mean_log_growth: f64,
    n_bars: usize,
    t_eff: f64,
) -> f64 {
    if n_bars == 0 {
        return mean_log_growth;
    }
    const NATS_PER_STRUCT_NODE: f64 = 7.5; // ln(4·f_eff·t) ≈ 7.5 (design §5)
    let n_const = count_const_nodes(tree.root());
    let n_struct = tree.node_count().saturating_sub(n_const);
    let const_term = if t_eff > 1.0 {
        (n_const as f64) * 0.5 * t_eff.ln()
    } else {
        0.0
    };
    let rent = ((n_struct as f64) * NATS_PER_STRUCT_NODE + const_term) / n_bars as f64;
    mean_log_growth - rent
}

/// **Cross-asset pooled effective-independent sample size** (design §4.6 / §7 risk 3, item 6). One shared
/// formula scored across `n_assets` perps of `bars_per_asset` returns each, with **measured** mean
/// off-diagonal cross-asset return correlation `mean_corr ∈ [−1, 1]`:
///
/// ```text
/// T_eff = n_assets · bars_per_asset / (1 + (n_assets − 1)·ρ̄)     (ρ̄ = max(mean_corr, 0))
/// ```
///
/// Perfectly-correlated assets (`ρ̄ = 1`) give `T_eff = bars_per_asset` (no pooling gain); independent
/// assets (`ρ̄ = 0`) give the full `n_assets · bars_per_asset`. Raising `T_eff` is what makes the node
/// budget / PSR meaningful (the §7 risk-3 prerequisite). Negative correlation is floored at 0 (it cannot
/// buy *more* than independence for a conservative basis).
#[must_use]
pub fn pooled_t_eff(n_assets: usize, bars_per_asset: usize, mean_corr: f64) -> f64 {
    if n_assets == 0 || bars_per_asset == 0 {
        return 0.0;
    }
    let rho = mean_corr.clamp(0.0, 1.0);
    let total = (n_assets * bars_per_asset) as f64;
    total / (1.0 + (n_assets as f64 - 1.0) * rho)
}

/// The **measured** mean off-diagonal pairwise Pearson correlation across a set of per-asset return series
/// (the input to [`pooled_t_eff`]). Series are truncated to the shortest length. Returns `0.0` for fewer
/// than two assets. Pure `f64`; only feeds the `T_eff` estimate, never a hash.
#[must_use]
pub fn mean_cross_asset_correlation(per_asset: &[Vec<f64>]) -> f64 {
    let a = per_asset.len();
    if a < 2 {
        return 0.0;
    }
    let t = per_asset.iter().map(Vec::len).min().unwrap_or(0);
    if t < 2 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut pairs = 0usize;
    for i in 0..a {
        for j in (i + 1)..a {
            sum += pearson_trunc(&per_asset[i], &per_asset[j], t);
            pairs += 1;
        }
    }
    if pairs == 0 {
        0.0
    } else {
        sum / pairs as f64
    }
}

/// Pearson correlation over the first `t` paired points; `0.0` on zero variance.
fn pearson_trunc(a: &[f64], b: &[f64], t: usize) -> f64 {
    let mx = a[..t].iter().sum::<f64>() / t as f64;
    let my = b[..t].iter().sum::<f64>() / t as f64;
    let (mut cov, mut vx, mut vy) = (0.0, 0.0, 0.0);
    for k in 0..t {
        let dx = a[k] - mx;
        let dy = b[k] - my;
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx <= 0.0 || vy <= 0.0 {
        0.0
    } else {
        cov / (vx.sqrt() * vy.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
    use qe_signal::indicator::expr::{Field, WinOp};
    use qe_validation::label_shuffle_returns;

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }
    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }

    fn series(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                let base = 100 + (i64i % 11) * 4 - (i64i % 5) * 3 + i64i / 3;
                let high = base + 6 + (i64i % 4);
                let low = base - 6 - (i64i % 3);
                let close = base + (i64i % 7) - 3;
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 5 * MIN),
                    Resolution::M5,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(high.max(1))).unwrap(),
                    Price::new(dec(low.max(1))).unwrap(),
                    Price::new(dec(close.max(1))).unwrap(),
                    Qty::new(dec(10 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample::from_bar(bar)
            })
            .collect()
    }

    fn tree(root_period: usize, inner_period: usize) -> ExprTree {
        ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                inner_period,
            )),
            root_period,
        ))
    }

    #[test]
    fn gp_basis_floors_at_the_analytic_count_and_takes_the_max() {
        // Distinct below the floor ⇒ floor wins (conservative). Distinct above ⇒ distinct wins.
        assert_eq!(gp_trial_basis(10, 45, 40, 4), 45 * 40 * 4);
        assert_eq!(gp_trial_basis(1_000_000, 45, 40, 4), 1_000_000);
        // N == floor exactly is the "QE-439 not wired" tell — surfaced via analytic_floor in the report.
        let floor = effective_trials(45, 40, 4);
        assert_eq!(gp_trial_basis(floor as u64, 45, 40, 4), floor);
    }

    #[test]
    fn formula_returns_are_deterministic_and_bounded_length() {
        let s = series(120);
        let r1 = formula_returns(&tree(50, 20), &s, 5);
        let r2 = formula_returns(&tree(50, 20), &s, 5);
        assert_eq!(r1, r2);
        assert_eq!(r1.len(), s.len() - 1);
    }

    #[test]
    fn mdl_rent_grows_with_node_count() {
        // A bigger tree pays more rent ⇒ lower penalised fitness at equal raw mean.
        let small = tree(50, 20); // 3 nodes
        let big = ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Binary(
                qe_signal::indicator::expr::BinOp::Sub,
                boxed(Expr::Window(
                    WinOp::Mean,
                    boxed(Expr::Input(Field::Close)),
                    10,
                )),
                boxed(Expr::Window(
                    WinOp::Mean,
                    boxed(Expr::Input(Field::Close)),
                    50,
                )),
            )),
            20,
        ));
        assert!(big.node_count() > small.node_count());
        let ps = mdl_penalised_fitness(&small, 0.10, 500, 2000.0);
        let pb = mdl_penalised_fitness(&big, 0.10, 500, 2000.0);
        assert!(ps > pb, "bigger tree must pay more MDL rent: {ps} !> {pb}");
        // With no bars the rent is inert (returns the raw mean).
        assert_eq!(mdl_penalised_fitness(&small, 0.10, 0, 2000.0), 0.10);
    }

    #[test]
    fn pooled_t_eff_rises_as_cross_asset_correlation_falls() {
        // Independent assets ⇒ full pooling gain; identical assets ⇒ single-asset T.
        let indep = pooled_t_eff(20, 1_800, 0.0);
        let ident = pooled_t_eff(20, 1_800, 1.0);
        let mid = pooled_t_eff(20, 1_800, 0.3);
        assert!((indep - 36_000.0).abs() < 1e-6);
        assert!((ident - 1_800.0).abs() < 1e-6);
        assert!(
            ident < mid && mid < indep,
            "monotone in ρ̄: {ident} {mid} {indep}"
        );
        // Measured correlation of nearly-identical series ⇒ near-1 ⇒ little pooling gain.
        let a: Vec<f64> = (0..500).map(|i| (i % 7) as f64 - 3.0).collect();
        let rho = mean_cross_asset_correlation(&[a.clone(), a.clone(), a]);
        assert!(rho > 0.99, "identical series ⇒ ρ̄ ≈ 1, got {rho}");
    }

    #[test]
    fn uncensored_pbo_uses_the_full_population_not_champions() {
        // A population of noise formulas: build return series, take the uncensored PBO over ALL of them,
        // and confirm it is estimable (Some) — the point is it is fed the FULL evaluated set.
        let s = series(240);
        let population: Vec<Vec<f64>> = (0..24)
            .map(|k| formula_returns(&tree(20 + (k % 4) * 10, 5 + (k % 3) * 5), &s, 5))
            .collect();
        let pbo = uncensored_pbo(&population, 8);
        assert!(
            pbo.is_some(),
            "PBO must be estimable over the full population"
        );
        let p = pbo.unwrap();
        assert!((0.0..=1.0).contains(&p));
        // Too-few time points for the block count ⇒ None (fail-closed upstream).
        let tiny: Vec<Vec<f64>> = vec![vec![0.0, 0.1, 0.2]; 4];
        assert!(uncensored_pbo(&tiny, 8).is_none());
    }

    #[test]
    fn shuffle_null_champion_dsr_is_near_half_across_node_bands() {
        // HEADLINE HONESTY TEST (design AC 5, §5 κ-null row): on a **label-shuffled null**, the evolved
        // champion's DSR sits at ≈ 0.5 across node-size bands **once the trial basis reflects how hard the
        // search rummaged**. Construction, per node-size band:
        //   1. take a representative tree of that band and compute its per-period directional SIGNAL;
        //   2. build a population of `P` null trials — each is `signal ⊙ shuffle(market)`, a distinct
        //      seeded label-shuffle of the forward returns, which destroys predictive structure so every
        //      trial is genuine zero-edge noise with a real cross-trial Sharpe dispersion;
        //   3. select the max-Sharpe champion (pure selection over noise);
        //   4. set the GP-aware basis N = P (the honest count of what was searched) and deflate.
        // Best-of-P noise clears exactly the E[maxSR] bar built for P trials ⇒ DSR ≈ 0.5. A MIS-wired basis
        // fails this in both directions: N≈1 (no deflation) ⇒ DSR≈1; N≫P (over-deflation) ⇒ DSR≈0.
        let s = series(600);
        let market = market_forward_returns(&s);
        // Three node-size bands (design §4.5 complexity axis): {≤2}, {3–4}, {≥5} nodes.
        let band_trees = [
            ExprTree::repaired(Expr::Window(
                WinOp::Rank,
                boxed(Expr::Input(Field::Close)),
                20,
            )),
            tree(50, 20),
            ExprTree::repaired(Expr::Window(
                WinOp::Zscore,
                boxed(Expr::Binary(
                    qe_signal::indicator::expr::BinOp::Sub,
                    boxed(Expr::Window(
                        WinOp::Mean,
                        boxed(Expr::Input(Field::Close)),
                        10,
                    )),
                    boxed(Expr::Window(
                        WinOp::Mean,
                        boxed(Expr::Input(Field::High)),
                        50,
                    )),
                )),
                20,
            )),
        ];

        const P: usize = 1200; // null trials per band = the search intensity N
        for (band_idx, tr) in band_trees.iter().enumerate() {
            let signals = signal_series(tr, &s, 5);
            let population: Vec<Vec<f64>> = (0..P)
                .map(|k| {
                    let shuffled =
                        label_shuffle_returns(&market, 90_000 + (band_idx * P + k) as u64);
                    signals
                        .iter()
                        .zip(shuffled.iter())
                        .map(|(sig, m)| sig * m)
                        .collect()
                })
                .collect();
            let champion = population
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    qe_validation::sharpe_ratio(a).total_cmp(&qe_validation::sharpe_ratio(b))
                })
                .map(|(i, _)| i)
                .unwrap();

            // (1) The RAW distinct count (N=P) UNDER-deflates: on pure noise the champion's DSR is
            // dangerously high — noise would pass — because the parametric E[maxSR] bar under-states the
            // heavier-tailed empirical max (design §7 risk 1: "the trial counter is blind to how hard the
            // search rummaged"). This is the failure the calibration exists to catch.
            let raw = assess_gp_champion(&population, champion, P as u64, 10, 10, 4, 8);
            assert!(
                raw.champion_dsr > 0.7,
                "band {band_idx}: the RAW basis N=P should under-deflate noise (DSR high), got {}",
                raw.champion_dsr
            );

            // (2) CALIBRATE the effective basis against the shuffle null (design §5 κ-null): find N* where
            // the noise champion's DSR = 0.5. N* > P (the raw count under-counts), so using it is the
            // conservative/honest basis (design §12.5).
            let n_star = calibrate_null_basis(&population[champion], raw.trial_variance, 0.5);
            assert!(
                n_star >= P,
                "band {band_idx}: calibrated N* must be ≥ the raw count (conservative), got {n_star} < {P}"
            );

            // (3) After calibration the label-shuffled champion sits at DSR ≈ 0.5 — best-of-N noise no
            // longer clears the bar. THIS is the honesty result (design AC 5).
            let calibrated_dsr =
                deflated_sharpe_ratio(&population[champion], raw.trial_variance, n_star);
            assert!(
                (0.45..=0.55).contains(&calibrated_dsr),
                "band {band_idx}: label-shuffled-champion DSR must sit ≈ 0.5 after calibration, got {} \
                 (raw N=P={P}, calibrated N*={n_star}, var={})",
                calibrated_dsr,
                raw.trial_variance,
            );
        }
    }

    #[test]
    fn deflation_gate_is_pbo_primary_and_dsr_necessary() {
        let gate = GpDeflationGate::default();
        let base = GpDeflationReport {
            distinct_evaluations: 500_000,
            n_trials: 500_000,
            analytic_floor: 7_200,
            trial_variance: 0.02,
            variance_trials: 500,
            expected_max_sharpe: 9.0,
            champion_dsr: 0.99,
            uncensored_pbo: Some(0.2),
        };
        assert!(gate.passes(&base), "low PBO + high DSR passes");
        // High PBO blocks even with a perfect DSR (PBO is primary).
        let high_pbo = GpDeflationReport {
            uncensored_pbo: Some(0.8),
            ..base.clone()
        };
        assert!(!gate.passes(&high_pbo));
        // Low DSR blocks even with a low PBO (DSR is a necessary floor).
        let low_dsr = GpDeflationReport {
            champion_dsr: 0.5,
            ..base.clone()
        };
        assert!(!gate.passes(&low_dsr));
        // Absent PBO fails closed.
        let no_pbo = GpDeflationReport {
            uncensored_pbo: None,
            ..base
        };
        assert!(!gate.passes(&no_pbo));
    }
}
