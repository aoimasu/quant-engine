//! Discrete differential-evolution portfolio search (QE-126).
//!
//! QE-115 fixed the operators ([`de_mutant`], [`binomial_crossover`]) and the tail-aware, wide-basin
//! [`objective`]; this module is the **search loop** that drives them — DE/rand/1/bin over the binary
//! [`EnsembleMask`], with elitist (greedy) selection so the best-so-far is monotonic, and a fold
//! cross-validated, leave-one-out score so the search converges on a **robust basin** rather than a sharp
//! peak. All scoring is on the pool's **net-of-cost** return series (QE-115/D3) — no gross path is ever
//! reintroduced. The whole loop is driven by one seeded `DetRng` (QE-006), so a seed reproduces the run
//! exactly. Correlation/regime constraints (QE-127) and capacity (QE-128) layer on top later.

use qe_determinism::{seed_rng, DetRng};
use rand_core::RngCore;

use crate::de::{binomial_crossover, de_mutant, EnsembleMask, DEFAULT_CR};
use crate::objective::{leave_one_out_min, ObjectiveConfig};

/// Default DE population size.
pub const DEFAULT_POP_SIZE: usize = 32;
/// Default number of generations.
pub const DEFAULT_GENERATIONS: usize = 40;
/// Default number of cross-validation folds.
pub const DEFAULT_FOLDS: usize = 4;
/// Default probability a bit is set in an initial random mask.
pub const DEFAULT_INIT_DENSITY: f64 = 0.5;

/// Configuration for [`search_portfolio`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchConfig {
    /// DE population size (clamped to `≥ 4` so three distinct donors always exist).
    pub pop_size: usize,
    /// Number of generations.
    pub generations: usize,
    /// Binomial-crossover rate.
    pub cr: f64,
    /// Cross-validation folds for the robust-basin score.
    pub folds: usize,
    /// Probability a bit is set in an initial random mask.
    pub init_density: f64,
    /// The QE-115 ensemble objective configuration.
    pub objective: ObjectiveConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            pop_size: DEFAULT_POP_SIZE,
            generations: DEFAULT_GENERATIONS,
            cr: DEFAULT_CR,
            folds: DEFAULT_FOLDS,
            init_density: DEFAULT_INIT_DENSITY,
            objective: ObjectiveConfig::with_defaults(),
        }
    }
}

impl SearchConfig {
    /// The QE-126 default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        SearchConfig::default()
    }
}

/// The outcome of [`search_portfolio`].
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// The best ensemble mask found.
    pub best: EnsembleMask,
    /// Its cross-validated robust-basin score.
    pub score: f64,
    /// Generations actually run.
    pub generations_run: usize,
    /// Best score after each generation — a monotonic non-decreasing convergence trace.
    pub history: Vec<f64>,
}

/// Cross-validated robust-basin score of `members` over `pool` (QE-126/D2): partition the common time
/// axis into `cfg.folds` contiguous folds, take the [`leave_one_out_min`] wide-basin floor **within each
/// fold**, and return the **minimum across folds** — the worst-fold, worst-member-dropped objective. An
/// empty membership scores `−∞`.
#[must_use]
pub fn cross_val_score(pool: &[Vec<f64>], members: &[usize], cfg: &SearchConfig) -> f64 {
    if members.is_empty() {
        return f64::NEG_INFINITY;
    }
    let t = pool.iter().map(Vec::len).min().unwrap_or(0);
    if t == 0 {
        // No time axis to validate over — fall back to the whole (empty) series score.
        return leave_one_out_min(pool, members, &cfg.objective);
    }
    let k = cfg.folds.max(1).min(t);
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
        worst = worst.min(leave_one_out_min(&sliced, members, &cfg.objective));
    }
    worst
}

/// A uniform `[0, 1)` draw from one `u64`.
fn uniform01(rng: &mut DetRng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A random mask over `pool_size` loci, each set with probability `density`, repaired to `≥ 1` member.
fn random_mask(pool_size: usize, density: f64, rng: &mut DetRng) -> EnsembleMask {
    let bits: Vec<bool> = (0..pool_size).map(|_| uniform01(rng) < density).collect();
    repair_nonempty(EnsembleMask(bits), rng)
}

/// Ensure a mask has at least one member (an empty ensemble scores `−∞`): if empty, set one random locus.
fn repair_nonempty(mut mask: EnsembleMask, rng: &mut DetRng) -> EnsembleMask {
    if !mask.is_empty() && mask.count() == 0 {
        let j = (rng.next_u64() % mask.len() as u64) as usize;
        mask.0[j] = true;
    }
    mask
}

/// Pick three distinct indices in `0..np`, all different from `target`. Requires `np ≥ 4`.
fn pick_three_distinct(np: usize, target: usize, rng: &mut DetRng) -> (usize, usize, usize) {
    let mut picks = [0usize; 3];
    let mut chosen = 0;
    while chosen < 3 {
        let cand = (rng.next_u64() % np as u64) as usize;
        if cand != target && !picks[..chosen].contains(&cand) {
            picks[chosen] = cand;
            chosen += 1;
        }
    }
    (picks[0], picks[1], picks[2])
}

/// Run the discrete-DE portfolio search over `pool` (per-strategy net-of-cost return series),
/// maximising the cross-validated robust-basin score. Deterministic in `seed`.
#[must_use]
pub fn search_portfolio(pool: &[Vec<f64>], cfg: &SearchConfig, seed: u64) -> SearchResult {
    let pool_size = pool.len();
    if pool_size == 0 {
        return SearchResult {
            best: EnsembleMask(Vec::new()),
            score: f64::NEG_INFINITY,
            generations_run: 0,
            history: Vec::new(),
        };
    }

    let np = cfg.pop_size.max(4);
    let mut rng = seed_rng(seed);

    let mut pop: Vec<EnsembleMask> = (0..np)
        .map(|_| random_mask(pool_size, cfg.init_density, &mut rng))
        .collect();
    let mut scores: Vec<f64> = pop
        .iter()
        .map(|m| cross_val_score(pool, &m.members(), cfg))
        .collect();

    let mut history = Vec::with_capacity(cfg.generations);
    for _gen in 0..cfg.generations {
        for i in 0..np {
            let (a, b, c) = pick_three_distinct(np, i, &mut rng);
            let mutant = de_mutant(&pop[a], &pop[b], &pop[c]);
            let trial = repair_nonempty(
                binomial_crossover(&pop[i], &mutant, cfg.cr, &mut rng),
                &mut rng,
            );
            let ts = cross_val_score(pool, &trial.members(), cfg);
            if ts >= scores[i] {
                pop[i] = trial;
                scores[i] = ts;
            }
        }
        let best = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        history.push(best);
    }

    let (bi, &bs) = scores
        .iter()
        .enumerate()
        .max_by(|(_, x), (_, y)| x.total_cmp(y))
        .unwrap_or((0, &f64::NEG_INFINITY));

    SearchResult {
        best: pop[bi].clone(),
        score: bs,
        generations_run: cfg.generations,
        history,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small config that converges fast on the fixtures.
    fn cfg() -> SearchConfig {
        SearchConfig {
            pop_size: 16,
            generations: 30,
            folds: 4,
            ..SearchConfig::default()
        }
    }

    /// Three anti-correlated, trough-filling diversifiers plus one negative-mean, fat-tailed "bad"
    /// strategy. On a period-3 cycle each good strategy spikes (`+0.02`) on **two** of the three slots
    /// and sits at `0.0` on its own trough slot, so every *pair*'s combined return strictly lifts the
    /// worst slot above any singleton's (whose CVaR is `0`), and the *triple* is constant (no tail). The
    /// pairwise correlations are negative ⇒ the QE-115 correlation penalty floors at `0`. So the robust
    /// basin is the full triple, and `leave_one_out_min` (which drops to a 2-member combo) still beats
    /// every singleton — the structure the wide-basin floor rewards. The bad strategy (index 3) has
    /// negative mean and a `−0.4` shock every 10 bars, so any mask including it scores far lower.
    fn fixture_pool(t: usize) -> Vec<Vec<f64>> {
        // spike on the two slots != trough_slot.
        let good = |trough_slot: usize| -> Vec<f64> {
            (0..t)
                .map(|i| if i % 3 == trough_slot { 0.0 } else { 0.02 })
                .collect()
        };
        let bad: Vec<f64> = (0..t)
            .map(|i| if i % 10 == 0 { -0.4 } else { -0.005 })
            .collect();
        vec![good(0), good(1), good(2), bad]
    }

    #[test]
    fn converges_to_a_robust_basin_excluding_the_bad_strategy() {
        let pool = fixture_pool(160);
        let c = cfg();
        let result = search_portfolio(&pool, &c, 2024);

        // (a) Greedy selection ⇒ the best-so-far trace never decreases.
        for w in result.history.windows(2) {
            assert!(
                w[1] >= w[0],
                "best score must be monotonic: {:?}",
                result.history
            );
        }
        // (b) The fat-tailed, negative-mean strategy (index 3) is excluded.
        assert!(
            !result.best.0[3],
            "bad strategy must be excluded: {:?}",
            result.best
        );
        // (c) A genuine ensemble (a basin), not a lucky singleton …
        assert!(
            result.best.count() >= 2,
            "expected an ensemble: {:?}",
            result.best
        );
        // … whose CV score beats every single-strategy ensemble.
        let best_singleton = (0..pool.len())
            .map(|i| cross_val_score(&pool, &[i], &c))
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            result.score > best_singleton,
            "basin score {} should beat best singleton {best_singleton}",
            result.score
        );
    }

    #[test]
    fn scoring_is_net_of_cost() {
        let pool = fixture_pool(160);
        let c = cfg();
        let gross = search_portfolio(&pool, &c, 7).score;

        // Subtract a per-period cost from every series → strictly worse converged score.
        let costed: Vec<Vec<f64>> = pool
            .iter()
            .map(|s| s.iter().map(|r| r - 0.004).collect())
            .collect();
        let net = search_portfolio(&costed, &c, 7).score;
        assert!(
            net < gross,
            "net-of-cost scoring must drop with cost: net={net} gross={gross}"
        );
    }

    #[test]
    fn search_is_deterministic() {
        let pool = fixture_pool(120);
        let c = cfg();
        let a = search_portfolio(&pool, &c, 42);
        let b = search_portfolio(&pool, &c, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_pool_yields_empty_mask() {
        let result = search_portfolio(&[], &cfg(), 1);
        assert!(result.best.is_empty());
        assert_eq!(result.score, f64::NEG_INFINITY);
    }

    #[test]
    fn single_good_strategy_is_selected() {
        let pool = vec![(0..120)
            .map(|i| if i % 7 == 0 { -0.01 } else { 0.012 })
            .collect()];
        let result = search_portfolio(&pool, &cfg(), 5);
        assert_eq!(result.best.count(), 1);
        assert!(result.best.0[0]);
        assert!(result.score.is_finite());
    }
}
