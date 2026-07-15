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

// Order-emission path (QE-268): reject `unwrap`/`expect`/`panic` — a panic here is a live-trading fault.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rust_decimal::Decimal;

use qe_risk::{BreakerThresholds, BreakerTier, CalibrationProfile, CircuitBreaker, Fraction};
use qe_signal::Decision;

use crate::boot_state::ReconstructedState;
use crate::evaluator::ChromosomeDecision;

/// A threshold set to 1.0, so a tier only fires at a full 100% drawdown (total wipeout) — effectively
/// never. Used to disable the ensemble breaker's slow/med tiers (it is fast-drop only).
fn never_fires() -> Fraction {
    Fraction::ONE
}

/// A zero-threshold breaker for an uncalibrated strategy. Note this is *defence in depth* only:
/// [`BreakerLayer::from_calibration`] also **explicitly pre-gates** any uncalibrated strategy, so the
/// fail-safe does not rely on the breaker's tier thresholds (which could change) to gate it.
fn fires_immediately() -> BreakerThresholds {
    let zero = Fraction::ZERO;
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
    /// Permanent per-strategy fail-safe (uncalibrated strategies) — re-applied after every `reset`.
    pre_gated: Vec<bool>,
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
            pre_gated: vec![false; n],
            ensemble_gated: false,
        }
    }

    /// Build a layer from a per-vintage [`CalibrationProfile`], mapping strategy `i` to
    /// `profile.per_strategy[strategy_ids[i]]`. A strategy **missing** from the profile is **explicitly
    /// pre-gated** (fail-safe: an uncalibrated strategy is never traded) — the gate does not depend on the
    /// breaker's tier thresholds, so a future threshold-logic change cannot silently un-protect it, and it
    /// is **re-applied by [`reset`](Self::reset)** so a session rollover cannot briefly un-protect it. Its
    /// breaker is also given a fires-immediately threshold as defence in depth. The ensemble breaker uses
    /// `profile.ensemble_fast_drop`.
    #[must_use]
    pub fn from_calibration(
        profile: &CalibrationProfile,
        strategy_ids: &[String],
        fast_window: usize,
    ) -> Self {
        let mut per_strategy = Vec::with_capacity(strategy_ids.len());
        let mut uncalibrated = Vec::new();
        for (i, id) in strategy_ids.iter().enumerate() {
            match profile.per_strategy.get(id) {
                Some(t) => per_strategy.push(*t),
                None => {
                    per_strategy.push(fires_immediately());
                    uncalibrated.push(i);
                }
            }
        }
        let mut layer = Self::new(per_strategy, profile.ensemble_fast_drop, fast_window);
        // Explicit fail-safe: gate uncalibrated strategies from the start, independent of any breaker trip,
        // and record them in `pre_gated` so `reset` re-applies the gate on every rollover.
        for i in uncalibrated {
            layer.pre_gated[i] = true;
            layer.strategy_gated[i] = true;
        }
        layer
    }

    /// Seed the drawdown anchors from a reconstructed cold-start state (QE-401) — the wiring that makes the
    /// *true* all-time `committed_peak_equity` (computed at bootstrap, [`ReconstructedState::from_replay`])
    /// actually drive the live breaker. Each strategy breaker's peak is pre-loaded from
    /// `state.strategies[i].committed_peak_equity` (aligned by [`StrategyState::index`](crate::boot_state::StrategyState::index)),
    /// so the **first** live equity tick measures total drawdown against the historical peak instead of
    /// re-anchoring on it — a book already below its peak trips the slow/med tier immediately rather than
    /// staying silent. The ensemble breaker is seeded from the aggregate committed peak
    /// ([`ReconstructedState::aggregate_committed_peak`](crate::boot_state::ReconstructedState::aggregate_committed_peak)).
    ///
    /// A strategy whose reconstructed peak is `None` (empty equity path) is left un-seeded (re-anchors on its
    /// first tick, as before). Indices outside this layer are ignored. The seed is preserved across
    /// [`reset`](Self::reset). Only the slow/med anchor is seeded; the fast-drop window is inherently
    /// windowed (out of scope). Call this immediately after construction, before the first live tick.
    pub fn seed_committed_peaks(&mut self, state: &ReconstructedState) {
        for s in &state.strategies {
            if let (Some(peak), Some(breaker)) =
                (s.committed_peak_equity, self.strategy.get_mut(s.index))
            {
                breaker.seed_peak(peak);
            }
        }
        if let Some(aggregate) = state.aggregate_committed_peak() {
            self.ensemble.seed_peak(aggregate);
        }
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

    /// Strategy `index`'s current all-time drawdown anchor (its seeded/observed peak), for observability
    /// (QE-304 cockpit surfaces the seeded committed peak). `None` for an out-of-range index or an
    /// un-anchored breaker.
    #[must_use]
    pub fn strategy_peak(&self, index: usize) -> Option<Decimal> {
        self.strategy.get(index).and_then(CircuitBreaker::peak)
    }

    /// The ensemble breaker's current drawdown anchor (its seeded/observed aggregate peak), for observability.
    #[must_use]
    pub fn ensemble_peak(&self) -> Option<Decimal> {
        self.ensemble.peak()
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

    /// Clear all *tripped* gating and re-arm every breaker (new vintage / session rollover). The permanent
    /// uncalibrated fail-safe is **re-applied**, so a reset never un-protects an uncalibrated strategy.
    pub fn reset(&mut self) {
        for b in &mut self.strategy {
            b.reset();
        }
        self.ensemble.reset();
        for (g, &pre) in self.strategy_gated.iter_mut().zip(&self.pre_gated) {
            *g = pre;
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

        // s1 (uncalibrated) is gated from the start — an EXPLICIT pre-gate, before any observe, so the
        // fail-safe does not depend on the breaker's tier thresholds firing.
        assert!(layer.is_gated(1), "uncalibrated strategy is pre-gated");
        assert!(!layer.is_gated(0), "calibrated strategy is not gated");
        // clamp flattens the uncalibrated strategy immediately.
        let clamped = layer.clamp(&[enter_long(0), enter_long(1)]);
        assert_eq!(clamped[1].decision, Decision::Exit);
        assert_eq!(clamped[0].decision, Decision::Enter(Direction::Long));

        // s0 (calibrated) does not trip on a flat first tick.
        assert!(layer.observe_strategy(0, dec(100)).is_none());
        assert!(!layer.is_gated(0));
    }

    /// QE-401: seeding pre-loads each per-strategy drawdown anchor from the reconstructed committed peak, so
    /// the **first** live tick of a book already below its peak trips (and gates) instead of re-anchoring.
    #[test]
    fn seed_committed_peaks_anchors_first_tick_and_gates() {
        use crate::boot_state::{DormancyLatch, ReconstructedState, StrategyState};
        use qe_signal::PositionState;

        let mut layer = BreakerLayer::new(
            vec![thresholds(), thresholds()],
            frac("0.10"),
            DEFAULT_FAST_WINDOW,
        );
        let state = ReconstructedState {
            strategies: vec![
                StrategyState {
                    index: 0,
                    position: PositionState::flat(),
                    dormancy: DormancyLatch::active(),
                    committed_peak_equity: Some(dec(100)),
                },
                StrategyState {
                    index: 1,
                    position: PositionState::flat(),
                    dormancy: DormancyLatch::active(),
                    committed_peak_equity: None, // empty path → left un-seeded
                },
            ],
        };
        layer.seed_committed_peaks(&state);

        // Strategy 0 is anchored at 100: a first tick at 85 is 15% drawdown ≥ med (0.12) → gated immediately.
        assert_eq!(layer.observe_strategy(0, dec(85)), Some(BreakerTier::Med));
        assert!(
            layer.is_gated(0),
            "seeded strategy gates on the true drawdown"
        );
        // Strategy 1 has no seed: the same first tick re-anchors and reports ~0 drawdown → not gated.
        assert!(layer.observe_strategy(1, dec(85)).is_none());
        assert!(!layer.is_gated(1));
    }

    /// QE-401: the seed survives `reset` (a session rollover does not silently un-anchor the breaker).
    #[test]
    fn seed_committed_peaks_survive_reset() {
        use crate::boot_state::{DormancyLatch, ReconstructedState, StrategyState};
        use qe_signal::PositionState;

        let mut layer = BreakerLayer::new(vec![thresholds()], frac("0.10"), DEFAULT_FAST_WINDOW);
        let state = ReconstructedState {
            strategies: vec![StrategyState {
                index: 0,
                position: PositionState::flat(),
                dormancy: DormancyLatch::active(),
                committed_peak_equity: Some(dec(100)),
            }],
        };
        layer.seed_committed_peaks(&state);
        layer.observe_strategy(0, dec(98)); // 2% drawdown, no fire
        layer.reset();
        // After the rollover the anchor is still 100, so a 15% drop trips Med on the first post-reset tick.
        assert_eq!(layer.observe_strategy(0, dec(85)), Some(BreakerTier::Med));
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

    /// `reset` re-applies the uncalibrated fail-safe: an uncalibrated strategy stays gated across a reset,
    /// while a tripped-but-calibrated strategy is un-gated.
    #[test]
    fn reset_reapplies_uncalibrated_fail_safe() {
        let mut profile = CalibrationProfile::new(frac("0.10"));
        profile.per_strategy.insert("s0".to_owned(), thresholds());
        // "s1" is uncalibrated → permanently pre-gated.
        let ids = vec!["s0".to_owned(), "s1".to_owned()];
        let mut layer = BreakerLayer::from_calibration(&profile, &ids, DEFAULT_FAST_WINDOW);

        // Trip the calibrated strategy too.
        layer.observe_strategy(0, dec(100));
        layer.observe_strategy(0, dec(50));
        assert!(layer.is_gated(0) && layer.is_gated(1));

        layer.reset();
        assert!(
            !layer.is_gated(0),
            "a calibrated strategy's trip clears on reset"
        );
        assert!(
            layer.is_gated(1),
            "an uncalibrated strategy stays gated across a reset (fail-safe re-applied)"
        );
    }
}
