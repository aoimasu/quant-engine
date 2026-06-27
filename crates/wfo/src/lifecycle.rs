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

use crate::fitness::{NoiseRobustFitness, DEFAULT_K_SIGMA};

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
}

impl QualityGate {
    /// Build a gate explicitly.
    #[must_use]
    pub fn new(policy: ThresholdPolicy, min_exploitation_windows: usize, k_sigma: f64) -> Self {
        QualityGate {
            policy,
            min_exploitation_windows,
            k_sigma,
        }
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
}
