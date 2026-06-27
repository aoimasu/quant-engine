//! Fusion (QE-104): coalesce + normalise + temporally align the canonical series onto one base
//! grid, producing a deterministic [`FusedCorpus`].
//!
//! Alignment consumes the QE-103 [`crate::fill::plan_fill`] plan: within-bound misses carry the
//! last present value forward, over-bound runs stay **holes** (NaN) — no leakage across a wide gap.
//! The corpus serialises to canonical JSON bytes (byte-reproducible for fixed inputs, AC #1); the
//! `arrow` feature adds the Arrow IPC artefact (see [`crate::arrow`]).

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::Serialize;

use qe_domain::{Bar, InstrumentId, Resolution, TimeInterval, Timestamp};

use crate::canonical::CanonicalSeries;
use crate::coalesce::coalesce_bars;
use crate::derive::{adjust_bar, Adjustment};
use crate::fill::plan_fill;

/// The fixed-interval base grid the corpus is aligned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Grid {
    /// First slot (inclusive).
    pub start: Timestamp,
    /// Grid end (exclusive).
    pub end: Timestamp,
    /// Slot spacing in milliseconds.
    pub interval_ms: i64,
}

impl Grid {
    /// Build a grid spanning `window` at `resolution` (the base bar, `M5` per the project default).
    #[must_use]
    pub fn from_window(window: TimeInterval, resolution: Resolution) -> Grid {
        Grid {
            start: window.start(),
            end: window.end(),
            interval_ms: i64::from(resolution.minutes()) * 60_000,
        }
    }

    /// The grid slot timestamps: `start, start+interval, … < end`.
    #[must_use]
    pub fn slots(&self) -> Vec<i64> {
        let mut out = Vec::new();
        if self.interval_ms <= 0 {
            return out;
        }
        let mut t = self.start.millis();
        while t < self.end.millis() {
            out.push(t);
            t += self.interval_ms;
        }
        out
    }

    /// Number of slots in the grid.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots().len()
    }

    /// Whether the grid has no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One aligned slot of a column: a real/forward-filled value (with its source timestamp), or a
/// leakage-safe hole left empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cell {
    /// A present or forward-filled value, carried from `from_ms` (`== slot` when present).
    Value {
        /// The aligned value (exact decimal).
        #[serde(with = "rust_decimal::serde::str")]
        value: Decimal,
        /// The source timestamp the value was carried from.
        from_ms: i64,
    },
    /// An over-bound (or leading) miss — left NaN, never filled across (QE-103 AC #1).
    Hole,
}

impl Cell {
    /// The value if present/filled, else `None` (a hole).
    #[must_use]
    pub fn value(&self) -> Option<Decimal> {
        match self {
            Cell::Value { value, .. } => Some(*value),
            Cell::Hole => None,
        }
    }
}

/// One canonical series aligned to the grid (one [`Cell`] per slot, in slot order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FusedColumn {
    /// Which canonical series this column carries.
    pub series: CanonicalSeries,
    /// One cell per grid slot.
    pub cells: Vec<Cell>,
}

/// The fused, normalised, temporally-aligned corpus for one instrument over one window.
///
/// Columns are emitted in [`CanonicalSeries::ALL`] order and the grid is fixed, so
/// [`FusedCorpus::to_json_bytes`] is byte-reproducible for fixed inputs (AC #1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FusedCorpus {
    /// The instrument this corpus is for.
    pub instrument: InstrumentId,
    /// The base resolution.
    pub resolution: Resolution,
    /// The base grid.
    pub grid: Grid,
    /// The grid slot timestamps (epoch-ms), ascending.
    pub slots: Vec<i64>,
    /// One column per canonical series, in `CanonicalSeries::ALL` order.
    pub columns: Vec<FusedColumn>,
}

impl FusedCorpus {
    /// Serialise to canonical JSON bytes — the deterministic, byte-reproducible artefact (AC #1).
    ///
    /// # Errors
    /// [`serde_json::Error`] if serialisation fails (it shouldn't for this `Vec`/scalar shape).
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// The column for `series`, if present.
    #[must_use]
    pub fn column(&self, series: CanonicalSeries) -> Option<&FusedColumn> {
        self.columns.iter().find(|c| c.series == series)
    }
}

/// Align a set of present `(open_time_ms, value)` samples onto `grid`, forward-filling within
/// `max_gap_ms` and leaving over-bound/leading runs as [`Cell::Hole`].
///
/// The fill decision is delegated to QE-103 [`plan_fill`], so the leakage-safety guarantee is
/// shared with the integrity layer rather than re-derived here.
///
/// **Precondition:** in-window samples are expected to fall **on the grid phase** (`open_time ==
/// start + k·interval`). A sample inside `[start, end)` but off-phase matches no slot and is
/// **silently dropped** — callers must pre-align to the base resolution first (kline series already
/// are, by construction, so [`fuse`] satisfies this). Violations are caught by a `debug_assert` in
/// debug builds. Out-of-window samples are intentionally ignored (windowing).
#[must_use]
pub fn align_onto_grid(present: &[(i64, Decimal)], grid: &Grid, max_gap_ms: i64) -> Vec<Cell> {
    let present_map: BTreeMap<i64, Decimal> = present.iter().copied().collect();
    debug_assert!(
        grid.interval_ms <= 0
            || present_map.keys().all(|&t| {
                t < grid.start.millis()
                    || t >= grid.end.millis()
                    || (t - grid.start.millis()).rem_euclid(grid.interval_ms) == 0
            }),
        "align_onto_grid: an in-window present sample is not aligned to the grid phase"
    );
    let timestamps: Vec<i64> = present_map.keys().copied().collect();
    let plan = plan_fill(
        &timestamps,
        grid.interval_ms,
        grid.start.millis(),
        grid.end.millis(),
        max_gap_ms,
    );
    let filled: BTreeMap<i64, i64> = plan.filled.iter().map(|f| (f.slot_ms, f.from_ms)).collect();

    grid.slots()
        .into_iter()
        .map(|slot| {
            if let Some(&value) = present_map.get(&slot) {
                Cell::Value {
                    value,
                    from_ms: slot,
                }
            } else if let Some(&from_ms) = filled.get(&slot) {
                // Carry the source sample's value forward (guaranteed present by plan_fill).
                Cell::Value {
                    value: present_map[&from_ms],
                    from_ms,
                }
            } else {
                Cell::Hole
            }
        })
        .collect()
}

/// The raw per-series inputs to fusion for one instrument/window. Kline series arrive as daily
/// partitions (coalesced internally); the scalar series arrive as `(time_ms, value)` samples.
#[derive(Debug, Clone)]
pub struct FusionInput {
    /// The instrument.
    pub instrument: InstrumentId,
    /// The window to align over.
    pub window: TimeInterval,
    /// The base resolution (e.g. `M5`).
    pub resolution: Resolution,
    /// Forward-fill bound (ms): misses wider than this stay holes.
    pub max_gap_ms: i64,
    /// Split/contract adjustment applied to every kline bar (default identity).
    pub adjustment: Adjustment,
    /// Perp klines as daily partitions.
    pub perp_partitions: Vec<Vec<Bar>>,
    /// Spot klines as daily partitions.
    pub spot_partitions: Vec<Vec<Bar>>,
    /// Funding-rate samples.
    pub funding: Vec<(i64, Decimal)>,
    /// Premium-index samples.
    pub premium_index: Vec<(i64, Decimal)>,
    /// `/futures/data/*` metric samples.
    pub futures_metrics: Vec<(i64, Decimal)>,
}

/// Fuse the inputs into a [`FusedCorpus`]: coalesce partitions, apply adjustments, align every
/// canonical series onto the base grid, and derive the spread-to-underlier column.
///
/// # Errors
/// [`qe_domain::DomainError`] if an adjustment produces an invalid bar (e.g. negative price).
pub fn fuse(input: &FusionInput) -> Result<FusedCorpus, qe_domain::DomainError> {
    let grid = Grid::from_window(input.window, input.resolution);
    let slots = grid.slots();

    // Coalesce + adjust the kline series, then reduce each to (open_time, close) points.
    let perp = adjusted_closes(&input.perp_partitions, input.adjustment)?;
    let spot = adjusted_closes(&input.spot_partitions, input.adjustment)?;

    let perp_col = align_onto_grid(&perp, &grid, input.max_gap_ms);
    let funding_col = align_onto_grid(&input.funding, &grid, input.max_gap_ms);
    let premium_col = align_onto_grid(&input.premium_index, &grid, input.max_gap_ms);
    let spot_col = align_onto_grid(&spot, &grid, input.max_gap_ms);
    let metrics_col = align_onto_grid(&input.futures_metrics, &grid, input.max_gap_ms);
    // Spread-to-underlier is derived slot-wise from the aligned perp & spot closes.
    let spread_col = subtract_columns(&perp_col, &spot_col, &slots);

    let columns = vec![
        FusedColumn {
            series: CanonicalSeries::PerpKlines,
            cells: perp_col,
        },
        FusedColumn {
            series: CanonicalSeries::Funding,
            cells: funding_col,
        },
        FusedColumn {
            series: CanonicalSeries::PremiumIndex,
            cells: premium_col,
        },
        FusedColumn {
            series: CanonicalSeries::SpotKlines,
            cells: spot_col,
        },
        FusedColumn {
            series: CanonicalSeries::FuturesMetrics,
            cells: metrics_col,
        },
        FusedColumn {
            series: CanonicalSeries::SpreadToUnderlier,
            cells: spread_col,
        },
    ];

    Ok(FusedCorpus {
        instrument: input.instrument.clone(),
        resolution: input.resolution,
        grid,
        slots,
        columns,
    })
}

/// Coalesce kline partitions, apply the adjustment, and reduce to `(open_time, close)` points.
fn adjusted_closes(
    partitions: &[Vec<Bar>],
    adj: Adjustment,
) -> Result<Vec<(i64, Decimal)>, qe_domain::DomainError> {
    let coalesced = coalesce_bars(partitions);
    let mut out = Vec::with_capacity(coalesced.len());
    for bar in &coalesced {
        let adjusted = adjust_bar(bar, adj)?;
        out.push((adjusted.open_time().millis(), adjusted.close().get()));
    }
    Ok(out)
}

/// Derive a column as `a − b` slot-wise: a [`Cell::Value`] only where **both** inputs have a value
/// at that slot (carried from the slot itself), else a [`Cell::Hole`].
fn subtract_columns(a: &[Cell], b: &[Cell], slots: &[i64]) -> Vec<Cell> {
    a.iter()
        .zip(b.iter())
        .zip(slots.iter())
        .map(|((ca, cb), &slot)| match (ca.value(), cb.value()) {
            (Some(va), Some(vb)) => Cell::Value {
                value: va - vb,
                from_ms: slot,
            },
            _ => Cell::Hole,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty};

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }

    fn instrument() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    fn window(start_ms: i64, end_ms: i64) -> TimeInterval {
        TimeInterval::new(
            Timestamp::from_millis(start_ms),
            Timestamp::from_millis(end_ms),
        )
        .unwrap()
    }

    fn close_bar(t_ms: i64, close: i64) -> Bar {
        let c = Price::new(dec(close)).unwrap();
        Bar::new(
            Timestamp::from_millis(t_ms),
            Resolution::M1,
            c,
            c,
            c,
            c,
            Qty::new(Decimal::ONE).unwrap(),
            1,
        )
        .unwrap()
    }

    #[test]
    fn grid_slots_are_the_expected_fixed_interval() {
        let g = Grid::from_window(window(0, 5 * MIN), Resolution::M1);
        assert_eq!(g.interval_ms, MIN);
        assert_eq!(g.slots(), vec![0, MIN, 2 * MIN, 3 * MIN, 4 * MIN]);
        assert_eq!(g.len(), 5);
        assert!(!g.is_empty());
    }

    #[test]
    fn align_fills_within_bound_and_holes_beyond() {
        // Present at 0 and 5m; max_gap = 2m. Slots 1,2 fill from 0; 3,4 are holes; 5 present.
        let grid = Grid::from_window(window(0, 6 * MIN), Resolution::M1);
        let present = vec![(0, dec(10)), (5 * MIN, dec(20))];
        let cells = align_onto_grid(&present, &grid, 2 * MIN);
        assert_eq!(cells[0].value(), Some(dec(10)));
        assert_eq!(cells[1].value(), Some(dec(10))); // filled from 0
        assert_eq!(cells[2].value(), Some(dec(10))); // filled from 0
        assert_eq!(cells[3], Cell::Hole); // over-bound
        assert_eq!(cells[4], Cell::Hole); // over-bound
        assert_eq!(cells[5].value(), Some(dec(20))); // present
    }

    #[test]
    fn out_of_window_sample_is_ignored_not_placed() {
        // A sample before the window start is out-of-window: dropped, no panic, no slot filled.
        let grid = Grid::from_window(window(2 * MIN, 4 * MIN), Resolution::M1);
        let present = vec![(0, dec(7)), (2 * MIN, dec(9))]; // 0 is before start
        let cells = align_onto_grid(&present, &grid, MIN);
        assert_eq!(cells.len(), 2); // slots 2m, 3m
        assert_eq!(cells[0].value(), Some(dec(9))); // present at 2m
        assert_eq!(cells[1].value(), Some(dec(9))); // filled from 2m within bound
    }

    #[test]
    fn fuse_is_byte_reproducible_for_fixed_inputs() {
        let input = FusionInput {
            instrument: instrument(),
            window: window(0, 4 * MIN),
            resolution: Resolution::M1,
            max_gap_ms: 2 * MIN,
            adjustment: Adjustment::IDENTITY,
            perp_partitions: vec![vec![close_bar(0, 100), close_bar(2 * MIN, 102)]],
            spot_partitions: vec![vec![close_bar(0, 99), close_bar(2 * MIN, 100)]],
            funding: vec![(0, dec(1))],
            premium_index: vec![(0, dec(2))],
            futures_metrics: vec![(0, dec(3))],
        };
        let a = fuse(&input).unwrap().to_json_bytes().unwrap();
        let b = fuse(&input).unwrap().to_json_bytes().unwrap();
        assert_eq!(a, b, "fusion must be byte-reproducible");
    }

    #[test]
    fn fuse_emits_all_canonical_columns_in_order() {
        let input = FusionInput {
            instrument: instrument(),
            window: window(0, 2 * MIN),
            resolution: Resolution::M1,
            max_gap_ms: MIN,
            adjustment: Adjustment::IDENTITY,
            perp_partitions: vec![vec![close_bar(0, 100)]],
            spot_partitions: vec![vec![close_bar(0, 99)]],
            funding: vec![],
            premium_index: vec![],
            futures_metrics: vec![],
        };
        let corpus = fuse(&input).unwrap();
        let got: Vec<CanonicalSeries> = corpus.columns.iter().map(|c| c.series).collect();
        assert_eq!(got, CanonicalSeries::ALL.to_vec());
        assert!(corpus
            .columns
            .iter()
            .all(|c| c.cells.len() == corpus.slots.len()));
    }

    #[test]
    fn spread_is_perp_minus_spot_where_both_present() {
        let input = FusionInput {
            instrument: instrument(),
            window: window(0, 2 * MIN),
            resolution: Resolution::M1,
            max_gap_ms: MIN,
            adjustment: Adjustment::IDENTITY,
            perp_partitions: vec![vec![close_bar(0, 101), close_bar(MIN, 103)]],
            spot_partitions: vec![vec![close_bar(0, 100), close_bar(MIN, 100)]],
            funding: vec![],
            premium_index: vec![],
            futures_metrics: vec![],
        };
        let corpus = fuse(&input).unwrap();
        let spread = corpus.column(CanonicalSeries::SpreadToUnderlier).unwrap();
        assert_eq!(spread.cells[0].value(), Some(dec(1))); // 101 - 100
        assert_eq!(spread.cells[1].value(), Some(dec(3))); // 103 - 100
    }

    #[test]
    fn spread_is_hole_where_spot_missing() {
        // Perp present at both slots; spot only at slot 0 with a tight fill bound → slot 1 spot
        // is a hole, so the spread at slot 1 is a hole too (no leakage).
        let input = FusionInput {
            instrument: instrument(),
            window: window(0, 3 * MIN),
            resolution: Resolution::M1,
            max_gap_ms: 0, // no forward-fill at all
            adjustment: Adjustment::IDENTITY,
            perp_partitions: vec![vec![
                close_bar(0, 101),
                close_bar(MIN, 102),
                close_bar(2 * MIN, 103),
            ]],
            spot_partitions: vec![vec![close_bar(0, 100)]],
            funding: vec![],
            premium_index: vec![],
            futures_metrics: vec![],
        };
        let corpus = fuse(&input).unwrap();
        let spread = corpus.column(CanonicalSeries::SpreadToUnderlier).unwrap();
        assert_eq!(spread.cells[0].value(), Some(dec(1)));
        assert_eq!(spread.cells[1], Cell::Hole);
        assert_eq!(spread.cells[2], Cell::Hole);
    }
}
