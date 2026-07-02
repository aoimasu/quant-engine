//! Circuit-breaker layer (QE-212) — the live gate that clamps gated strategies to flat before netting.
//!
//! Wraps the QE-116 [`CircuitBreaker`] primitive into a live layer: one breaker per strategy (aligned to
//! the vintage's chromosomes) plus one **ensemble** breaker, each fed a per-scope **equity stream** and
//! calibrated from the per-vintage [`CalibrationProfile`]. When a scope breaches it is **latched** gated
//! (a flattened strategy does not un-flatten on a noisy recovery), and [`BreakerLayer::clamp`] rewrites any
//! gated strategy's decision to [`Decision::Exit`] — flat — **before** netting (QE-213), so its aggregate
//! contribution is zero.
//!
//! Because each scope delegates to `CircuitBreaker::observe`, the layer fires **identically to the QE-116
//! historical replay** ([`qe_risk::replay`]) on the same equity stream — the AC's parity requirement.
//!
//! The equity stream is a declared input, built from the QE-208 smoothed mark × positions net-of-cost; that
//! live feed is QE-217 (the same equity boundary QE-210 established). Per-direction / per-cohort gating uses
//! the identical breaker + latched-gate mechanism keyed by those scopes, deferred until their aggregate
//! equity streams and the strategy→scope map exist (QE-213 / vintage metadata).

use rust_decimal::Decimal;

use qe_risk::{BreakerThresholds, BreakerTier, CalibrationProfile, CircuitBreaker, Fraction};
use qe_signal::Decision;

use crate::evaluator::ChromosomeDecision;

/// A threshold that never fires (100% drawdown) — disables a tier (used for the ensemble's slow/med tiers).
fn never_fires() -> Fraction {
    Fraction::new(Decimal::ONE).expect("1.0 is a valid fraction")
}

/// A zero threshold that fires immediately — the fail-safe for an uncalibrated strategy (never trade one).
fn fires_immediately() -> BreakerThresholds {
    let zero = Fraction::new(Decimal::ZERO).expect("0.0 is a valid fraction");
    BreakerThresholds {
        slow_dd: zero,
        med_dd: zero,
        fast_drop: zero,
    }
}

/// The live circuit-breaker layer: per-strategy + ensemble breakers with latched gating.
pub struct BreakerLayer {
    /// One breaker per strategy (aligned to the vintage's chromosomes).
    strategy: Vec<CircuitBreaker>,
    /// The ensemble fast-drop breaker (gates every strategy when it trips).
    ensemble: CircuitBreaker,
    /// Latched per-strategy gating.
    strategy_gated: Vec<bool>,
    /// Latched ensemble gating.
    ensemble_gated: bool,
}

impl BreakerLayer {
    /// A layer over `per_strategy` thresholds (one per chromosome) plus an ensemble **fast-drop** breaker
    /// (`ensemble_fast_drop`; its slow/med tiers are disabled). `fast_window` is the fast-drop window.
    #[must_use]
    pub fn new(
        per_strategy: Vec<BreakerThresholds>,
        ensemble_fast_drop: Fraction,
        fast_window: usize,
    ) -> Self {
        let n = per_strategy.len();
        let strategy = per_strategy
            .into_iter()
            .map(|t| CircuitBreaker::new(t, fast_window))
            .collect();
        let ensemble = CircuitBreaker::new(
            BreakerThresholds {
                slow_dd: never_fires(),
                med_dd: never_fires(),
                fast_drop: ensemble_fast_drop,
            },
            fast_window,
        );
        Self {
            strategy,
            ensemble,
            strategy_gated: vec![false; n],
            ensemble_gated: false,
        }
    }

    /// Build a layer from a per-vintage [`CalibrationProfile`], mapping strategy `i` to
    /// `profile.per_strategy[strategy_ids[i]]`. A strategy **missing** from the profile gets a
    /// fires-immediately breaker (fail-safe: an uncalibrated strategy is gated, not silently un-protected).
    /// The ensemble breaker uses `profile.ensemble_fast_drop`.
    #[must_use]
    pub fn from_calibration(
        profile: &CalibrationProfile,
        strategy_ids: &[String],
        fast_window: usize,
    ) -> Self {
        let per_strategy = strategy_ids
            .iter()
            .map(|id| {
                profile
                    .per_strategy
                    .get(id)
                    .copied()
                    .unwrap_or_else(fires_immediately)
            })
            .collect();
        Self::new(per_strategy, profile.ensemble_fast_drop, fast_window)
    }

    /// Observe one equity tick for strategy `index`, latching it gated if any tier trips. Returns the tier
    /// that fired (for observability), or `None`. Out-of-range indices return `None`.
    pub fn observe_strategy(&mut self, index: usize, equity: Decimal) -> Option<BreakerTier> {
        let tier = self.strategy.get_mut(index)?.observe(equity);
        if tier.is_some() {
            self.strategy_gated[index] = true;
        }
        tier
    }

    /// Observe one equity tick for the ensemble, latching **all** strategies gated if it trips.
    pub fn observe_ensemble(&mut self, equity: Decimal) -> Option<BreakerTier> {
        let tier = self.ensemble.observe(equity);
        if tier.is_some() {
            self.ensemble_gated = true;
        }
        tier
    }

    /// Whether strategy `index` is gated — directly, or because the ensemble is gated.
    #[must_use]
    pub fn is_gated(&self, index: usize) -> bool {
        self.ensemble_gated || self.strategy_gated.get(index).copied().unwrap_or(false)
    }

    /// Whether the ensemble breaker has tripped (gating every strategy).
    #[must_use]
    pub fn ensemble_gated(&self) -> bool {
        self.ensemble_gated
    }

    /// The number of strategies the layer covers.
    #[must_use]
    pub fn strategy_count(&self) -> usize {
        self.strategy.len()
    }

    /// Clamp gated strategies to flat **before netting**: any decision whose strategy is gated becomes
    /// [`Decision::Exit`] (drives the position flat and keeps it flat); ungated decisions pass through.
    #[must_use]
    pub fn clamp(&self, decisions: &[ChromosomeDecision]) -> Vec<ChromosomeDecision> {
        decisions
            .iter()
            .map(|cd| {
                if self.is_gated(cd.index) {
                    ChromosomeDecision {
                        index: cd.index,
                        decision: Decision::Exit,
                    }
                } else {
                    *cd
                }
            })
            .collect()
    }

    /// Clear all gating and re-arm every breaker (new vintage / session rollover).
    pub fn reset(&mut self) {
        for b in &mut self.strategy {
            b.reset();
        }
        self.ensemble.reset();
        for g in &mut self.strategy_gated {
            *g = false;
        }
        self.ensemble_gated = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::Direction;
    use qe_risk::DEFAULT_FAST_WINDOW;
    use std::str::FromStr;

    fn dec(v: i64) -> Decimal {
        Decimal::from(v)
    }
    fn frac(s: &str) -> Fraction {
        Fraction::new(Decimal::from_str(s).unwrap()).unwrap()
    }
    fn thresholds() -> BreakerThresholds {
        BreakerThresholds {
            slow_dd: frac("0.05"),
            med_dd: frac("0.12"),
            fast_drop: frac("0.08"),
        }
    }
    fn enter_long(index: usize) -> ChromosomeDecision {
        ChromosomeDecision {
            index,
            decision: Decision::Enter(Direction::Long),
        }
    }

    /// AC (parity half): the live layer fires exactly the tiers QE-116's `replay` produces on the same
    /// equity stream.
    #[test]
    fn live_layer_matches_qe116_replay() {
        let th = thresholds();
        // Rise to a peak, then a drawdown deep and fast enough to trip tiers.
        let series: Vec<Decimal> = [100, 110, 108, 104, 98, 92, 88, 95, 90]
            .iter()
            .map(|&v| dec(v))
            .collect();

        let expected = qe_risk::replay(th, DEFAULT_FAST_WINDOW, &series);
        assert!(
            !expected.is_empty(),
            "the fixture must actually trip the breaker"
        );

        let mut layer = BreakerLayer::new(vec![th], never_fires(), DEFAULT_FAST_WINDOW);
        let mut events = Vec::new();
        for (i, &e) in series.iter().enumerate() {
            if let Some(tier) = layer.observe_strategy(0, e) {
                events.push((i, tier));
            }
        }
        assert_eq!(
            events, expected,
            "live layer must match the QE-116 replay tier-for-tier"
        );
    }

    /// AC (clamp half): a strategy breach clamps *that* strategy to flat; others pass through.
    #[test]
    fn strategy_breach_clamps_that_strategy_to_flat() {
        let mut layer = BreakerLayer::new(
            vec![thresholds(), thresholds()],
            never_fires(),
            DEFAULT_FAST_WINDOW,
        );
        layer.observe_strategy(0, dec(100));
        assert!(
            layer.observe_strategy(0, dec(50)).is_some(),
            "50% drawdown trips strategy 0"
        );
        assert!(layer.is_gated(0) && !layer.is_gated(1));

        let clamped = layer.clamp(&[enter_long(0), enter_long(1)]);
        assert_eq!(
            clamped[0].decision,
            Decision::Exit,
            "gated strategy is clamped to flat"
        );
        assert_eq!(
            clamped[1].decision,
            Decision::Enter(Direction::Long),
            "ungated strategy passes through"
        );
    }

    /// An ensemble breach gates and clamps every strategy.
    #[test]
    fn ensemble_breach_clamps_all_strategies() {
        let mut layer = BreakerLayer::new(
            vec![thresholds(), thresholds()],
            frac("0.10"),
            DEFAULT_FAST_WINDOW,
        );
        layer.observe_ensemble(dec(100));
        assert!(
            layer.observe_ensemble(dec(80)).is_some(),
            "20% fast drop trips the ensemble"
        );
        assert!(layer.ensemble_gated() && layer.is_gated(0) && layer.is_gated(1));

        let clamped = layer.clamp(&[
            ChromosomeDecision {
                index: 0,
                decision: Decision::Hold,
            },
            enter_long(1),
        ]);
        assert!(clamped.iter().all(|c| c.decision == Decision::Exit));
    }

    /// Gating latches: once tripped, a strategy stays gated even after equity recovers.
    #[test]
    fn gating_is_latched() {
        let mut layer = BreakerLayer::new(vec![thresholds()], never_fires(), DEFAULT_FAST_WINDOW);
        layer.observe_strategy(0, dec(100));
        layer.observe_strategy(0, dec(50)); // trips
        assert!(layer.is_gated(0));
        // Recovery: the breaker won't fire (drawdown from the new peak is small), but gating stays latched.
        layer.observe_strategy(0, dec(100));
        layer.observe_strategy(0, dec(101));
        assert!(layer.is_gated(0), "gating latches through a recovery");
    }

    /// `from_calibration` wires per-strategy thresholds + ensemble, and fails safe on a missing strategy.
    #[test]
    fn from_calibration_wires_profile_and_fails_safe_on_missing() {
        let mut profile = CalibrationProfile::new(frac("0.07"));
        profile.per_strategy.insert("s0".to_owned(), thresholds());
        // "s1" is intentionally absent from the profile.
        let ids = vec!["s0".to_owned(), "s1".to_owned()];
        let mut layer = BreakerLayer::from_calibration(&profile, &ids, DEFAULT_FAST_WINDOW);
        assert_eq!(layer.strategy_count(), 2);

        // s1 (uncalibrated) fires on its first tick — fail-safe gate.
        assert!(layer.observe_strategy(1, dec(100)).is_some());
        assert!(layer.is_gated(1));
        // s0 (calibrated) does not fire on a flat first tick.
        assert!(layer.observe_strategy(0, dec(100)).is_none());
        assert!(!layer.is_gated(0));
    }

    /// `reset` clears gating and re-arms the breakers.
    #[test]
    fn reset_clears_gating() {
        let mut layer = BreakerLayer::new(vec![thresholds()], frac("0.10"), DEFAULT_FAST_WINDOW);
        layer.observe_strategy(0, dec(100));
        layer.observe_strategy(0, dec(50));
        layer.observe_ensemble(dec(100));
        layer.observe_ensemble(dec(80));
        assert!(layer.is_gated(0) && layer.ensemble_gated());

        layer.reset();
        assert!(!layer.is_gated(0) && !layer.ensemble_gated());
        // Re-armed: a flat first tick does not fire.
        assert!(layer.observe_strategy(0, dec(100)).is_none());
    }
}
