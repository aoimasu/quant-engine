//! Adaptive operator selection (QE-112) — the credit-assignment bandit that allocates search budget
//! across the variation operators.
//!
//! Three operators compete as emitters (Colas 2020 multi-emitter MAP-Elites): [`Operator::LocalRefine`]
//! (exploit), [`Operator::Explore`] and [`Operator::FreshRandom`] (explore). Each application earns an
//! **in-training** reward — archive novelty ([`ApplicationOutcome::NewCell`]) or in-training fitness
//! gain ([`ApplicationOutcome::ImprovedElite`]) — and the [`OperatorSelector`] shifts budget toward
//! whichever operators are currently productive, via a sliding-window reward bandit with an
//! exploration floor.
//!
//! **Emergent exploration/exploitation (QE-112/D3).** The selector is never told the archive density.
//! On a sparse archive exploratory operators fill new cells (high novelty reward) so budget shifts to
//! exploration; on a dense archive `LocalRefine` keeps improving elites so budget shifts to
//! exploitation. The shift falls out of the reward, not a hard-coded rule.
//!
//! **Information firewall (QE-112/D4).** [`ApplicationOutcome`] has no field for validation / holdout
//! / live performance — the bandit *cannot* be fed an out-of-sample reward by construction. QE-121
//! enforces the same for parent selection; QE-132 makes it a CI guard.
//!
//! This module fixes the operators and the credit signal; the genome-level variation each operator
//! performs is QE-119, and archive insertion / fitness are QE-118 / QE-120.

use std::collections::VecDeque;

use rand_core::RngCore;

/// Reward for an offspring that fills a previously-empty niche (archive novelty).
pub const NOVELTY_REWARD: f64 = 1.0;
/// Default sliding-window length — how many recent applications define an operator's current credit.
pub const DEFAULT_WINDOW: usize = 64;
/// Default exploration floor added to every operator's weight so none ever starves.
pub const DEFAULT_EPSILON: f64 = 0.05;

/// A variation operator competing for search budget (QE-112/D1). Genome-level mechanics are QE-119.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operator {
    /// Small perturbation around a parent — exploitation (fine-tune an elite).
    LocalRefine,
    /// Aggressive multi-locus mutation — exploration (jump to a new niche).
    Explore,
    /// A brand-new random genome, no parent — maximal exploration (escape the basin).
    FreshRandom,
}

/// All operators in fixed order (also their bandit-arm index order).
pub const OPERATORS: [Operator; 3] = [
    Operator::LocalRefine,
    Operator::Explore,
    Operator::FreshRandom,
];

impl Operator {
    /// This operator's arm index in `0..OPERATORS.len()`.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Operator::LocalRefine => 0,
            Operator::Explore => 1,
            Operator::FreshRandom => 2,
        }
    }

    /// Whether this operator is exploratory (`Explore` / `FreshRandom`) rather than exploitative
    /// (`LocalRefine`).
    #[must_use]
    pub fn is_exploratory(self) -> bool {
        matches!(self, Operator::Explore | Operator::FreshRandom)
    }
}

/// The in-training effect of one operator application on the archive — the *only* thing the bandit is
/// credited with (QE-112/D4). Deliberately carries **no** out-of-sample / validation field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApplicationOutcome {
    /// The offspring filled a previously-empty niche (archive novelty).
    NewCell,
    /// The offspring improved an occupied cell's elite by `gain` (in-training fitness delta, ≥ 0,
    /// normalised to a novelty-comparable scale by the caller — QE-118/120).
    ImprovedElite {
        /// Normalised in-training fitness improvement.
        gain: f64,
    },
    /// The offspring neither filled a cell nor improved an elite.
    NoImprovement,
}

impl ApplicationOutcome {
    /// The scalar reward credited to the operator that produced this outcome.
    #[must_use]
    pub fn reward(&self) -> f64 {
        match self {
            ApplicationOutcome::NewCell => NOVELTY_REWARD,
            ApplicationOutcome::ImprovedElite { gain } => gain.max(0.0),
            ApplicationOutcome::NoImprovement => 0.0,
        }
    }
}

/// A sliding-window reward bandit over the [`OPERATORS`] (QE-112/D2). Tracks each operator's *current*
/// productivity (mean of its last `window` rewards) and selects proportionally to credit plus an
/// `epsilon` exploration floor. Deterministic through a seeded [`RngCore`].
#[derive(Debug, Clone)]
pub struct OperatorSelector {
    window: usize,
    epsilon: f64,
    recent: [VecDeque<f64>; OPERATORS.len()],
}

impl OperatorSelector {
    /// Build a selector with an explicit sliding-window length and exploration floor.
    ///
    /// `window` is clamped to ≥ 1 and `epsilon` to a small positive minimum so selection weights are
    /// always strictly positive (no operator can starve, no divide-by-zero).
    #[must_use]
    pub fn new(window: usize, epsilon: f64) -> Self {
        OperatorSelector {
            window: window.max(1),
            epsilon: epsilon.max(f64::MIN_POSITIVE),
            recent: Default::default(),
        }
    }

    /// A selector with the QE-112 defaults (`DEFAULT_WINDOW`, `DEFAULT_EPSILON`).
    #[must_use]
    pub fn with_defaults() -> Self {
        OperatorSelector::new(DEFAULT_WINDOW, DEFAULT_EPSILON)
    }

    /// Credit an operator with the reward of one application's `outcome`, evicting the oldest reward
    /// once the window is full.
    pub fn record(&mut self, op: Operator, outcome: &ApplicationOutcome) {
        let q = &mut self.recent[op.index()];
        q.push_back(outcome.reward());
        while q.len() > self.window {
            q.pop_front();
        }
    }

    /// An operator's current credit = the mean of its recent-window rewards (0 if it has none yet).
    #[must_use]
    pub fn credit(&self, op: Operator) -> f64 {
        let q = &self.recent[op.index()];
        if q.is_empty() {
            return 0.0;
        }
        q.iter().sum::<f64>() / q.len() as f64
    }

    /// Per-operator selection weight = `max(credit, 0) + epsilon` (strictly positive).
    fn weight(&self, op: Operator) -> f64 {
        self.credit(op).max(0.0) + self.epsilon
    }

    /// The normalised selection probability of each operator, in [`OPERATORS`] order. At cold start
    /// (no rewards) every operator weighs `epsilon`, so the distribution is uniform.
    #[must_use]
    pub fn probabilities(&self) -> [f64; OPERATORS.len()] {
        let weights = OPERATORS.map(|op| self.weight(op));
        let total: f64 = weights.iter().sum();
        weights.map(|w| w / total)
    }

    /// Select an operator by its credit-proportional probability, drawing one sample from `rng`.
    /// Deterministic for a given RNG state (QE-006).
    pub fn select<R: RngCore>(&self, rng: &mut R) -> Operator {
        let probs = self.probabilities();
        // Uniform in [0, 1) from 53 high bits of one u64 draw — portable, no float-distribution dep.
        let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        let mut acc = 0.0;
        for (op, p) in OPERATORS.iter().zip(probs.iter()) {
            acc += *p;
            if u < acc {
                return *op;
            }
        }
        // Fallthrough only on floating-point round-off at the top of the range.
        Operator::FreshRandom
    }
}

impl Default for OperatorSelector {
    fn default() -> Self {
        OperatorSelector::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_determinism::seed_rng;

    fn improved(gain: f64) -> ApplicationOutcome {
        ApplicationOutcome::ImprovedElite { gain }
    }

    #[test]
    fn cold_start_is_uniform_and_deterministic() {
        let sel = OperatorSelector::with_defaults();
        let p = sel.probabilities();
        for prob in p {
            assert!((prob - 1.0 / 3.0).abs() < 1e-12);
        }
        // select is reproducible for a fixed seed.
        let mut a = seed_rng(42);
        let mut b = seed_rng(42);
        for _ in 0..50 {
            assert_eq!(sel.select(&mut a), sel.select(&mut b));
        }
    }

    #[test]
    fn reward_maps_outcomes() {
        assert_eq!(ApplicationOutcome::NewCell.reward(), NOVELTY_REWARD);
        assert_eq!(improved(0.4).reward(), 0.4);
        assert_eq!(improved(-3.0).reward(), 0.0); // clamped ≥ 0
        assert_eq!(ApplicationOutcome::NoImprovement.reward(), 0.0);
    }

    #[test]
    fn credit_tracks_reward_and_window_forgets() {
        let mut sel = OperatorSelector::new(4, DEFAULT_EPSILON);
        for _ in 0..4 {
            sel.record(Operator::Explore, &ApplicationOutcome::NewCell);
        }
        assert!((sel.credit(Operator::Explore) - 1.0).abs() < 1e-12);
        // Four NoImprovement evict the four NewCell rewards (window = 4) → credit back to 0.
        for _ in 0..4 {
            sel.record(Operator::Explore, &ApplicationOutcome::NoImprovement);
        }
        assert_eq!(sel.credit(Operator::Explore), 0.0);
        // Untouched operators stay at 0.
        assert_eq!(sel.credit(Operator::LocalRefine), 0.0);
    }

    /// One adaptive round: select an operator, ask the scenario model for its in-training outcome,
    /// and credit it. `sparse` decides which operators are currently productive.
    fn run_simulation(sparse: bool, rounds: usize, seed: u64) -> OperatorSelector {
        let mut sel = OperatorSelector::with_defaults();
        let mut rng = seed_rng(seed);
        for _ in 0..rounds {
            let op = sel.select(&mut rng);
            let outcome = if sparse {
                // Empty niches everywhere → exploratory ops fill cells; refine finds nothing new.
                if op.is_exploratory() {
                    ApplicationOutcome::NewCell
                } else {
                    ApplicationOutcome::NoImprovement
                }
            } else {
                // Archive full → only refinement of existing elites pays off.
                if op.is_exploratory() {
                    ApplicationOutcome::NoImprovement
                } else {
                    improved(1.0)
                }
            };
            sel.record(op, &outcome);
        }
        sel
    }

    #[test]
    fn sparse_archive_shifts_budget_to_exploration() {
        let sel = run_simulation(true, 500, 7);
        let p = sel.probabilities();
        let explore = p[Operator::Explore.index()];
        let fresh = p[Operator::FreshRandom.index()];
        let refine = p[Operator::LocalRefine.index()];
        // Exploratory share rose well above the uniform 2/3, and each exploratory op beats refine.
        assert!(
            explore + fresh > 0.85,
            "exploration share = {}",
            explore + fresh
        );
        assert!(explore > refine);
        assert!(fresh > refine);
    }

    #[test]
    fn dense_archive_shifts_budget_to_exploitation() {
        let sel = run_simulation(false, 500, 7);
        let p = sel.probabilities();
        let refine = p[Operator::LocalRefine.index()];
        let explore = p[Operator::Explore.index()];
        // Same machinery, opposite regime: refinement now dominates.
        assert!(refine > 0.85, "exploitation share = {refine}");
        assert!(refine > explore);
    }

    #[test]
    fn epsilon_floor_prevents_starvation_and_allows_recovery() {
        // A heavily-rewarded operator never drives another to probability 0.
        let mut sel = OperatorSelector::new(8, DEFAULT_EPSILON);
        for _ in 0..8 {
            sel.record(Operator::Explore, &ApplicationOutcome::NewCell);
        }
        let p = sel.probabilities();
        assert!(p[Operator::LocalRefine.index()] > 0.0);
        assert!(p[Operator::FreshRandom.index()] > 0.0);
        // A starved operator recovers once it earns reward.
        for _ in 0..8 {
            sel.record(Operator::LocalRefine, &ApplicationOutcome::NewCell);
        }
        assert!(
            (sel.credit(Operator::LocalRefine) - 1.0).abs() < 1e-12,
            "starved operator must recover credit after rewards"
        );
    }

    #[test]
    fn is_exploratory_classification() {
        assert!(!Operator::LocalRefine.is_exploratory());
        assert!(Operator::Explore.is_exploratory());
        assert!(Operator::FreshRandom.is_exploratory());
    }
}
