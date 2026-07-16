//! Live kline source — REST prime + wss stitch → streaming multi-resolution reconstruction (QE-205).
//!
//! Live coarser bars must be reconstructed by streaming, primed by REST and stitched to wss, using the
//! **same** reconstruction as batch. This source adds only the stitch (a monotonic open-time dedup over
//! the prime/wss boundary) and a per-tier fan-out in front of the unmodified QE-106
//! [`BarReconstructor`](qe_signal::reconstruct::BarReconstructor) — so its per-tier output is, bar-for-bar,
//! `reconstruct_batch` over the same base sequence (the parity guarantee, AC).

use qe_domain::{Bar, Resolution};
use qe_signal::reconstruct::{BarReconstructor, ReconError};

/// Reconstructs live multi-resolution bars from a stitched stream of base bars.
///
/// REST primes it with closed historical base bars; wss continues with live closed base bars. The two
/// overlap at the boundary — the stitch drops any bar already covered (`open_time <= last_open_ms`) so the
/// base sequence the reconstructors see is gap-free and strictly increasing regardless of which side a bar
/// came from.
pub struct LiveKlineSource {
    base: Resolution,
    /// One reconstructor per target tier, in the order tiers were given (output order within a step).
    reconstructors: Vec<(Resolution, BarReconstructor)>,
    /// The stitch marker: the greatest base `open_time` (ms) accepted so far.
    last_open_ms: Option<i64>,
}

impl LiveKlineSource {
    /// A source rolling `base` bars up to each of `tiers`.
    ///
    /// # Errors
    /// [`ReconError`] if any tier is not a strictly-coarser integer multiple of `base`.
    pub fn new(base: Resolution, tiers: &[Resolution]) -> Result<Self, ReconError> {
        let mut reconstructors = Vec::with_capacity(tiers.len());
        for &target in tiers {
            reconstructors.push((target, BarReconstructor::new(base, target)?));
        }
        Ok(Self {
            base,
            reconstructors,
            last_open_ms: None,
        })
    }

    /// The stitch marker — the greatest base open-time accepted so far (`None` before any bar).
    #[must_use]
    pub fn last_open_ms(&self) -> Option<i64> {
        self.last_open_ms
    }

    /// Accept one base bar: stitch dedup, then fan out to every tier's reconstructor. A bar already
    /// covered by the marker (`open_time <= last_open_ms`) is dropped (returns no completed bars and does
    /// not advance the marker). Otherwise it advances the marker and any completed coarser bars are
    /// returned, in tier order.
    ///
    /// # Errors
    /// [`ReconError::UnexpectedResolution`] if the bar is not the base resolution; [`ReconError::InvalidBar`]
    /// if a flushed roll-up fails validation.
    fn accept(&mut self, bar: &Bar) -> Result<Vec<Bar>, ReconError> {
        if bar.resolution() != self.base {
            return Err(ReconError::UnexpectedResolution {
                expected: self.base,
                found: bar.resolution(),
            });
        }
        // Stitch: a bar at or before the marker is already covered (the REST/wss overlap) — drop it.
        if let Some(last) = self.last_open_ms {
            if bar.open_time().millis() <= last {
                return Ok(Vec::new());
            }
        }
        self.last_open_ms = Some(bar.open_time().millis());

        let mut completed = Vec::new();
        for (_tier, recon) in &mut self.reconstructors {
            if let Some(done) = recon.push(bar)? {
                completed.push(done);
            }
        }
        Ok(completed)
    }

    /// Prime the source from REST with closed historical base bars (ascending). Returns any coarser bars
    /// completed during priming.
    ///
    /// # Errors
    /// [`ReconError`] as for [`accept`](Self::accept).
    pub fn prime(&mut self, base_bars: &[Bar]) -> Result<Vec<Bar>, ReconError> {
        let mut out = Vec::new();
        for bar in base_bars {
            out.extend(self.accept(bar)?);
        }
        Ok(out)
    }

    /// Feed one live (wss) closed base bar, stitched onto the primed sequence. Returns any coarser bars it
    /// completes (empty if the bar was a boundary duplicate).
    ///
    /// # Errors
    /// [`ReconError`] as for [`accept`](Self::accept).
    pub fn push_live(&mut self, bar: &Bar) -> Result<Vec<Bar>, ReconError> {
        self.accept(bar)
    }

    /// Flush every tier's final in-progress window (e.g. on shutdown). Returns the trailing coarser bars in
    /// tier order.
    ///
    /// # Errors
    /// [`ReconError::InvalidBar`] if a final roll-up fails validation.
    pub fn finish(&mut self) -> Result<Vec<Bar>, ReconError> {
        let mut out = Vec::new();
        for (_tier, recon) in &mut self.reconstructors {
            if let Some(done) = recon.finish()? {
                out.push(done);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Timestamp};
    use qe_signal::reconstruct::reconstruct_batch;
    use rust_decimal::Decimal;

    const MIN: i64 = 60_000;

    fn p(n: i64) -> Price {
        Price::new(Decimal::from(n)).unwrap()
    }
    fn q(n: i64) -> Qty {
        Qty::new(Decimal::from(n)).unwrap()
    }

    /// A 5m base bar at the `slot`-th 5-minute slot.
    fn base_bar(slot: i64, o: i64, h: i64, l: i64, c: i64, v: i64, trades: u64) -> Bar {
        Bar::new(
            Timestamp::from_millis(slot * 5 * MIN),
            Resolution::M5,
            p(o),
            p(h),
            p(l),
            p(c),
            q(v),
            trades,
        )
        .unwrap()
    }

    /// A varied base series of `n` 5m bars.
    fn base_series(n: i64) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let base = 100 + (i % 13);
                base_bar(
                    i,
                    base,
                    base + 5,
                    base - 4,
                    base + 1,
                    10 + (i % 7),
                    1 + (i as u64 % 3),
                )
            })
            .collect()
    }

    /// Collect a source's emitted bars for one tier (time-ordered).
    fn tier_bars(emitted: &[Bar], tier: Resolution) -> Vec<Bar> {
        emitted
            .iter()
            .filter(|b| b.resolution() == tier)
            .cloned()
            .collect()
    }

    #[test]
    fn prime_then_stitched_live_equals_batch_parity() {
        // 26 base 5m bars span 30m windows [0,30),[30,60),... and one 4h window worth at the start.
        let all = base_series(26);
        let tiers = [Resolution::M30, Resolution::H4];

        // Split: prime the first 14 bars (REST), stream the rest via wss — but the first wss bar
        // re-delivers the last primed bar (the boundary overlap the stitch must drop).
        let prime_prefix = &all[..14];
        let live_suffix = &all[13..]; // index 13 duplicates the last primed bar (index 13)

        let mut src = LiveKlineSource::new(Resolution::M5, &tiers).unwrap();
        let mut emitted = src.prime(prime_prefix).unwrap();
        assert_eq!(src.last_open_ms(), Some(13 * 5 * MIN));
        for bar in live_suffix {
            emitted.extend(src.push_live(bar).unwrap());
        }
        emitted.extend(src.finish().unwrap());

        // The deduped base sequence is just `all` (the overlap removed) — batch over it is the reference.
        for tier in tiers {
            let batch = reconstruct_batch(&all, Resolution::M5, tier).unwrap();
            assert_eq!(
                tier_bars(&emitted, tier),
                batch,
                "streaming {tier} bars must equal batch reconstruction"
            );
            assert!(!batch.is_empty(), "tier {tier} should produce bars");
        }
    }

    #[test]
    fn stitch_drops_overlap_and_keeps_order() {
        let mut src = LiveKlineSource::new(Resolution::M5, &[Resolution::M30]).unwrap();
        src.prime(&base_series(3)).unwrap(); // slots 0,1,2 → last_open = 2*5m
        assert_eq!(src.last_open_ms(), Some(2 * 5 * MIN));

        // A duplicate (slot 2) and an older bar (slot 1) are both dropped; the marker is unchanged.
        assert!(src.push_live(&base_series(3)[2]).unwrap().is_empty());
        assert!(src.push_live(&base_series(3)[1]).unwrap().is_empty());
        assert_eq!(src.last_open_ms(), Some(2 * 5 * MIN));

        // A strictly-later bar advances the marker.
        let later = base_bar(3, 100, 101, 99, 100, 1, 1);
        src.push_live(&later).unwrap();
        assert_eq!(src.last_open_ms(), Some(3 * 5 * MIN));
    }

    #[test]
    fn multi_tier_fan_out_counts_match_batch() {
        // 48 5m bars = one full 4h window = eight 30m windows.
        let bars = base_series(48);
        let tiers = [Resolution::M30, Resolution::H4];
        let mut src = LiveKlineSource::new(Resolution::M5, &tiers).unwrap();
        let mut emitted = src.prime(&bars).unwrap();
        emitted.extend(src.finish().unwrap());

        assert_eq!(tier_bars(&emitted, Resolution::M30).len(), 8);
        assert_eq!(tier_bars(&emitted, Resolution::H4).len(), 1);
        for tier in tiers {
            assert_eq!(
                tier_bars(&emitted, tier),
                reconstruct_batch(&bars, Resolution::M5, tier).unwrap()
            );
        }
    }

    #[test]
    fn wrong_base_resolution_is_rejected() {
        let mut src = LiveKlineSource::new(Resolution::M5, &[Resolution::M30]).unwrap();
        let wrong = Bar::new(
            Timestamp::from_millis(0),
            Resolution::M15,
            p(100),
            p(100),
            p(100),
            p(100),
            q(1),
            1,
        )
        .unwrap();
        assert_eq!(
            src.push_live(&wrong).unwrap_err(),
            ReconError::UnexpectedResolution {
                expected: Resolution::M5,
                found: Resolution::M15,
            }
        );
    }

    #[test]
    fn live_only_with_no_prime_still_equals_batch() {
        let bars = base_series(20);
        let tiers = [Resolution::M30];
        let mut src = LiveKlineSource::new(Resolution::M5, &tiers).unwrap();
        let mut emitted = Vec::new();
        for bar in &bars {
            emitted.extend(src.push_live(bar).unwrap());
        }
        emitted.extend(src.finish().unwrap());
        assert_eq!(
            tier_bars(&emitted, Resolution::M30),
            reconstruct_batch(&bars, Resolution::M5, Resolution::M30).unwrap()
        );
    }
}
