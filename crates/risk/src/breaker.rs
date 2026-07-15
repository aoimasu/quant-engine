//! Circuit-breaker model + smoothed-mark observer (QE-116).
//!
//! A three-tier breaker watches an **equity stream** and fires the most severe triggered tier: a
//! **fast** tier on a *rapid* drop (speed), a **med** tier on a moderate drawdown, and a **slow** tier
//! on a gentle grind-down. It is a pure function of the stream, so the *same* code runs inside the WFO
//! harness on history (calibration replay) and live (QE-212).
//!
//! The equity stream is driven by a **smoothed mark** ([`MarkEma`], EMA τ½=60s per spec) — smoothing
//! rejects 1-tick noise so the slow/med tiers don't trip on jitter. A documented alternative (QE-116/D1,
//! A3) feeds the **raw** mark to the fast tier so a gap is not averaged away; baseline uses the smoothed
//! stream. No float money — `Decimal` throughout (the EMA `alpha` is a smoothing coefficient, not a
//! price).

use std::collections::VecDeque;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::limit::Fraction;

/// Default fast-drop measurement window, in ticks.
pub const DEFAULT_FAST_WINDOW: usize = 5;

/// Exponential moving average over the mark price — the smoothed-mark tick observer (QE-116/D1).
#[derive(Debug, Clone)]
pub struct MarkEma {
    alpha: Decimal,
    value: Option<Decimal>,
}

impl MarkEma {
    /// Build an EMA with per-tick smoothing `alpha = 1 − 2^(−tick/half_life)` (so τ½ = 60s at 1s ticks
    /// per spec). A non-positive half-life ⇒ `alpha = 1` (no smoothing).
    #[must_use]
    pub fn with_half_life(half_life_secs: f64, tick_secs: f64) -> Self {
        let a = if half_life_secs > 0.0 {
            1.0 - 0.5_f64.powf(tick_secs / half_life_secs)
        } else {
            1.0
        };
        let alpha = Decimal::from_f64_retain(a)
            .unwrap_or(Decimal::ONE)
            .clamp(Decimal::ZERO, Decimal::ONE);
        MarkEma { alpha, value: None }
    }

    /// Build an EMA with an explicit smoothing coefficient (clamped to `[0,1]`).
    #[must_use]
    pub fn with_alpha(alpha: Decimal) -> Self {
        MarkEma {
            alpha: alpha.clamp(Decimal::ZERO, Decimal::ONE),
            value: None,
        }
    }

    /// Push a mark `price`, returning the updated smoothed value. The first sample seeds the EMA.
    pub fn update(&mut self, price: Decimal) -> Decimal {
        let v = match self.value {
            None => price,
            Some(prev) => prev + self.alpha * (price - prev),
        };
        self.value = Some(v);
        v
    }

    /// The current smoothed value, if any sample has been seen.
    #[must_use]
    pub fn value(&self) -> Option<Decimal> {
        self.value
    }
}

/// Which breaker tier fired, in increasing urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerTier {
    /// Gentle grind-down: total drawdown ≥ `slow_dd`.
    Slow,
    /// Moderate drawdown ≥ `med_dd`.
    Med,
    /// Rapid drop ≥ `fast_drop` within the fast window (speed, not depth) — most urgent.
    Fast,
}

/// The drawdown thresholds for the three tiers (`slow_dd < med_dd`; `fast_drop` is over the fast window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerThresholds {
    /// Slow-tier total-drawdown threshold.
    pub slow_dd: Fraction,
    /// Med-tier total-drawdown threshold.
    pub med_dd: Fraction,
    /// Fast-tier drop-over-window threshold.
    pub fast_drop: Fraction,
}

/// A three-tier circuit breaker walking an equity stream (QE-116/D2).
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    thresholds: BreakerThresholds,
    fast_window: usize,
    peak: Option<Decimal>,
    /// The pre-loaded all-time committed-peak anchor (QE-401), preserved across [`reset`](Self::reset). A
    /// breaker built without a seed keeps this `None` and re-anchors from the first observed tick (legacy).
    seed_peak: Option<Decimal>,
    recent: VecDeque<Decimal>,
}

impl CircuitBreaker {
    /// Build a breaker. `fast_window` is clamped to ≥ 1.
    #[must_use]
    pub fn new(thresholds: BreakerThresholds, fast_window: usize) -> Self {
        CircuitBreaker {
            thresholds,
            fast_window: fast_window.max(1),
            peak: None,
            seed_peak: None,
            recent: VecDeque::new(),
        }
    }

    /// Pre-load the all-time committed-peak equity anchor (QE-401 — builder form), so the **first** observed
    /// tick already measures total drawdown against the true historical peak instead of re-anchoring on it.
    /// Only the slow/med drawdown anchor is seeded; the fast-drop window is intentionally left empty (the
    /// speed tier is inherently windowed). The seed is preserved across [`reset`](Self::reset) unless a
    /// genuinely higher peak is later observed.
    #[must_use]
    pub fn with_seed_peak(mut self, peak: Decimal) -> Self {
        self.seed_peak(peak);
        self
    }

    /// Pre-load the all-time committed-peak equity anchor in place (QE-401). See [`with_seed_peak`](Self::with_seed_peak).
    pub fn seed_peak(&mut self, peak: Decimal) {
        self.seed_peak = Some(peak);
        self.peak = Some(peak);
    }

    /// The all-time equity peak seen so far (including any seeded committed peak).
    #[must_use]
    pub fn peak(&self) -> Option<Decimal> {
        self.peak
    }

    /// Re-arm for a new vintage / session rollover. The fast-drop window is cleared. A **seeded** breaker
    /// preserves its committed-peak anchor (QE-401) — carried at the higher of the seed and the highest
    /// observed peak, so a genuinely higher live peak survives — instead of re-anchoring to `None`. An
    /// un-seeded breaker keeps the legacy behaviour (`peak = None`).
    pub fn reset(&mut self) {
        self.recent.clear();
        if self.seed_peak.is_some() {
            // `self.peak` is already `max(seed, highest observed)` (see `observe`), so carrying it forward
            // preserves the seed unless a genuinely higher peak was observed. Persist it as the new anchor
            // floor so subsequent rollovers keep it too (monotone non-decreasing).
            self.seed_peak = self.peak;
        } else {
            self.peak = None;
        }
    }

    /// Observe one equity tick and return the most severe tier triggered, if any. Fast (speed) beats
    /// Med beats Slow (depth).
    pub fn observe(&mut self, equity: Decimal) -> Option<BreakerTier> {
        // Rolling window for the fast-drop measure (length fast_window+1 to span fast_window ticks).
        self.recent.push_back(equity);
        while self.recent.len() > self.fast_window + 1 {
            self.recent.pop_front();
        }
        let window_max = self.recent.iter().copied().max().unwrap_or(equity);
        let fast_drop = if window_max > Decimal::ZERO {
            (window_max - equity) / window_max
        } else {
            Decimal::ZERO
        };

        // All-time peak drives total drawdown.
        let peak = match self.peak {
            Some(p) => p.max(equity),
            None => equity,
        };
        self.peak = Some(peak);
        let drawdown = if peak > Decimal::ZERO {
            (peak - equity) / peak
        } else {
            Decimal::ZERO
        };

        let fast_thresh = self.thresholds.fast_drop.get();
        if fast_thresh > Decimal::ZERO && fast_drop >= fast_thresh {
            return Some(BreakerTier::Fast);
        }
        if drawdown >= self.thresholds.med_dd.get() {
            return Some(BreakerTier::Med);
        }
        if drawdown >= self.thresholds.slow_dd.get() {
            return Some(BreakerTier::Slow);
        }
        None
    }
}

/// Replay a breaker over a historical `equity` series (QE-116 — runnable in the WFO harness), returning
/// `(tick_index, tier)` for every tick that fired.
#[must_use]
pub fn replay(
    thresholds: BreakerThresholds,
    fast_window: usize,
    equity: &[Decimal],
) -> Vec<(usize, BreakerTier)> {
    let mut breaker = CircuitBreaker::new(thresholds, fast_window);
    let mut events = Vec::new();
    for (i, &e) in equity.iter().enumerate() {
        if let Some(tier) = breaker.observe(e) {
            events.push((i, tier));
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn frac(s: &str) -> Fraction {
        Fraction::new(d(s)).unwrap()
    }

    fn thresholds() -> BreakerThresholds {
        BreakerThresholds {
            slow_dd: frac("0.05"),
            med_dd: frac("0.12"),
            fast_drop: frac("0.08"),
        }
    }

    #[test]
    fn ema_half_life_reaches_halfway_after_one_half_life() {
        let mut ema = MarkEma::with_half_life(60.0, 1.0);
        ema.update(d("100")); // seed
        let mut v = d("100");
        for _ in 0..60 {
            v = ema.update(d("200"));
        }
        // After one half-life of a step input the smoothed value is ~halfway (150).
        assert!(
            (v - d("150")).abs() < d("2"),
            "EMA after one half-life = {v}"
        );
    }

    #[test]
    fn ema_rejects_a_one_tick_spike() {
        let mut ema = MarkEma::with_half_life(60.0, 1.0);
        for _ in 0..10 {
            ema.update(d("100"));
        }
        let after = ema.update(d("1000")); // a single huge spike
        assert!(
            after < d("200"),
            "smoothed value should barely move: {after}"
        );
    }

    #[test]
    fn historical_replay_fires_slow_med_and_fast_across_regimes() {
        // Calm (no fire) → slow grind-down (Slow then Med) → recover to new high → sharp crash (Fast).
        let equity: Vec<Decimal> = [
            100, 100, 100, // calm
            98, 96, 94, 92, 90, 88, 87, // grind: dd crosses slow (0.05) then med (0.12)
            95, 100, 105, // recover to a new peak
            96,  // single-tick crash from 105 → fast-drop ≈ 8.6%
        ]
        .iter()
        .map(|n| Decimal::from(*n))
        .collect();

        let events = replay(thresholds(), 3, &equity);
        let tiers: Vec<BreakerTier> = events.iter().map(|(_, t)| *t).collect();
        assert!(
            tiers.contains(&BreakerTier::Slow),
            "no Slow fired: {events:?}"
        );
        assert!(
            tiers.contains(&BreakerTier::Med),
            "no Med fired: {events:?}"
        );
        assert!(
            tiers.contains(&BreakerTier::Fast),
            "no Fast fired: {events:?}"
        );
        // The crash tick (index 13) fires Fast specifically.
        assert!(events.contains(&(13, BreakerTier::Fast)));
    }

    /// QE-401: a seeded breaker measures drawdown against the true committed peak on the *first* tick — a
    /// book already 15% below its historical peak reports ≈15% drawdown (not ≈0) and trips the med tier.
    #[test]
    fn seed_peak_anchors_drawdown_on_first_tick() {
        let mut cb = CircuitBreaker::new(thresholds(), 3).with_seed_peak(d("100"));
        assert_eq!(cb.peak(), Some(d("100")), "seed pre-loads the anchor");
        // First live tick 15% below the seed: drawdown = (100 − 85)/100 = 0.15 ≥ med_dd (0.12) → Med.
        assert_eq!(cb.observe(d("85")), Some(BreakerTier::Med));
        // Without the seed the same first tick re-anchors on 85 and reports ~0 drawdown (silent).
        let mut unseeded = CircuitBreaker::new(thresholds(), 3);
        assert_eq!(unseeded.observe(d("85")), None);
    }

    /// QE-401: a seed at the med threshold exactly trips Med (boundary), and a shallower drop stays Slow.
    #[test]
    fn seed_peak_trips_med_at_threshold() {
        // med_dd = 0.12, slow_dd = 0.05. Seed 200; a drop to 176 is exactly 12% → Med.
        let mut cb = CircuitBreaker::new(thresholds(), 3).with_seed_peak(d("200"));
        assert_eq!(cb.observe(d("176")), Some(BreakerTier::Med));
        // A 6% drop (→188) from a fresh seed is Slow, not Med.
        let mut cb2 = CircuitBreaker::new(thresholds(), 3).with_seed_peak(d("200"));
        assert_eq!(cb2.observe(d("188")), Some(BreakerTier::Slow));
    }

    /// QE-401: `reset` preserves the seed (the anchor survives a rollover), unlike an un-seeded breaker.
    #[test]
    fn reset_preserves_seed_peak() {
        let mut cb = CircuitBreaker::new(thresholds(), 3).with_seed_peak(d("100"));
        cb.observe(d("98")); // 2% drawdown, no fire
        cb.reset();
        assert_eq!(cb.peak(), Some(d("100")), "seed survives reset");
        // After reset the anchor is still 100, so a 15% drop trips Med on the first post-reset tick.
        assert_eq!(cb.observe(d("85")), Some(BreakerTier::Med));
    }

    /// QE-401: a genuinely higher observed peak survives reset (the anchor is monotone non-decreasing).
    #[test]
    fn reset_keeps_a_higher_observed_peak() {
        let mut cb = CircuitBreaker::new(thresholds(), 3).with_seed_peak(d("100"));
        cb.observe(d("120")); // climbs to a new all-time high above the seed
        assert_eq!(cb.peak(), Some(d("120")));
        cb.reset();
        assert_eq!(
            cb.peak(),
            Some(d("120")),
            "the higher observed peak, not the seed, is carried across the rollover"
        );
        // Drawdown is now measured from 120: a drop to 102 is 15% → Med.
        assert_eq!(cb.observe(d("102")), Some(BreakerTier::Med));
    }

    #[test]
    fn peak_tracking_and_reset() {
        let mut cb = CircuitBreaker::new(thresholds(), 3);
        assert_eq!(cb.observe(d("100")), None);
        assert_eq!(cb.observe(d("110")), None); // new peak
        assert_eq!(cb.peak(), Some(d("110")));
        // From peak 110, a slow drawdown of 6% (→ 103.4) trips Slow, not Med.
        assert_eq!(cb.observe(d("103")), Some(BreakerTier::Slow));
        cb.reset();
        assert_eq!(cb.peak(), None);
        assert_eq!(cb.observe(d("103")), None); // fresh peak, no drawdown
    }
}
