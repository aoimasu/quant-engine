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

/// The default markPrice cadence, in seconds (markPrice@1s).
pub const DEFAULT_TICK_SECS: f64 = 1.0;

/// Default "mark stream stale" bound, in seconds (QE-417). A Δt above this between consecutive samples raises
/// [`MarkTick::stale`]. 5s ≈ 5× the nominal 1s cadence — tolerant of minor jitter, tripping on a genuine stall
/// (e.g. a wss reconnect gap). Never trips at the nominal 1s cadence, so today's behaviour is unchanged.
pub const DEFAULT_STALENESS_BOUND_SECS: f64 = 5.0;

/// The spec-baseline mark-EMA half-life, in seconds (τ½=60s per the QE-208 spec).
pub const DEFAULT_HALF_LIFE_SECS: f64 = 60.0;

/// Per-run runtime-risk config for the mark-EMA feed (QE-429, promoting the QE-417 constants).
///
/// The smoothing `half_life_secs`, nominal sample `tick_secs`, and `staleness_bound_secs` were hardcoded
/// constructor params/consts; this block promotes them to a per-run config so operators can tune the live
/// mark feed without a recompile. [`Default`] reproduces the spec baseline (τ½=60s, 1s cadence, 5s bound)
/// **byte-for-byte**, so adopting the config changes no behaviour (proven by
/// `from_config_default_matches_spec_baseline`).
///
/// **Runtime/live-only — never serialized into a vintage.** These knobs do not feed the seal, the
/// [`Lineage`](qe_determinism::Lineage), or `qe_config::Config::content_hash`, so they cannot move a
/// vintage content hash or any golden. (The QE-416 seal-time capacity/calibration constants, by contrast,
/// *do* feed the hash and are intentionally left hardcoded — see the QE-429 design note.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkEmaConfig {
    /// EMA half-life in seconds (the time for a held step to move the smoothed mark halfway).
    pub half_life_secs: f64,
    /// Nominal markPrice sample cadence in seconds (drives the seeding first tick's Δt).
    pub tick_secs: f64,
    /// Gap (seconds) above which a tick is flagged [`MarkTick::stale`].
    pub staleness_bound_secs: f64,
}

impl Default for MarkEmaConfig {
    /// The spec baseline: τ½=60s on 1-second markPrice ticks, 5s staleness bound — byte-identical to the
    /// pre-QE-429 hardcoded constants.
    fn default() -> Self {
        Self {
            half_life_secs: DEFAULT_HALF_LIFE_SECS,
            tick_secs: DEFAULT_TICK_SECS,
            staleness_bound_secs: DEFAULT_STALENESS_BOUND_SECS,
        }
    }
}

/// One mark observation: the raw markPrice@1s sample and its EMA-smoothed value at the same tick.
///
/// The `smoothed` value drives the slow/med-DD probe (spec baseline); `raw` is carried so the fast tier /
/// the QE-116 A3 raw-mark alternative can see un-averaged price. Both are available to breakers per the AC.
/// `stale` is the QE-417 "mark stream stale" health signal — set when the gap since the previous sample exceeds
/// the configured staleness bound — so the breaker/cockpit can halt or annotate on a stalled feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkTick {
    /// Venue event time of the mark sample (epoch ms).
    pub event_time_ms: i64,
    /// The raw mark price for this tick.
    pub raw: Decimal,
    /// The EMA-smoothed mark for this tick (== `raw` on the seeding first tick). Time-aware (QE-417): the gap
    /// since the previous sample sets the per-tick smoothing weight, so a post-gap sample is not under-smoothed.
    pub smoothed: Decimal,
    /// QE-417 health signal: `true` when the gap since the previous sample exceeded the staleness bound. Always
    /// `false` on the seeding first tick (no previous sample to measure a gap against).
    pub stale: bool,
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
///
/// Time-aware (QE-417): the loop tracks the previous sample's `event_time_ms` and drives the EMA with the actual
/// elapsed `Δt`, so a stream gap (wss reconnect) is smoothed as the real elapsed time rather than a single 1s
/// step. It also raises the [`MarkTick::stale`] health signal when `Δt` exceeds `staleness_bound_secs`. All
/// timing comes from the `event_time_ms` that already flows through the loop — no wall-clock read, so the tick
/// stream stays a deterministic function of `(samples, timestamps)`.
pub struct MarkEmaLoop {
    ema: MarkEma,
    /// Nominal cadence — used only to drive the seeding first tick (its Δt is irrelevant to a seed).
    tick_secs: f64,
    /// Gap (seconds) above which a tick is flagged [`MarkTick::stale`].
    staleness_bound_secs: f64,
    /// Event time of the previous sample, for the Δt measurement; `None` before the first tick.
    prev_event_time_ms: Option<i64>,
}

impl MarkEmaLoop {
    /// A loop with an EMA of half-life `half_life_secs` at a `tick_secs` sample spacing and the default
    /// staleness bound ([`DEFAULT_STALENESS_BOUND_SECS`]).
    #[must_use]
    pub fn with_half_life(half_life_secs: f64, tick_secs: f64) -> Self {
        Self::with_config(half_life_secs, tick_secs, DEFAULT_STALENESS_BOUND_SECS)
    }

    /// A fully-configured loop: EMA half-life, nominal `tick_secs` cadence, and the `staleness_bound_secs` above
    /// which a gap raises [`MarkTick::stale`] (QE-417). All three are config-driven.
    #[must_use]
    pub fn with_config(half_life_secs: f64, tick_secs: f64, staleness_bound_secs: f64) -> Self {
        Self {
            ema: MarkEma::with_half_life(half_life_secs, tick_secs),
            tick_secs,
            staleness_bound_secs,
            prev_event_time_ms: None,
        }
    }

    /// A loop from a per-run [`MarkEmaConfig`] (QE-429) — the promoted-to-config construction path. Threads
    /// the config's half-life, cadence, and staleness bound to the EMA loop. `MarkEmaConfig::default()`
    /// yields the spec baseline byte-for-byte.
    #[must_use]
    pub fn from_config(cfg: &MarkEmaConfig) -> Self {
        Self::with_config(cfg.half_life_secs, cfg.tick_secs, cfg.staleness_bound_secs)
    }

    /// The spec baseline: EMA τ½=60s on 1-second markPrice ticks, default staleness bound.
    #[must_use]
    pub fn spec_baseline() -> Self {
        Self::from_config(&MarkEmaConfig::default())
    }

    /// The configured staleness bound (seconds) — the Δt above which [`MarkTick::stale`] is raised.
    #[must_use]
    pub fn staleness_bound_secs(&self) -> f64 {
        self.staleness_bound_secs
    }

    /// Observe one raw mark sample: derive the elapsed `Δt` from the previous sample's event time, update the
    /// EMA time-aware, flag staleness, and return the tick (raw + smoothed + stale). The first sample seeds the
    /// EMA, so its `smoothed == raw` and `stale == false`.
    pub fn observe(&mut self, event_time_ms: i64, raw: Decimal) -> MarkTick {
        // Δt from the previous event time; the seeding first tick has no previous sample. Integer subtraction of
        // the timestamps that already flow here — no wall-clock read (determinism).
        let dt_secs = match self.prev_event_time_ms {
            Some(prev) => (event_time_ms - prev).max(0) as f64 / 1000.0,
            None => self.tick_secs,
        };
        let smoothed = self.ema.update_after(dt_secs, raw);
        let stale = self.prev_event_time_ms.is_some() && dt_secs > self.staleness_bound_secs;
        self.prev_event_time_ms = Some(event_time_ms);
        MarkTick {
            event_time_ms,
            raw,
            smoothed,
            stale,
        }
    }

    /// The current smoothed mark, if any sample has been observed.
    #[must_use]
    pub fn smoothed(&self) -> Option<Decimal> {
        self.ema.value()
    }

    /// Drive an ordered sequence of `(event_time_ms, raw)` marks, forwarding each produced [`MarkTick`] to
    /// `observer` (the breaker feed) and returning the ticks in arrival order.
    ///
    /// The returned `Vec` and the observer receive the *same* ticks — the vector is a convenience for
    /// callers that also want the batch (e.g. tests / logging); it is not a second dispatch. Callers that
    /// only need the streaming side can ignore the return value.
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
        // After one half-life the smoothed value is exactly ~50 (the only error is alpha's f64→Decimal
        // rounding), so a tight tolerance is a strong guard against a regression in the alpha formula.
        let smoothed: f64 = last.smoothed.try_into().unwrap();
        assert!(
            (smoothed - 50.0).abs() < 0.05,
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

    /// QE-417 (a): a 300s gap then a step yields a smoothed value consistent with 300s of elapsed time — the
    /// EMA nearly jumps to the post-gap price, far past the ~1% move a single 1s step would give.
    #[test]
    fn gap_makes_ema_nearly_reseed_to_post_gap_price() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        loop_.observe(0, dec(100)); // seed at t=0
                                    // Next sample arrives 300s later at 200 (a wss-reconnect gap).
        let after_gap = loop_.observe(300_000, dec(200));
        // alpha(300s) = 1 - 0.5^(300/60) = 0.96875 -> 100 + 0.96875*100 = 196.875.
        let s: f64 = after_gap.smoothed.try_into().unwrap();
        assert!(s > 195.0, "300s gap should nearly reseed, got {s}");
        // A single 1s step would only reach ~101.15 — prove the gap-aware value is far above that.
        assert!(
            s > 190.0,
            "gap-aware value must dwarf a single-1s-step value (~101), got {s}"
        );
    }

    /// QE-417 (b): at the nominal 1s spacing the time-aware smoothed series is byte-identical to the old
    /// fixed-alpha behaviour (backward-compat — no golden churn).
    #[test]
    fn one_second_spacing_matches_old_fixed_alpha_series() {
        let mut loop_ = MarkEmaLoop::spec_baseline();
        // Reference: the pre-QE-417 behaviour was exactly `MarkEma::update` (fixed nominal alpha) per tick.
        let mut reference = qe_risk::MarkEma::with_half_life(60.0, 1.0);
        let prices = [dec(100), dec(120), dec(120), dec(80), dec(95), dec(95)];
        for (i, p) in prices.into_iter().enumerate() {
            let t = (i as i64) * 1_000; // strict 1s spacing
            let tick = loop_.observe(t, p);
            assert_eq!(
                tick.smoothed,
                reference.update(p),
                "1s-spaced smoothed series must equal the old fixed-alpha series at tick {i}"
            );
            assert!(!tick.stale, "a 1s gap is never stale");
        }
    }

    /// QE-417 (c): a Δt beyond the staleness bound raises the stale health signal; a nominal gap does not.
    #[test]
    fn gap_beyond_bound_raises_stale_signal() {
        // Bound = 5s.
        let mut loop_ = MarkEmaLoop::spec_baseline();
        assert_eq!(loop_.staleness_bound_secs(), 5.0);
        let seed = loop_.observe(0, dec(100));
        assert!(!seed.stale, "the seeding first tick is never stale");
        // A 1s gap: not stale.
        assert!(!loop_.observe(1_000, dec(100)).stale);
        // A 30s gap (> 5s bound): stale.
        let stalled = loop_.observe(31_000, dec(100));
        assert!(stalled.stale, "a 30s gap must raise the stale signal");
        // Back to nominal cadence: clears.
        assert!(!loop_.observe(32_000, dec(100)).stale);
    }

    /// QE-429: `MarkEmaConfig::default()` reproduces `spec_baseline()` tick-for-tick — promoting the
    /// constants to config is behaviour-preserving (no golden/mark-feed churn).
    #[test]
    fn from_config_default_matches_spec_baseline() {
        let cfg = MarkEmaConfig::default();
        assert_eq!(cfg.half_life_secs, 60.0);
        assert_eq!(cfg.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(cfg.staleness_bound_secs, DEFAULT_STALENESS_BOUND_SECS);

        let mut from_cfg = MarkEmaLoop::from_config(&cfg);
        let mut baseline = MarkEmaLoop::spec_baseline();
        assert_eq!(
            from_cfg.staleness_bound_secs(),
            baseline.staleness_bound_secs()
        );
        // A gappy, jumpy series exercises the time-aware alpha + the stale signal on both loops.
        let marks = [
            (0, dec(100)),
            (1_000, dec(120)),
            (2_000, dec(120)),
            (33_000, dec(80)), // a >5s gap → stale, and a large gap-aware alpha
            (34_000, dec(95)),
        ];
        for (t, p) in marks {
            assert_eq!(
                from_cfg.observe(t, p),
                baseline.observe(t, p),
                "from_config(default) must equal spec_baseline tick-for-tick"
            );
        }
    }

    /// QE-429: a non-default config threads its knobs through (distinct behaviour from the baseline).
    #[test]
    fn from_config_threads_non_default_knobs() {
        let cfg = MarkEmaConfig {
            half_life_secs: 10.0,
            tick_secs: 1.0,
            staleness_bound_secs: 2.0,
        };
        let mut loop_ = MarkEmaLoop::from_config(&cfg);
        assert_eq!(loop_.staleness_bound_secs(), 2.0);
        loop_.observe(0, dec(100));
        // A 3s gap exceeds the tightened 2s bound → stale (the default 5s bound would not trip here).
        assert!(loop_.observe(3_000, dec(100)).stale);
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
