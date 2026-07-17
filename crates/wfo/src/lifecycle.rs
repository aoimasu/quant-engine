//! Phased-lifecycle quality gate (QE-114) — the filter deciding which evaluated genomes graduate from
//! the search into the (eventual QE-123) strategy repository.
//!
//! A candidate moves through two phases by **evaluation depth**: while shallowly evaluated it is in
//! [`Phase::Exploration`] and is **never persisted** (this is what stops an early *lucky* one-shot
//! candidate); once re-evaluated across enough windows it reaches [`Phase::Exploitation`] and becomes
//! eligible. It then persists only if it clears a [`QualityThreshold`] **robustly** — its lower
//! confidence bound `mean − k_sigma·se` must beat the bar ("exceed threshold + survive exploitation").
//!
//! **Threshold = full validation distribution (spec A1 baseline, QE-114/D3).** The bar is derived from
//! the population's validation fitness distribution. A stricter **train/CV-only** threshold (D4) that
//! avoids selection leaking the validation distribution into the criterion is a documented,
//! ready-to-enable alternative — `from_distribution` is source-agnostic, so switching is just passing a
//! different distribution; not enabled now, per spec fidelity.

use std::cmp::Ordering;

use crate::fitness::{within_noise_band, NoiseRobustFitness, DEFAULT_K_SIGMA};

/// Default exploration→exploitation transition: windows a candidate must be evaluated on to graduate.
pub const DEFAULT_MIN_EXPLOITATION_WINDOWS: usize = 5;
/// Default baseline quantile of the validation distribution a survivor must reach (top quartile).
pub const DEFAULT_QUANTILE: f64 = 0.75;

/// A candidate's lifecycle phase, set by how many windows it has been evaluated on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Shallowly evaluated — never persisted (guards against early lucky candidates).
    Exploration,
    /// Re-evaluated across enough windows — eligible for persistence.
    Exploitation,
}

/// How the quality bar is derived from a fitness distribution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThresholdPolicy {
    /// The `q`-quantile (nearest-rank) of the distribution — persist at/above it.
    Quantile(f64),
    /// `mean + k·sd` of the distribution.
    MeanPlusSigma(f64),
}

/// A concrete quality bar a candidate's fitness must clear.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityThreshold(f64);

impl QualityThreshold {
    /// An explicit threshold value.
    #[must_use]
    pub fn at(value: f64) -> Self {
        QualityThreshold(value)
    }

    /// The threshold value.
    #[must_use]
    pub fn value(self) -> f64 {
        self.0
    }

    /// Derive the bar from a fitness `distribution` under `policy`. Non-finite (ruined) samples are
    /// excluded so they cannot drag the bar; an empty finite distribution yields `+∞` (fail-safe:
    /// nothing can persist).
    #[must_use]
    pub fn from_distribution(distribution: &[f64], policy: ThresholdPolicy) -> Self {
        let mut finite: Vec<f64> = distribution
            .iter()
            .copied()
            .filter(|x| x.is_finite())
            .collect();
        if finite.is_empty() {
            return QualityThreshold(f64::INFINITY);
        }
        let value = match policy {
            ThresholdPolicy::Quantile(q) => {
                finite.sort_by(f64::total_cmp);
                let q = q.clamp(0.0, 1.0);
                let idx = (q * (finite.len() - 1) as f64).round() as usize;
                finite[idx.min(finite.len() - 1)]
            }
            ThresholdPolicy::MeanPlusSigma(k) => {
                let n = finite.len() as f64;
                let mean = finite.iter().sum::<f64>() / n;
                let sd = if finite.len() < 2 {
                    0.0
                } else {
                    let var = finite
                        .iter()
                        .map(|x| {
                            let d = x - mean;
                            d * d
                        })
                        .sum::<f64>()
                        / (n - 1.0);
                    var.sqrt()
                };
                mean + k * sd
            }
        };
        QualityThreshold(value)
    }
}

/// The phased-lifecycle quality gate (QE-114). Holds the threshold policy, the exploration→exploitation
/// transition depth, and the robustness margin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityGate {
    /// How the bar is derived from the distribution.
    pub policy: ThresholdPolicy,
    /// Windows required to reach [`Phase::Exploitation`].
    pub min_exploitation_windows: usize,
    /// Lower-confidence-bound margin (in standard errors) for "survive exploitation".
    pub k_sigma: f64,
    /// **Optional** peak-to-trough drawdown ceiling (QE-446), as a non-negative magnitude in `[0, 1]`
    /// (e.g. `0.30` = reject a graduation candidate whose equity path drew down more than 30 %).
    /// **`None` = OFF (no ceiling)** — the default, so graduation behaviour is byte-identical to the
    /// pre-QE-446 gate unless a ceiling is explicitly configured. Consumed only by
    /// [`persists_with_drawdown`](Self::persists_with_drawdown); the log-growth-only
    /// [`persists`](Self::persists) ignores it entirely.
    pub max_drawdown_ceiling: Option<f64>,
}

impl QualityGate {
    /// Build a gate explicitly. The drawdown ceiling is **OFF** (`None`); opt in with
    /// [`with_drawdown_ceiling`](Self::with_drawdown_ceiling).
    #[must_use]
    pub fn new(policy: ThresholdPolicy, min_exploitation_windows: usize, k_sigma: f64) -> Self {
        QualityGate {
            policy,
            min_exploitation_windows,
            k_sigma,
            max_drawdown_ceiling: None,
        }
    }

    /// Return a copy of this gate with an **optional drawdown ceiling** applied (QE-446) — the
    /// behaviour-changing opt-in. `ceiling` is a non-negative peak-to-trough magnitude in `[0, 1]`
    /// (see [`max_drawdown`](crate::fitness::max_drawdown)); a candidate whose realised drawdown
    /// exceeds it is rejected by [`persists_with_drawdown`](Self::persists_with_drawdown) even if its
    /// log-growth lower bound clears the quality bar. The value is clamped to `[0, 1]`.
    #[must_use]
    pub fn with_drawdown_ceiling(mut self, ceiling: f64) -> Self {
        self.max_drawdown_ceiling = Some(ceiling.clamp(0.0, 1.0));
        self
    }

    /// The QE-114 defaults: top-quartile baseline, 5-window graduation, 1σ robustness margin.
    #[must_use]
    pub fn with_defaults() -> Self {
        QualityGate::new(
            ThresholdPolicy::Quantile(DEFAULT_QUANTILE),
            DEFAULT_MIN_EXPLOITATION_WINDOWS,
            DEFAULT_K_SIGMA,
        )
    }

    /// The candidate's phase from its evaluation depth (`n`).
    #[must_use]
    pub fn phase(&self, fitness: &NoiseRobustFitness) -> Phase {
        if fitness.n >= self.min_exploitation_windows {
            Phase::Exploitation
        } else {
            Phase::Exploration
        }
    }

    /// Derive the quality bar from a fitness `distribution` (baseline: pass the **full validation**
    /// distribution; the train/CV-only alternative passes the train/CV distribution — QE-114/D3–D4).
    #[must_use]
    pub fn threshold(&self, distribution: &[f64]) -> QualityThreshold {
        QualityThreshold::from_distribution(distribution, self.policy)
    }

    /// Whether a candidate `fitness` persists (QE-114/D2): in Exploitation, finite, and its lower
    /// confidence bound `mean − k_sigma·se` clears `threshold`.
    #[must_use]
    pub fn persists(&self, fitness: &NoiseRobustFitness, threshold: &QualityThreshold) -> bool {
        if self.phase(fitness) != Phase::Exploitation {
            return false;
        }
        if !fitness.mean.is_finite() {
            return false;
        }
        let lower = fitness.mean - self.k_sigma * fitness.std_error;
        lower >= threshold.value()
    }

    /// Whether a candidate's `max_drawdown` (a non-negative peak-to-trough magnitude, see
    /// [`max_drawdown`](crate::fitness::max_drawdown)) is within the configured drawdown ceiling
    /// (QE-446). **`true` whenever the ceiling is OFF (`None`)** — the golden-safe default — otherwise
    /// `true` iff `max_drawdown ≤ ceiling`. A non-finite drawdown never passes a set ceiling.
    #[must_use]
    pub fn drawdown_within_ceiling(&self, max_drawdown: f64) -> bool {
        match self.max_drawdown_ceiling {
            None => true,
            Some(ceiling) => max_drawdown.is_finite() && max_drawdown <= ceiling,
        }
    }

    /// Like [`persists`](Self::persists) but **additionally** enforces the optional drawdown ceiling
    /// (QE-446): a candidate persists iff it clears the log-growth quality bar **and**
    /// [`drawdown_within_ceiling`](Self::drawdown_within_ceiling) holds for its realised
    /// `max_drawdown`. When the ceiling is OFF (`None`, the default) this is **exactly** `persists` —
    /// so graduation behaviour is byte-identical unless a ceiling is configured. When a ceiling IS set,
    /// a high-growth / deep-drawdown genome that would otherwise graduate on growth alone is blocked.
    #[must_use]
    pub fn persists_with_drawdown(
        &self,
        fitness: &NoiseRobustFitness,
        max_drawdown: f64,
        threshold: &QualityThreshold,
    ) -> bool {
        self.persists(fitness, threshold) && self.drawdown_within_ceiling(max_drawdown)
    }

    /// The **lifecycle lower bound** `mean − k_sigma·se` — the robust fitness a candidate is judged on
    /// (the quantity [`persists`](Self::persists) compares against the threshold). Exposed so the
    /// parsimony tie-break (QE-436) can rank equal-robust candidates. `−∞` for a ruined fitness.
    #[must_use]
    pub fn robust_lower_bound(&self, fitness: &NoiseRobustFitness) -> f64 {
        if !fitness.mean.is_finite() {
            return f64::NEG_INFINITY;
        }
        fitness.mean - self.k_sigma * fitness.std_error
    }

    /// Rank two graduation candidates **fitness-first, parsimony-second** (QE-436). Returns
    /// [`Ordering::Greater`] when `a` is the better pick, [`Ordering::Less`] when `b` is, and
    /// [`Ordering::Equal`] when they are indistinguishable on both axes.
    ///
    /// A finite candidate always beats a ruined one. When the two are **within the noise band** (equal
    /// robust fitness) the tie breaks toward parsimony — *lower* complexity is better. Otherwise the
    /// higher robust lower bound wins. The MDL/complexity term is a pure tie-break here: it is consulted
    /// only inside the noise band and never alters the robust-fitness ordering, so it stays out of the
    /// DSR-facing fitness.
    #[must_use]
    pub fn graduation_cmp(
        &self,
        a: (&NoiseRobustFitness, u32),
        b: (&NoiseRobustFitness, u32),
    ) -> Ordering {
        let (fa, ca) = a;
        let (fb, cb) = b;
        match (fa.mean.is_finite(), fb.mean.is_finite()) {
            (false, false) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (true, true) => {}
        }
        if within_noise_band(fa, fb, self.k_sigma) {
            // Equal robust fitness: fewer clauses/features (lower complexity) is the better pick.
            return cb.cmp(&ca);
        }
        // Materially different fitness: the higher robust lower bound wins.
        self.robust_lower_bound(fa)
            .total_cmp(&self.robust_lower_bound(fb))
    }

    /// The most parsimonious among `candidates` at the **best robust fitness** (QE-436): picks the
    /// highest robust lower bound, breaking ties *within the noise band* toward the lowest
    /// [`Genome::mdl_complexity`](crate::genome::Genome::mdl_complexity). Candidates are
    /// `(&T, fitness, complexity)`. Deterministic — an exact tie on both axes keeps the earliest in input
    /// order. `None` for an empty input. Note this only *selects among* candidates; it does not change
    /// which candidates [`persists`](Self::persists) admits, so it never moves the graduation set.
    #[must_use]
    pub fn most_parsimonious<'a, T>(
        &self,
        candidates: &[(&'a T, NoiseRobustFitness, u32)],
    ) -> Option<&'a T> {
        let mut best: Option<&(&'a T, NoiseRobustFitness, u32)> = None;
        for cand in candidates {
            match best {
                None => best = Some(cand),
                Some(cur) => {
                    // Strictly better ⇒ take `cand`; Equal/worse ⇒ keep the earlier `cur` (determinism).
                    if self.graduation_cmp((&cand.1, cand.2), (&cur.1, cur.2)) == Ordering::Greater
                    {
                        best = Some(cand);
                    }
                }
            }
        }
        best.map(|(t, _, _)| *t)
    }

    /// The candidates that persist, preserving input order.
    #[must_use]
    pub fn survivors<'a, T>(
        &self,
        candidates: &[(&'a T, NoiseRobustFitness)],
        threshold: &QualityThreshold,
    ) -> Vec<&'a T> {
        candidates
            .iter()
            .filter(|(_, f)| self.persists(f, threshold))
            .map(|(c, _)| *c)
            .collect()
    }
}

impl Default for QualityGate {
    fn default() -> Self {
        QualityGate::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    fn fit(mean: f64, std_error: f64, n: usize) -> NoiseRobustFitness {
        NoiseRobustFitness { mean, std_error, n }
    }

    #[test]
    fn threshold_from_distribution() {
        // Quantile (nearest-rank): 0.5-quantile of 5 points → index round(0.5·4)=2 → 0.2.
        let q = QualityThreshold::from_distribution(
            &[0.0, 0.1, 0.2, 0.3, 0.4],
            ThresholdPolicy::Quantile(0.5),
        );
        approx(q.value(), 0.2);
        // Ruined samples excluded: finite [0.1, 0.3], q=1.0 → 0.3.
        let q2 = QualityThreshold::from_distribution(
            &[f64::NEG_INFINITY, 0.1, 0.3],
            ThresholdPolicy::Quantile(1.0),
        );
        approx(q2.value(), 0.3);
        // Empty finite distribution → +∞ (nothing persists).
        let q3 = QualityThreshold::from_distribution(
            &[f64::NEG_INFINITY],
            ThresholdPolicy::Quantile(0.5),
        );
        assert_eq!(q3.value(), f64::INFINITY);
        // MeanPlusSigma: mean of [0,2] = 1; k=0 → 1.0; k=1 → 1 + sqrt(2).
        approx(
            QualityThreshold::from_distribution(&[0.0, 2.0], ThresholdPolicy::MeanPlusSigma(0.0))
                .value(),
            1.0,
        );
        approx(
            QualityThreshold::from_distribution(&[0.0, 2.0], ThresholdPolicy::MeanPlusSigma(1.0))
                .value(),
            1.0 + 2.0_f64.sqrt(),
        );
    }

    #[test]
    fn phase_transition_on_evaluation_depth() {
        let gate = QualityGate::with_defaults(); // min 5
        assert_eq!(gate.phase(&fit(0.1, 0.0, 1)), Phase::Exploration);
        assert_eq!(gate.phase(&fit(0.1, 0.0, 4)), Phase::Exploration);
        assert_eq!(gate.phase(&fit(0.1, 0.0, 5)), Phase::Exploitation);
    }

    #[test]
    fn early_lucky_candidate_is_not_persisted() {
        // The AC: a candidate with a single huge-fitness window (lucky) must not persist.
        let gate = QualityGate::with_defaults(); // quantile 0.75, min 5, k 1.0
        let dist = [0.0, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07];
        let t = gate.threshold(&dist); // 0.75-quantile → 0.05

        let lucky = fit(1.0, 0.0, 1); // mean far above the bar, but n=1
        assert_eq!(gate.phase(&lucky), Phase::Exploration);
        assert!(
            !gate.persists(&lucky, &t),
            "an early lucky one-shot candidate must not persist"
        );

        // The very same fitness, once graduated (n ≥ 5) and tight, *does* persist — isolating depth
        // as the gate, not the fitness value.
        let graduated = fit(1.0, 0.0, 5);
        assert_eq!(gate.phase(&graduated), Phase::Exploitation);
        assert!(gate.persists(&graduated, &t));
    }

    #[test]
    fn survive_exploitation_requires_robust_lower_bound() {
        let gate = QualityGate::new(ThresholdPolicy::Quantile(0.75), 5, 1.0);
        let t = QualityThreshold::at(0.10);
        // Graduated but noisy: lower bound 0.12 − 0.05 = 0.07 < 0.10 → not persisted.
        assert!(!gate.persists(&fit(0.12, 0.05, 6), &t));
        // Graduated and robust: lower bound 0.20 − 0.02 = 0.18 ≥ 0.10 → persisted.
        assert!(gate.persists(&fit(0.20, 0.02, 6), &t));
    }

    #[test]
    fn ruin_and_below_bar_never_persist() {
        let gate = QualityGate::with_defaults();
        let t = QualityThreshold::at(0.10);
        assert!(!gate.persists(&fit(f64::NEG_INFINITY, 0.0, 9), &t)); // ruined
        assert!(!gate.persists(&fit(0.05, 0.0, 9), &t)); // below the bar
    }

    #[test]
    fn survivors_returns_persisted_in_order() {
        let gate = QualityGate::with_defaults();
        let t = QualityThreshold::at(0.10);
        let ids = [10usize, 20, 30, 40];
        let candidates = [
            (&ids[0], fit(0.20, 0.01, 6)), // persist
            (&ids[1], fit(0.20, 0.01, 1)), // exploration → no
            (&ids[2], fit(0.05, 0.0, 6)),  // below bar → no
            (&ids[3], fit(0.30, 0.02, 8)), // persist
        ];
        let survivors = gate.survivors(&candidates, &t);
        assert_eq!(survivors, vec![&ids[0], &ids[3]]);
    }

    // --- QE-446 optional drawdown ceiling at the graduation gate -----------------------------

    #[test]
    fn drawdown_ceiling_is_off_by_default() {
        // Default gate carries no ceiling: any drawdown passes, and persists_with_drawdown ==
        // persists across the board (ceiling OFF ⇒ graduation behaviour byte-identical).
        let gate = QualityGate::with_defaults();
        assert_eq!(gate.max_drawdown_ceiling, None);
        let t = QualityThreshold::at(0.10);
        for (mean, se, n, dd) in [
            (0.20, 0.02, 6, 0.05), // graduates, shallow dd
            (0.20, 0.02, 6, 0.99), // graduates, deep dd — still passes because ceiling is OFF
            (0.05, 0.0, 6, 0.01),  // below bar
            (0.20, 0.02, 1, 0.01), // exploration
        ] {
            let f = fit(mean, se, n);
            assert!(gate.drawdown_within_ceiling(dd));
            assert_eq!(
                gate.persists_with_drawdown(&f, dd, &t),
                gate.persists(&f, &t),
                "ceiling OFF must not change the persist decision"
            );
        }
    }

    #[test]
    fn drawdown_ceiling_blocks_high_growth_deep_drawdown_genome() {
        // A candidate that clears the log-growth bar comfortably but whose equity path drew down 45%.
        let gate = QualityGate::with_defaults().with_drawdown_ceiling(0.30);
        assert_eq!(gate.max_drawdown_ceiling, Some(0.30));
        let t = QualityThreshold::at(0.10);
        let high_growth = fit(0.20, 0.02, 6); // lower bound 0.18 ≥ 0.10 → clears the growth bar

        // Without the ceiling it would graduate …
        assert!(QualityGate::with_defaults().persists(&high_growth, &t));
        // … but a 45% drawdown exceeds the 30% ceiling → blocked.
        assert!(!gate.drawdown_within_ceiling(0.45));
        assert!(!gate.persists_with_drawdown(&high_growth, 0.45, &t));
        // A shallow-drawdown genome with the SAME growth still graduates under the ceiling.
        assert!(gate.drawdown_within_ceiling(0.20));
        assert!(gate.persists_with_drawdown(&high_growth, 0.20, &t));
        // Exactly at the ceiling passes (≤), just over does not.
        assert!(gate.persists_with_drawdown(&high_growth, 0.30, &t));
        assert!(!gate.persists_with_drawdown(&high_growth, 0.3000001, &t));
    }

    #[test]
    fn drawdown_ceiling_never_rescues_a_below_bar_candidate() {
        // The ceiling only *tightens* admission — a candidate that fails the growth bar is rejected
        // regardless of how shallow its drawdown is.
        let gate = QualityGate::with_defaults().with_drawdown_ceiling(0.50);
        let t = QualityThreshold::at(0.10);
        let below_bar = fit(0.05, 0.0, 6); // below the growth bar
        assert!(!gate.persists_with_drawdown(&below_bar, 0.0, &t));
        // Ceiling is clamped into [0, 1].
        assert_eq!(
            QualityGate::with_defaults()
                .with_drawdown_ceiling(1.5)
                .max_drawdown_ceiling,
            Some(1.0)
        );
        // A non-finite drawdown never passes a set ceiling.
        assert!(!gate.drawdown_within_ceiling(f64::NAN));
    }

    // --- QE-436 parsimony (MDL) tie-break at the gate ----------------------------------------

    #[test]
    fn most_parsimonious_picks_simplest_at_equal_robust_fitness() {
        let gate = QualityGate::with_defaults();
        // Three survivors with essentially equal robust fitness (all inside the 1σ band) but different
        // structural complexity. The simplest (complexity 2) must win.
        let ids = [10usize, 20, 30];
        let candidates = [
            (&ids[0], fit(0.10, 0.02, 6), 6),   // 4-clause-ish
            (&ids[1], fit(0.1005, 0.02, 6), 2), // 1-clause, simplest
            (&ids[2], fit(0.0997, 0.02, 6), 4),
        ];
        assert_eq!(gate.most_parsimonious(&candidates), Some(&ids[1]));
    }

    #[test]
    fn material_fitness_beats_parsimony_at_the_gate() {
        let gate = QualityGate::with_defaults();
        let ids = [1usize, 2];
        // A far simpler but materially worse candidate must NOT be chosen over a much stronger one.
        let candidates = [
            (&ids[0], fit(0.30, 0.02, 6), 10), // strong, complex
            (&ids[1], fit(0.05, 0.02, 6), 1),  // simplest, but far weaker
        ];
        assert_eq!(gate.most_parsimonious(&candidates), Some(&ids[0]));
        // graduation_cmp agrees: the strong complex one is the better pick.
        assert_eq!(
            gate.graduation_cmp((&fit(0.30, 0.02, 6), 10), (&fit(0.05, 0.02, 6), 1)),
            Ordering::Greater
        );
    }

    #[test]
    fn graduation_cmp_and_selection_are_deterministic_and_ruin_aware() {
        let gate = QualityGate::with_defaults();
        // Finite beats ruined regardless of complexity.
        assert_eq!(
            gate.graduation_cmp(
                (&fit(0.01, 0.0, 6), 9),
                (&fit(f64::NEG_INFINITY, 0.0, 6), 0)
            ),
            Ordering::Greater
        );
        // An exact tie on both axes keeps the earliest in input order (determinism).
        let ids = [7usize, 8];
        let candidates = [
            (&ids[0], fit(0.10, 0.02, 6), 3),
            (&ids[1], fit(0.10, 0.02, 6), 3),
        ];
        assert_eq!(gate.most_parsimonious(&candidates), Some(&ids[0]));
        // Empty input → None.
        let empty: [(&usize, NoiseRobustFitness, u32); 0] = [];
        assert_eq!(gate.most_parsimonious(&empty), None);
        // robust_lower_bound is the named mean − k·se.
        approx(
            gate.robust_lower_bound(&fit(0.20, 0.02, 6)),
            0.20 - DEFAULT_K_SIGMA * 0.02,
        );
        assert_eq!(
            gate.robust_lower_bound(&fit(f64::NEG_INFINITY, 0.0, 6)),
            f64::NEG_INFINITY
        );
    }
}
