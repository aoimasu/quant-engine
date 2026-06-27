//! Leakage-safe forward-fill **policy**.
//!
//! Forward-filling across a long outage fabricates a flat price/edge that never existed — leakage.
//! [`plan_fill`] fills a missing slot from the most recent present sample **only while** the run of
//! consecutive misses stays within `max_gap_ms`; once a gap exceeds the bound, the remaining slots
//! are **holes**, never filled. It is value-agnostic — it returns *which* slots fill (and from which
//! source timestamp) vs which stay holes — so the fuser carries real values without leaking across a
//! large gap.

use serde::Serialize;

use crate::integrity::Gap;

/// A missing slot that is filled forward from an earlier present sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FilledPoint {
    /// The (previously absent) slot timestamp being filled.
    pub slot_ms: i64,
    /// The present timestamp whose value is carried forward into the slot.
    pub from_ms: i64,
}

/// The outcome of applying the fill policy over a grid.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FillPlan {
    /// Slots filled forward (within the bound).
    pub filled: Vec<FilledPoint>,
    /// Gaps left unfilled because they exceed `max_gap_ms` — leakage-safe holes.
    pub holes: Vec<Gap>,
}

/// Plan a leakage-safe forward-fill over the expected grid `start, start+interval, … < end`.
///
/// `present` is the set of timestamps that actually have data. A missing slot is filled from the
/// last present sample while the elapsed time since that sample is `<= max_gap_ms`; slots beyond that
/// bound are emitted as a [`Gap`] hole (no fill across a gap larger than the bound — AC #1).
///
/// A leading run of missing slots before the first present sample cannot be filled (there is nothing
/// to carry forward); it is reported as a hole.
#[must_use]
pub fn plan_fill(
    present: &[i64],
    interval_ms: i64,
    start_ms: i64,
    end_ms: i64,
    max_gap_ms: i64,
) -> FillPlan {
    let mut plan = FillPlan::default();
    if interval_ms <= 0 {
        return plan;
    }
    let present: std::collections::BTreeSet<i64> = present.iter().copied().collect();

    let mut last_present: Option<i64> = None;
    // Track the start of the current unfilled run (for hole reporting).
    let mut hole_start: Option<i64> = None;

    let mut slot = start_ms;
    while slot < end_ms {
        if present.contains(&slot) {
            // Close any open hole at the present sample.
            if let Some(h0) = hole_start.take() {
                plan.holes.push(make_hole(h0, slot, interval_ms));
            }
            last_present = Some(slot);
        } else {
            match last_present {
                // Within the bound → fill forward from the last present sample.
                Some(src) if slot - src <= max_gap_ms => {
                    plan.filled.push(FilledPoint {
                        slot_ms: slot,
                        from_ms: src,
                    });
                }
                // Beyond the bound, or nothing to carry forward → the start of a hole (the first
                // unfilled slot itself, so the hole covers exactly the unfilled region).
                _ => {
                    hole_start.get_or_insert(slot);
                }
            }
        }
        slot += interval_ms;
    }
    if let Some(h0) = hole_start.take() {
        plan.holes.push(make_hole(h0, end_ms, interval_ms));
    }
    plan
}

/// Build a hole spanning `[from_ms, to_ms)` where `from_ms` is the first **unfilled** slot, so the
/// number of holed slots is `(to - from) / interval` (no `-1` — `from` is itself missing).
fn make_hole(from_ms: i64, to_ms: i64, interval_ms: i64) -> Gap {
    let missing = ((to_ms - from_ms) / interval_ms).max(0) as u64;
    Gap {
        from_ms,
        to_ms,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn small_gap_fills_forward() {
        // Present at 0 and 3min; gap of 2 missing slots (1,2). max_gap = 2min covers it.
        let plan = plan_fill(&[0, 3 * MIN], MIN, 0, 4 * MIN, 2 * MIN);
        assert!(plan.holes.is_empty());
        let slots: Vec<i64> = plan.filled.iter().map(|f| f.slot_ms).collect();
        assert_eq!(slots, vec![MIN, 2 * MIN]);
        // All filled from the last present sample at t=0.
        assert!(plan.filled.iter().all(|f| f.from_ms == 0));
    }

    #[test]
    fn gap_larger_than_bound_is_not_filled() {
        // AC #1: present at 0 and 5min; max_gap = 2min. Slots 1,2 are within bound (filled);
        // slots 3,4 are beyond the bound → a hole, NOT filled across.
        let plan = plan_fill(&[0, 5 * MIN], MIN, 0, 6 * MIN, 2 * MIN);
        let filled: Vec<i64> = plan.filled.iter().map(|f| f.slot_ms).collect();
        assert_eq!(filled, vec![MIN, 2 * MIN], "only within-bound slots fill");
        assert_eq!(plan.holes.len(), 1);
        // No filled slot lies in the over-bound region [3min, 5min).
        assert!(plan.filled.iter().all(|f| f.slot_ms < 3 * MIN));
    }

    #[test]
    fn gap_exactly_at_bound_fills() {
        // Present at 0 and 3min; max_gap = 2min. Slot 1 (Δ1m) and slot 2 (Δ2m == bound) both fill.
        let plan = plan_fill(&[0, 3 * MIN], MIN, 0, 4 * MIN, 2 * MIN);
        assert!(plan.holes.is_empty());
        assert_eq!(plan.filled.len(), 2);
    }

    #[test]
    fn leading_missing_run_is_a_hole_not_filled() {
        // First present sample is at 2min; slots 0,1 have nothing to carry forward → hole.
        let plan = plan_fill(&[2 * MIN, 3 * MIN], MIN, 0, 4 * MIN, 10 * MIN);
        assert!(plan.filled.is_empty());
        assert_eq!(plan.holes.len(), 1);
        assert_eq!(plan.holes[0].from_ms, 0);
    }

    #[test]
    fn fully_present_series_needs_no_fill() {
        let present: Vec<i64> = (0..4).map(|i| i * MIN).collect();
        let plan = plan_fill(&present, MIN, 0, 4 * MIN, MIN);
        assert!(plan.filled.is_empty() && plan.holes.is_empty());
    }
}
