//! Correlation penalty + per-regime expectancy constraint (QE-127).
//!
//! QE-126 searches for tail-aware, wide-basin ensembles on **blended** history. This module adds the
//! regime half: an ensemble must be net-positive in *each* labelled regime (QE-125), not only on average,
//! and highly P&L-correlated combinations are penalised (the QE-115 correlation penalty, already inside
//! [`objective`], rides along). [`regime_aware_objective`] subtracts a shortfall penalty for any regime
//! whose expectancy falls below a floor; [`search_portfolio_regime_aware`] runs the shared QE-126 DE
//! engine with the fold-cross-validated, leave-one-out form of that score, so the search converges on an
//! ensemble that is robust **and** regime-positive, net-of-cost.

use qe_signal::{expectancy_table, ExpectancyTable, Regime};

use crate::objective::{combined_returns, objective};
use crate::search::{corr_penalty_effective_n, run_de, SearchConfig, SearchResult};

/// Default per-regime expectancy floor — an ensemble should be net-positive in every regime.
pub const DEFAULT_REGIME_FLOOR: f64 = 0.0;
/// Default penalty multiplier on the worst-regime shortfall below the floor.
pub const DEFAULT_REGIME_WEIGHT: f64 = 10.0;

/// Configuration for the regime-aware portfolio score / search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegimeAwareConfig {
    /// The QE-126 DE + cross-validation + base-objective configuration.
    pub search: SearchConfig,
    /// Minimum acceptable per-regime expectancy (mean combined return). Default `0` = net-positive.
    pub regime_floor: f64,
    /// Penalty multiplier on `max(0, regime_floor − worst_regime_expectancy)`.
    pub regime_weight: f64,
}

impl Default for RegimeAwareConfig {
    fn default() -> Self {
        RegimeAwareConfig {
            search: SearchConfig::with_defaults(),
            regime_floor: DEFAULT_REGIME_FLOOR,
            regime_weight: DEFAULT_REGIME_WEIGHT,
        }
    }
}

impl RegimeAwareConfig {
    /// The QE-127 default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        RegimeAwareConfig::default()
    }
}

/// The per-regime expectancy table of an ensemble's equal-weight combined **net-of-cost** returns
/// (QE-127/D1), bucketed by the QE-125 `labels`. `labels` are paired with the combined series by index
/// (QE-125's contract). Empty membership ⇒ an empty table.
#[must_use]
pub fn per_regime_expectancy(
    pool: &[Vec<f64>],
    members: &[usize],
    labels: &[Option<Regime>],
) -> ExpectancyTable {
    let combined = combined_returns(pool, members);
    expectancy_table(&combined, labels)
}

/// The ensemble's weakest regime: the minimum `mean_return` across the table's regime rows. `+∞` when
/// there are no labelled rows (no regime information ⇒ no regime penalty).
#[must_use]
pub fn worst_regime_expectancy(table: &ExpectancyTable) -> f64 {
    table
        .rows
        .iter()
        .map(|r| r.mean_return)
        .fold(f64::INFINITY, f64::min)
}

/// The regime-aware objective (QE-127/D2): the QE-115 [`objective`] (which already carries the
/// correlation penalty) **minus** `regime_weight · max(0, regime_floor − worst_regime_expectancy)`. A
/// regime whose expectancy falls below the floor is penalised in proportion to the shortfall, so a
/// regime-fragile ensemble (net-positive on blended history but net-negative in some regime) is ranked
/// down. With no labelled regimes the penalty is `0` and this equals the base objective.
#[must_use]
pub fn regime_aware_objective(
    pool: &[Vec<f64>],
    members: &[usize],
    labels: &[Option<Regime>],
    cfg: &RegimeAwareConfig,
) -> f64 {
    let base = objective(pool, members, &cfg.search.objective);
    if members.is_empty() || !base.is_finite() {
        return base;
    }
    let table = per_regime_expectancy(pool, members, labels);
    let worst = worst_regime_expectancy(&table);
    let shortfall = (cfg.regime_floor - worst).max(0.0);
    base - cfg.regime_weight * shortfall
}

/// The wide-basin floor over the regime-aware objective (QE-127/D3, mirroring QE-115's
/// `leave_one_out_min`): the worst single-member-removed regime-aware objective. Single-member ensembles
/// return their own regime-aware objective.
#[must_use]
pub fn leave_one_out_min_regime(
    pool: &[Vec<f64>],
    members: &[usize],
    labels: &[Option<Regime>],
    cfg: &RegimeAwareConfig,
) -> f64 {
    if members.len() <= 1 {
        return regime_aware_objective(pool, members, labels, cfg);
    }
    let mut worst = f64::INFINITY;
    for drop in 0..members.len() {
        let reduced: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != drop)
            .map(|(_, &m)| m)
            .collect();
        worst = worst.min(regime_aware_objective(pool, &reduced, labels, cfg));
    }
    worst
}

/// The regime-aware robust-basin CV score (QE-127/D3): the min across `cfg.search.folds` contiguous time
/// folds of [`leave_one_out_min_regime`], slicing **both** the pool and the `labels` per fold so each
/// fold scores its own regimes. Empty membership ⇒ `−∞`.
#[must_use]
pub fn regime_aware_cv_score(
    pool: &[Vec<f64>],
    members: &[usize],
    labels: &[Option<Regime>],
    cfg: &RegimeAwareConfig,
) -> f64 {
    if members.is_empty() {
        return f64::NEG_INFINITY;
    }
    let t = pool.iter().map(Vec::len).min().unwrap_or(0);
    if t == 0 {
        return leave_one_out_min_regime(pool, members, labels, cfg);
    }
    let k = cfg.search.folds.max(1).min(t);
    let mut worst = f64::INFINITY;
    for f in 0..k {
        let lo = f * t / k;
        let hi = (f + 1) * t / k;
        if hi <= lo {
            continue;
        }
        let sliced: Vec<Vec<f64>> = pool
            .iter()
            .map(|s| s[lo..hi.min(s.len())].to_vec())
            .collect();
        let sliced_labels = &labels[lo.min(labels.len())..hi.min(labels.len())];
        worst = worst.min(leave_one_out_min_regime(
            &sliced,
            members,
            sliced_labels,
            cfg,
        ));
    }
    worst
}

/// Run the discrete-DE portfolio search with the regime-aware robust-basin score (QE-127): the DE
/// converges on an ensemble that is wide-basin, correlation-diverse, **and** net-positive in every
/// labelled regime, net-of-cost. Deterministic in `seed`.
#[must_use]
pub fn search_portfolio_regime_aware(
    pool: &[Vec<f64>],
    labels: &[Option<Regime>],
    cfg: &RegimeAwareConfig,
    seed: u64,
) -> SearchResult {
    let mut result = run_de(
        pool.len(),
        cfg.search.pop_size,
        cfg.search.generations,
        cfg.search.cr,
        cfg.search.init_density,
        seed,
        |members| regime_aware_cv_score(pool, members, labels, cfg),
    );
    result.corr_effective_n = corr_penalty_effective_n(pool, &result.best.members(), &cfg.search);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objective::ObjectiveConfig;
    use qe_signal::{Regime, TrendState, VolState};

    fn cfg() -> RegimeAwareConfig {
        RegimeAwareConfig {
            search: SearchConfig {
                pop_size: 16,
                generations: 30,
                folds: 4,
                ..SearchConfig::default()
            },
            ..RegimeAwareConfig::default()
        }
    }

    const A: Regime = Regime {
        vol: VolState::Calm,
        trend: TrendState::Trending,
    };
    const B: Regime = Regime {
        vol: VolState::Volatile,
        trend: TrendState::Choppy,
    };

    /// Labels: first half regime A, second half regime B.
    fn ab_labels(t: usize) -> Vec<Option<Regime>> {
        (0..t)
            .map(|i| Some(if i < t / 2 { A } else { B }))
            .collect()
    }

    #[test]
    fn highly_correlated_combinations_are_penalised() {
        // Two anti-correlated series vs the same series duplicated (corr = 1).
        let a: Vec<f64> = (0..40)
            .map(|i| if i % 2 == 0 { 0.01 } else { -0.01 })
            .collect();
        let anti: Vec<f64> = a.iter().map(|v| -v).collect();
        let oc = ObjectiveConfig::with_defaults();

        let decorrelated = objective(&[a.clone(), anti], &[0, 1], &oc);
        let correlated = objective(&[a.clone(), a.clone()], &[0, 1], &oc);
        assert!(
            decorrelated > correlated,
            "the correlation penalty must rank the decorrelated pair higher: \
             decorrelated={decorrelated} correlated={correlated}"
        );

        // Isolate the correlation term itself: on the *same* highly-correlated pair, turning the
        // correlation weight on strictly lowers the score vs turning it off — so the penalty (not the
        // tail term) is what punishes correlation.
        let with_corr = ObjectiveConfig {
            corr_weight: 1.0,
            ..ObjectiveConfig::with_defaults()
        };
        let without_corr = ObjectiveConfig {
            corr_weight: 0.0,
            ..ObjectiveConfig::with_defaults()
        };
        let corr_pair = [a.clone(), a.clone()];
        assert!(
            objective(&corr_pair, &[0, 1], &with_corr)
                < objective(&corr_pair, &[0, 1], &without_corr),
            "the correlation penalty (corr_weight) specifically lowers a correlated combo's score"
        );
    }

    #[test]
    fn per_regime_expectancy_is_part_of_the_score() {
        // An ensemble that is positive in regime A but negative in regime B.
        let t = 80;
        let s: Vec<f64> = (0..t)
            .map(|i| if i < t / 2 { 0.01 } else { -0.02 })
            .collect();
        let pool = vec![s];
        let labels = ab_labels(t);
        let c = cfg();

        // The per-regime table exposes the negative regime …
        let table = per_regime_expectancy(&pool, &[0], &labels);
        assert_eq!(table.rows.len(), 2);
        assert!(table.row(B).unwrap().mean_return < 0.0);
        assert!(table.row(A).unwrap().mean_return > 0.0);

        // … and the regime-aware score is strictly below the base objective (the table changed it).
        let base = objective(&pool, &[0], &c.search.objective);
        let aware = regime_aware_objective(&pool, &[0], &labels, &c);
        assert!(
            aware < base,
            "regime shortfall must lower the score: base={base} aware={aware}"
        );
    }

    #[test]
    fn all_regime_positive_ensemble_keeps_its_base_score() {
        let t = 80;
        let s: Vec<f64> = vec![0.01; t]; // positive in every regime
        let pool = vec![s];
        let labels = ab_labels(t);
        let c = cfg();
        let base = objective(&pool, &[0], &c.search.objective);
        let aware = regime_aware_objective(&pool, &[0], &labels, &c);
        assert!(
            (aware - base).abs() < 1e-12,
            "no shortfall ⇒ score unchanged"
        );
    }

    /// Labels interleaved bar-by-bar (even = A, odd = B), so *every* time-fold contains both regimes —
    /// regime fragility is spread across time, the case fold-CV alone cannot localise.
    fn interleaved_labels(t: usize) -> Vec<Option<Regime>> {
        (0..t)
            .map(|i| Some(if i % 2 == 0 { A } else { B }))
            .collect()
    }

    #[test]
    fn regime_fragile_strategy_is_rejected_by_the_search() {
        // Strategy 0: steady, net-positive in both regimes.
        // Strategy 1: strongly positive in regime A but net-negative in regime B (regime-fragile),
        // with the fragility interleaved across time so no single fold is uniformly bad.
        let t = 160;
        let robust: Vec<f64> = vec![0.01; t];
        let fragile: Vec<f64> = (0..t)
            .map(|i| if i % 2 == 0 { 0.05 } else { -0.04 })
            .collect();
        let pool = vec![robust, fragile];
        let labels = interleaved_labels(t);
        let c = cfg();

        // The fragile strategy is genuinely strong in regime A (it is tempting, not merely bad) …
        let frag_table = per_regime_expectancy(&pool, &[1], &labels);
        assert!(frag_table.row(A).unwrap().mean_return > 0.0);
        assert!(frag_table.row(B).unwrap().mean_return < 0.0);

        // … but its regime-B shortfall makes its regime-aware score far worse than the robust strategy's.
        let aware_robust = regime_aware_cv_score(&pool, &[0], &labels, &c);
        let aware_fragile = regime_aware_cv_score(&pool, &[1], &labels, &c);
        assert!(
            aware_robust > aware_fragile,
            "regime-aware scoring must penalise the fragile strategy: robust={aware_robust} fragile={aware_fragile}"
        );

        // Isolate the regime constraint's *added* value over QE-126: turning the regime weight on
        // strictly lowers the fragile strategy's score vs off — the regime shortfall, not the base
        // fold-CV tail term, is what this ticket contributes.
        let no_regime = RegimeAwareConfig {
            regime_weight: 0.0,
            ..c
        };
        assert!(
            regime_aware_cv_score(&pool, &[1], &labels, &c)
                < regime_aware_cv_score(&pool, &[1], &labels, &no_regime),
            "the regime penalty (regime_weight) specifically lowers the fragile strategy's score"
        );

        // And the regime-aware search excludes the fragile strategy.
        let result = search_portfolio_regime_aware(&pool, &labels, &c, 2024);
        assert!(result.best.0[0], "robust strategy should be selected");
        assert!(
            !result.best.0[1],
            "fragile strategy must be excluded: {:?}",
            result.best
        );
    }

    #[test]
    fn regime_aware_search_is_deterministic() {
        let t = 120;
        let robust: Vec<f64> = vec![0.01; t];
        let fragile: Vec<f64> = (0..t)
            .map(|i| if i < t / 2 { 0.05 } else { -0.04 })
            .collect();
        let pool = vec![robust, fragile];
        let labels = ab_labels(t);
        let c = cfg();
        let a = search_portfolio_regime_aware(&pool, &labels, &c, 42);
        let b = search_portfolio_regime_aware(&pool, &labels, &c, 42);
        assert_eq!(a, b);
    }
}
