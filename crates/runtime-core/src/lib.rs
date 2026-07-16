//! qe-runtime-core — the shared runtime **contract** the Hedge Planner (⑤) produces and the Edge gateway
//! (⑥) consumes (QE-426).
//!
//! Split out of `qe-runtime` so the gRPC seam (QE-218) between the planner and the adapter is a **crate**
//! boundary, not a boundary inside one crate: `qe-hedger` and `qe-edge` each depend on this contract but not
//! on each other, exactly as two colocated processes over the wire would. This crate carries only the wire /
//! seam types — no live logic, no venue, no order submission.
//!
//! - [`TargetPosition`] — the **absolute** signed net target (QE-214) the planner emits and the adapter
//!   translates to a venue delta.
//! - [`CapitalView`] / [`PositionKeeper`] — the equity/margin + venue-position seam the planner reads and the
//!   real [`VenueKeeper`](../qe_edge/edge/struct.VenueKeeper.html) implements.

// Order-emission path (QE-268): reject `unwrap`/`expect`/`panic` — a panic on the contract that carries the
// live target is a live-trading fault. Carried from the `hedger.rs` order-path module this was extracted from.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use qe_domain::{Direction, Notional};

/// An independent equity + available-margin view (capital allocation), sourced from the position keeper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapitalView {
    /// Account equity (allowed capital the target scales against).
    pub equity: Notional,
    /// Available margin / buying power (surfaced for cockpit + QE-215 caps).
    pub available_margin: Notional,
}

/// The keeper truth the planner reads: capital, plus the current venue position.
///
/// The planner reads [`capital`](PositionKeeper::capital) for its equity/margin view;
/// [`venue_position`](PositionKeeper::venue_position) is keeper truth for QE-217's delta translation and is
/// **deliberately not** consulted when planning — that omission is what makes the planner stateless.
pub trait PositionKeeper {
    /// The current equity + available-margin view.
    fn capital(&self) -> CapitalView;
    /// The current signed venue position (notional). Not used for planning.
    fn venue_position(&self) -> Notional;
    /// Just the equity (allowed capital the target scales against). Defaults to `capital().equity`; a real
    /// keeper whose `capital()` is expensive can override this to avoid computing the margin the planner
    /// discards.
    fn equity(&self) -> Notional {
        self.capital().equity
    }
}

/// Forward the keeper seam through a shared reference, so a `HedgePlanner` can *borrow* a keeper (read its
/// equity/position for planning) while the keeper is still mutated by the user-data feed between plans.
impl<K: PositionKeeper + ?Sized> PositionKeeper for &K {
    fn capital(&self) -> CapitalView {
        (**self).capital()
    }
    fn venue_position(&self) -> Notional {
        (**self).venue_position()
    }
    fn equity(&self) -> Notional {
        (**self).equity()
    }
}

/// An absolute target position for the instrument: a signed net [`Notional`] (sign = direction, `0` = flat)
/// plus the unsigned **gross** exposure (`long + short`) the QE-215 governor caps.
///
/// `notional` is the net; `gross` is the total two-sided exposure, always `≥ |notional|`. For a single
/// instrument the two sides never oppose, so `gross == |notional|`; once a hedged/offsetting book exists (net
/// small, both sides large) `gross` exceeds `|notional|` and the gross cap must be checked against `gross`, not
/// the net (QE-418).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetPosition {
    /// The signed absolute **net** target notional (sign = direction, `0` = flat).
    pub notional: Notional,
    /// The unsigned **gross** exposure notional (`long + short`), always `≥ |notional|`.
    pub gross: Notional,
}

impl TargetPosition {
    /// A **single-instrument** target: gross equals `|notional|` by construction (the two sides never oppose).
    /// Use this for every path that does not model a hedged/offsetting book; the multi-instrument case sets
    /// `gross` explicitly.
    #[must_use]
    pub fn single(notional: Notional) -> Self {
        Self {
            notional,
            gross: Notional::new(notional.get().abs()),
        }
    }

    /// The target's direction: `Long` if positive, `Short` if negative, `None` if flat.
    #[must_use]
    pub fn direction(&self) -> Option<Direction> {
        match self.notional.cmp(&Notional::ZERO) {
            std::cmp::Ordering::Greater => Some(Direction::Long),
            std::cmp::Ordering::Less => Some(Direction::Short),
            std::cmp::Ordering::Equal => None,
        }
    }
}
