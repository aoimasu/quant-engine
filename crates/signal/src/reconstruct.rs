//! Multi-resolution bar reconstruction (QE-106).
//!
//! Roll a stream of base bars (5m) up into a coarser resolution (30m, 4h, …) with **deterministic,
//! epoch-aligned boundaries**. The same incremental fold drives both batch ([`reconstruct_batch`])
//! and streaming ([`BarReconstructor`]) — batch is literally streaming fed the whole slice — so the
//! two are byte-identical by construction (the QE-206 batch/streaming parity guarantee).

use rust_decimal::Decimal;

use qe_domain::{Bar, DomainError, Price, Qty, Resolution, Timestamp};
use thiserror::Error;

/// Why reconstruction could not proceed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReconError {
    /// The target resolution is not strictly coarser than the base.
    #[error("target {target} is not coarser than base {base}")]
    TargetNotCoarser {
        /// The base resolution.
        base: Resolution,
        /// The requested target.
        target: Resolution,
    },
    /// The target is not an integer multiple of the base (boundaries would not align).
    #[error("target {target} is not an integer multiple of base {base}")]
    TargetNotMultiple {
        /// The base resolution.
        base: Resolution,
        /// The requested target.
        target: Resolution,
    },
    /// An input bar's resolution did not match the configured base.
    #[error("expected base resolution {expected}, got {found}")]
    UnexpectedResolution {
        /// The configured base.
        expected: Resolution,
        /// The bar's actual resolution.
        found: Resolution,
    },
    /// A rolled-up bar failed domain validation (should not happen for valid base bars).
    #[error("reconstructed bar invalid: {0}")]
    InvalidBar(#[from] DomainError),
}

/// The in-progress aggregate for one target window. Volume sums as an exact [`Decimal`] (the domain
/// `Qty` is non-negative-by-construction and doesn't implement `Add`); it is re-wrapped at finish.
#[derive(Debug, Clone)]
struct Window {
    start_ms: i64,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: Decimal,
    trades: u64,
}

impl Window {
    fn open_from(bar: &Bar, start_ms: i64) -> Self {
        Window {
            start_ms,
            open: bar.open(),
            high: bar.high(),
            low: bar.low(),
            close: bar.close(),
            volume: bar.volume().get(),
            trades: bar.trades(),
        }
    }

    fn fold(&mut self, bar: &Bar) {
        if bar.high() > self.high {
            self.high = bar.high();
        }
        if bar.low() < self.low {
            self.low = bar.low();
        }
        self.close = bar.close();
        self.volume += bar.volume().get();
        self.trades += bar.trades();
    }

    fn finish(self, target: Resolution) -> Result<Bar, ReconError> {
        Ok(Bar::new(
            Timestamp::from_millis(self.start_ms),
            target,
            self.open,
            self.high,
            self.low,
            self.close,
            Qty::new(self.volume)?,
            self.trades,
        )?)
    }
}

/// An incremental, deterministic bar roll-up: feed base bars in ascending time order; a completed
/// coarser bar is emitted when an incoming bar crosses into the next target window.
///
/// Shared by batch and streaming so they cannot diverge (QE-206). Input is expected ascending by
/// `open_time`; out-of-order input merely splits a window the same way in both modes, so parity
/// holds regardless.
#[derive(Debug, Clone)]
pub struct BarReconstructor {
    base: Resolution,
    target: Resolution,
    target_ms: i64,
    current: Option<Window>,
}

impl BarReconstructor {
    /// Create a reconstructor rolling `base` bars up to `target`.
    ///
    /// # Errors
    /// [`ReconError::TargetNotCoarser`] if `target ≤ base`; [`ReconError::TargetNotMultiple`] if
    /// `target` is not an integer multiple of `base`.
    pub fn new(base: Resolution, target: Resolution) -> Result<Self, ReconError> {
        if target.minutes() <= base.minutes() {
            return Err(ReconError::TargetNotCoarser { base, target });
        }
        if !target.minutes().is_multiple_of(base.minutes()) {
            return Err(ReconError::TargetNotMultiple { base, target });
        }
        Ok(BarReconstructor {
            base,
            target,
            target_ms: i64::from(target.minutes()) * 60_000,
            current: None,
        })
    }

    /// The epoch-aligned target-window start for a timestamp.
    fn window_start(&self, open_time: Timestamp) -> i64 {
        open_time.millis().div_euclid(self.target_ms) * self.target_ms
    }

    /// Feed one base bar. Returns a completed coarser bar when this bar opens a new target window.
    ///
    /// # Errors
    /// [`ReconError::UnexpectedResolution`] if the bar's resolution is not the configured base;
    /// [`ReconError::InvalidBar`] if a flushed roll-up fails validation.
    pub fn push(&mut self, bar: &Bar) -> Result<Option<Bar>, ReconError> {
        if bar.resolution() != self.base {
            return Err(ReconError::UnexpectedResolution {
                expected: self.base,
                found: bar.resolution(),
            });
        }
        let start_ms = self.window_start(bar.open_time());
        match &mut self.current {
            Some(window) if window.start_ms == start_ms => {
                window.fold(bar);
                Ok(None)
            }
            _ => {
                let completed = self.current.take();
                self.current = Some(Window::open_from(bar, start_ms));
                completed.map(|w| w.finish(self.target)).transpose()
            }
        }
    }

    /// Flush the final in-progress window, if any.
    ///
    /// # Errors
    /// [`ReconError::InvalidBar`] if the final roll-up fails validation.
    pub fn finish(&mut self) -> Result<Option<Bar>, ReconError> {
        self.current
            .take()
            .map(|w| w.finish(self.target))
            .transpose()
    }
}

/// Reconstruct `base_bars` into a single coarser `target` series (batch).
///
/// Equivalent to pushing every bar through a [`BarReconstructor`] then [`BarReconstructor::finish`]
/// — the same code path as streaming, so the two are identical (AC: batch == streaming parity).
///
/// # Errors
/// [`ReconError`] on an invalid target or an input bar with the wrong resolution.
pub fn reconstruct_batch(
    base_bars: &[Bar],
    base: Resolution,
    target: Resolution,
) -> Result<Vec<Bar>, ReconError> {
    let mut recon = BarReconstructor::new(base, target)?;
    let mut out = Vec::new();
    for bar in base_bars {
        if let Some(done) = recon.push(bar)? {
            out.push(done);
        }
    }
    if let Some(done) = recon.finish()? {
        out.push(done);
    }
    Ok(out)
}

/// Reconstruct `base_bars` into every resolution in `tiers` (configurable tier set), concatenated.
///
/// Each tier is reconstructed independently from the base series, so the result groups all of one
/// tier's bars before the next.
///
/// # Errors
/// [`ReconError`] if any tier is an invalid target or an input bar has the wrong resolution.
pub fn reconstruct_tiers(
    base_bars: &[Bar],
    base: Resolution,
    tiers: &[Resolution],
) -> Result<Vec<Bar>, ReconError> {
    let mut out = Vec::new();
    for &target in tiers {
        out.extend(reconstruct_batch(base_bars, base, target)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    const MIN: i64 = 60_000;

    fn price(n: i64) -> Price {
        Price::new(Decimal::from(n)).unwrap()
    }
    fn qty(n: i64) -> Qty {
        Qty::new(Decimal::from(n)).unwrap()
    }

    /// A 5m base bar at `min5`-th 5-minute slot, with explicit OHLCV.
    fn base_bar(open_min: i64, o: i64, h: i64, l: i64, c: i64, v: i64, trades: u64) -> Bar {
        Bar::new(
            Timestamp::from_millis(open_min * MIN),
            Resolution::M5,
            price(o),
            price(h),
            price(l),
            price(c),
            qty(v),
            trades,
        )
        .unwrap()
    }

    /// Six 5m bars covering one 30m window [0, 30m), with varied highs/lows.
    fn six_5m() -> Vec<Bar> {
        vec![
            base_bar(0, 100, 105, 99, 104, 10, 1),
            base_bar(5, 104, 110, 103, 108, 12, 2),
            base_bar(10, 108, 109, 101, 102, 8, 1),
            base_bar(15, 102, 107, 100, 106, 9, 3),
            base_bar(20, 106, 112, 105, 111, 11, 2),
            base_bar(25, 111, 113, 95, 97, 20, 5),
        ]
    }

    #[test]
    fn rolls_up_one_window_to_hand_computed_values() {
        let bars = six_5m();
        let out = reconstruct_batch(&bars, Resolution::M5, Resolution::M30).unwrap();
        assert_eq!(out.len(), 1);
        let b = &out[0];
        assert_eq!(b.resolution(), Resolution::M30);
        assert_eq!(b.open_time(), Timestamp::from_millis(0));
        assert_eq!(b.open().get(), Decimal::from(100)); // first open
        assert_eq!(b.high().get(), Decimal::from(113)); // max high (bar 25)
        assert_eq!(b.low().get(), Decimal::from(95)); // min low (bar 25)
        assert_eq!(b.close().get(), Decimal::from(97)); // last close
        assert_eq!(b.volume().get(), Decimal::from(70)); // sum 10+12+8+9+11+20
        assert_eq!(b.trades(), 14); // 1+2+1+3+2+5
    }

    #[test]
    fn batch_equals_streaming_parity() {
        // AC: feeding the same input as a batch vs one-at-a-time streaming yields identical output.
        // Span 70 minutes → 30m windows [0,30),[30,60),[60,90) (the last partial).
        let mut bars = six_5m();
        bars.extend((6..14).map(|i| base_bar(i * 5, 100 + i, 120 + i, 90, 100 + i, 5, 1)));

        let batch = reconstruct_batch(&bars, Resolution::M5, Resolution::M30).unwrap();

        let mut recon = BarReconstructor::new(Resolution::M5, Resolution::M30).unwrap();
        let mut streamed = Vec::new();
        for bar in &bars {
            if let Some(done) = recon.push(bar).unwrap() {
                streamed.push(done);
            }
        }
        if let Some(done) = recon.finish().unwrap() {
            streamed.push(done);
        }

        assert_eq!(batch, streamed);
        assert_eq!(batch.len(), 3); // [0,30),[30,60),[60,90)
        assert_eq!(batch[0].open_time(), Timestamp::from_millis(0));
        assert_eq!(batch[1].open_time(), Timestamp::from_millis(30 * MIN));
        assert_eq!(batch[2].open_time(), Timestamp::from_millis(60 * MIN));
    }

    #[test]
    fn windows_align_to_epoch_boundary_not_first_bar() {
        // First base bar at the 2nd 5m slot (10m) → still falls in the 30m window starting at 0.
        let bars = vec![
            base_bar(2, 100, 100, 100, 100, 1, 1),
            base_bar(7, 100, 101, 99, 100, 1, 1),
        ];
        let out = reconstruct_batch(&bars, Resolution::M5, Resolution::M30).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].open_time(), Timestamp::from_millis(0));
    }

    #[test]
    fn reconstruct_tiers_yields_all_configured_tiers() {
        // 48 5m bars = one full 4h window = eight 30m windows.
        let bars: Vec<Bar> = (0..48)
            .map(|i| base_bar(i * 5, 100, 100, 100, 100, 1, 1))
            .collect();
        let out =
            reconstruct_tiers(&bars, Resolution::M5, &[Resolution::M30, Resolution::H4]).unwrap();
        let m30 = out
            .iter()
            .filter(|b| b.resolution() == Resolution::M30)
            .count();
        let h4 = out
            .iter()
            .filter(|b| b.resolution() == Resolution::H4)
            .count();
        assert_eq!(m30, 8);
        assert_eq!(h4, 1);
    }

    #[test]
    fn rejects_non_coarser_target() {
        // A target no coarser than the base is rejected (the common misconfiguration).
        assert_eq!(
            BarReconstructor::new(Resolution::M30, Resolution::M5).unwrap_err(),
            ReconError::TargetNotCoarser {
                base: Resolution::M30,
                target: Resolution::M5
            }
        );
        assert_eq!(
            BarReconstructor::new(Resolution::M5, Resolution::M5).unwrap_err(),
            ReconError::TargetNotCoarser {
                base: Resolution::M5,
                target: Resolution::M5
            }
        );
        // Note: `TargetNotMultiple` is a defensive guard — every coarser tier in the current
        // `Resolution` enum is already an integer multiple of every finer one (durations
        // 5,15,30,60,240,720,1440 min), so no real pair triggers it. It guards a future
        // non-aligned addition (e.g. a hypothetical 45m).
    }

    #[test]
    fn rejects_wrong_resolution_input() {
        let mut recon = BarReconstructor::new(Resolution::M5, Resolution::M30).unwrap();
        let wrong = base_bar_res(Resolution::M15, 0);
        assert_eq!(
            recon.push(&wrong).unwrap_err(),
            ReconError::UnexpectedResolution {
                expected: Resolution::M5,
                found: Resolution::M15
            }
        );
    }

    fn base_bar_res(res: Resolution, open_min: i64) -> Bar {
        Bar::new(
            Timestamp::from_millis(open_min * MIN),
            res,
            price(100),
            price(100),
            price(100),
            price(100),
            qty(1),
            1,
        )
        .unwrap()
    }
}
