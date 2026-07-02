//! Mark EMA loop + tick observer (QE-208).
//!
//! Slow-DD probing rides a **smoothed mark** (EMA τ½=60s per spec). This module drives markPrice@1s samples
//! through the QE-116 [`MarkEma`] and emits a [`MarkTick`] carrying **both** the raw sample and the smoothed
//! value, forwarded to a [`MarkTickObserver`] — the seam the breaker layer (QE-212) plugs into. The smoothed
//! stream is the spec baseline for the slow/med-DD probe; the raw mark is exposed on every tick so a
//! fast/raw tier (QE-116/D1's documented A3 alternative) can watch un-averaged price without a second
//! pipeline (the AC: both smoothed and raw available to breakers).
//!
//! Like the rest of the live pipeline this operates on **already-decoded** marks `(event_time_ms, price)`;
//! the markPrice@1s JSON decode + wss drive is runtime plumbing (mirrors QE-205 operating on decoded bars).

use rust_decimal::Decimal;

use qe_risk::MarkEma;

/// One mark observation: the raw markPrice@1s sample and its EMA-smoothed value at the same tick.
///
/// The `smoothed` value drives the slow/med-DD probe (spec baseline); `raw` is carried so the fast tier /
/// the QE-116 A3 raw-mark alternative can see un-averaged price. Both are available to breakers per the AC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkTick {
    /// Venue event time of the mark sample (epoch ms).
    pub event_time_ms: i64,
    /// The raw mark price for this tick.
    pub raw: Decimal,
    /// The EMA-smoothed mark for this tick (== `raw` on the seeding first tick).
    pub smoothed: Decimal,
}

/// Receives the mark-tick stream. The breaker layer (QE-212) implements this to consume ticks; a blanket
/// impl for `FnMut(&MarkTick)` lets callers pass a closure.
pub trait MarkTickObserver {
    /// Handle one produced tick.
    fn on_tick(&mut self, tick: &MarkTick);
}

impl<F: FnMut(&MarkTick)> MarkTickObserver for F {
    fn on_tick(&mut self, tick: &MarkTick) {
        self(tick);
    }
}

/// The EMA loop over markPrice@1s: smooths each raw sample and produces [`MarkTick`]s (raw + smoothed).
pub struct MarkEmaLoop {
    ema: MarkEma,
}

impl MarkEmaLoop {
    /// A loop with an EMA of half-life `half_life_secs` at a `tick_secs` sample spacing.
    #[must_use]
    pub fn with_half_life(half_life_secs: f64, tick_secs: f64) -> Self {
        Self {
            ema: MarkEma::with_half_life(half_life_secs, tick_secs),
        }
    }

    /// The spec baseline: EMA τ½=60s on 1-second markPrice ticks.
    #[must_use]
    pub fn spec_baseline() -> Self {
        Self::with_half_life(60.0, 1.0)
    }

    /// Observe one raw mark sample: update the EMA and return the tick (raw + smoothed). The first sample
    /// seeds the EMA, so its `smoothed == raw`.
    pub fn observe(&mut self, event_time_ms: i64, raw: Decimal) -> MarkTick {
        let smoothed = self.ema.update(raw);
        MarkTick {
            event_time_ms,
            raw,
            smoothed,
        }
    }

    /// The current smoothed mark, if any sample has been observed.
    #[must_use]
    pub fn smoothed(&self) -> Option<Decimal> {
        self.ema.value()
    }

    /// Drive an ordered sequence of `(event_time_ms, raw)` marks, forwarding each produced [`MarkTick`] to
    /// `observer` (the breaker feed) and returning the ticks in arrival order.
    pub fn drive<I, O>(&mut self, marks: I, observer: &mut O) -> Vec<MarkTick>
    where
        I: IntoIterator<Item = (i64, Decimal)>,
        O: MarkTickObserver,
    {
        marks
            .into_iter()
            .map(|(event_time_ms, raw)| {
                let tick = self.observe(event_time_ms, raw);
                observer.on_tick(&tick);
                tick
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(v: i64) -> Decimal {
        Decimal::from(v)
    }

    /// A collecting observer standing in for the breaker layer.
    #[derive(Default)]
    struct Collector {
        ticks: Vec<MarkTick>,
    }

    impl MarkTickObserver for Collector {
        fn on_tick(&mut self, tick: &MarkTick) {
            self.ticks.push(*tick);
        }
    }

    /// AC part 1: the EMA half-life is correct. Seeded at 0, a step to 100 held for τ½=60 ticks moves the
    /// smoothed mark ~halfway (to ≈50).
    #[test]
    fn ema_half_life_is_correct() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        // Seed at 0.
        let seed = loop_.observe(0, dec(0));
        assert_eq!(seed.smoothed, dec(0));
        // Feed the step (100) for 60 one-second ticks.
        let mut last = seed;
        for t in 1..=60 {
            last = loop_.observe(t * 1_000, dec(100));
        }
        // After one half-life the smoothed value is ~halfway from 0 to 100.
        let smoothed: f64 = last.smoothed.try_into().unwrap();
        assert!(
            (smoothed - 50.0).abs() < 1.0,
            "after one half-life the smoothed mark should be ~50, got {smoothed}"
        );
        assert_eq!(last.raw, dec(100), "raw is the un-smoothed input");
    }

    /// The first tick seeds the EMA: smoothed equals raw.
    #[test]
    fn first_tick_seeds_ema_raw_equals_smoothed() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        let tick = loop_.observe(1_000, dec(30_000));
        assert_eq!(tick.raw, dec(30_000));
        assert_eq!(tick.smoothed, dec(30_000));
        assert_eq!(loop_.smoothed(), Some(dec(30_000)));
    }

    /// AC part 2: both the raw and the smoothed mark reach the breaker (observer) on every tick, and the
    /// smoothed value lags the raw on a rising series (so smoothing is actually applied).
    #[test]
    fn both_raw_and_smoothed_reach_the_observer() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        let mut collector = Collector::default();
        let marks = vec![
            (1_000, dec(100)),
            (2_000, dec(200)),
            (3_000, dec(200)),
            (4_000, dec(200)),
        ];
        let ticks = loop_.drive(marks.clone(), &mut collector);

        assert_eq!(
            collector.ticks, ticks,
            "observer sees exactly the produced ticks"
        );
        // Every tick carries the raw input.
        for (tick, (_, raw)) in collector.ticks.iter().zip(marks.iter()) {
            assert_eq!(tick.raw, *raw);
        }
        // Seed tick: smoothed == raw.
        assert_eq!(collector.ticks[0].smoothed, dec(100));
        // After the jump to 200, the smoothed mark trails the raw (100 < smoothed < 200) — smoothing works.
        assert!(collector.ticks[1].smoothed > dec(100));
        assert!(collector.ticks[1].smoothed < dec(200));
        // Held at 200, the smoothed mark keeps rising toward raw but still lags.
        assert!(collector.ticks[2].smoothed > collector.ticks[1].smoothed);
        assert!(collector.ticks[2].smoothed < dec(200));
    }

    /// `drive` preserves input order and event times.
    #[test]
    fn drive_preserves_order_and_event_times() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        let mut collector = Collector::default();
        let marks = vec![(10, dec(1)), (20, dec(2)), (30, dec(3))];
        loop_.drive(marks, &mut collector);
        let times: Vec<i64> = collector.ticks.iter().map(|t| t.event_time_ms).collect();
        assert_eq!(times, vec![10, 20, 30]);
    }

    /// A `FnMut(&MarkTick)` closure is usable as a `MarkTickObserver` (the blanket impl).
    #[test]
    fn closure_observer_blanket_impl_works() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        let mut count = 0usize;
        let mut seen_raw = Decimal::ZERO;
        {
            let mut obs = |tick: &MarkTick| {
                count += 1;
                seen_raw = tick.raw;
            };
            loop_.drive(vec![(1, dec(5)), (2, dec(7))], &mut obs);
        }
        assert_eq!(count, 2);
        assert_eq!(seen_raw, dec(7));
    }
}
