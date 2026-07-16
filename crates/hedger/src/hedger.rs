//! Hedge Planner (QE-214) — emits **absolute** target positions from netted targets, stateless wrt the
//! current venue position.
//!
//! QE-213's [`NetTarget`] is a fraction of allowed capital; the planner scales it by equity into an absolute
//! [`TargetPosition`] (a signed [`Notional`]; sign = direction, `0` = flat). Because it emits an **absolute**
//! target — not a delta from the current position — it never reads the venue position: the delta
//! `target − current` is QE-217's job. That omission *is* the statelessness the spec calls out (the
//! architectural benefit of target-based hedging).
//!
//! Equity and available margin come from a [`PositionKeeper`] seam (the real keeper is QE-217); the planner's
//! [`capital_view`](HedgePlanner::capital_view) matches keeper truth by construction. Available margin is
//! surfaced for the cockpit and QE-215 pre-trade caps but does not clamp the target here (sizing caps are
//! QE-215).

// Order-emission path (QE-268): reject `unwrap`/`expect`/`panic` — a panic here is a live-trading fault.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use qe_domain::Notional;

use crate::live_netter::NetTarget;
// QE-426: the equity/margin + venue-position seam and the absolute target now live in the shared
// `qe-runtime-core` contract, so the planner⑤ and the edge⑥ meet through it without depending on each other.
use qe_runtime_core::{CapitalView, PositionKeeper, TargetPosition};

/// Emits absolute target positions from netted targets, statelessly wrt the current venue position.
pub struct HedgePlanner<K> {
    keeper: K,
}

impl<K: PositionKeeper> HedgePlanner<K> {
    /// A planner over `keeper` (the source of equity/margin truth).
    pub fn new(keeper: K) -> Self {
        Self { keeper }
    }

    /// The equity + available-margin view — sourced from, and equal to, the keeper's truth (tracks it as the
    /// keeper moves).
    #[must_use]
    pub fn capital_view(&self) -> CapitalView {
        self.keeper.capital()
    }

    /// Emit the **absolute** target position for `net`: `net.net × equity` (equity read fresh from the
    /// keeper, so the target tracks equity). **Stateless:** it never reads the keeper's `venue_position`, so
    /// the same `net` + equity yields the same target regardless of what the venue currently holds — the
    /// delta from the current position is computed downstream (QE-217).
    #[must_use]
    pub fn plan(&self, net: NetTarget) -> TargetPosition {
        let equity = self.keeper.equity().get();
        TargetPosition {
            notional: Notional::new(net.net * equity),
            // Gross exposure scales the same fraction-of-capital by equity as the net, but from `long + short`
            // — so a hedged book (small net, large sides) carries its true two-sided exposure to the governor.
            gross: Notional::new(net.gross() * equity),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::Direction;
    use rust_decimal::Decimal;
    use std::cell::Cell;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn n(s: &str) -> Notional {
        Notional::new(dec(s))
    }

    /// A fake keeper with mutable capital + venue position, for exercising the seam deterministically.
    struct FakeKeeper {
        equity: Cell<Decimal>,
        margin: Cell<Decimal>,
        venue: Cell<Decimal>,
    }
    impl FakeKeeper {
        fn new(equity: &str, margin: &str, venue: &str) -> Self {
            Self {
                equity: Cell::new(dec(equity)),
                margin: Cell::new(dec(margin)),
                venue: Cell::new(dec(venue)),
            }
        }
    }
    impl PositionKeeper for FakeKeeper {
        fn capital(&self) -> CapitalView {
            CapitalView {
                equity: Notional::new(self.equity.get()),
                available_margin: Notional::new(self.margin.get()),
            }
        }
        fn venue_position(&self) -> Notional {
            Notional::new(self.venue.get())
        }
    }

    fn net_target(net: &str, long: &str, short: &str) -> NetTarget {
        NetTarget {
            net: dec(net),
            long: dec(long),
            short: dec(short),
        }
    }

    /// AC #1: the planner emits identical targets regardless of the current venue position.
    #[test]
    fn plan_is_stateless_wrt_current_venue_position() {
        let keeper = FakeKeeper::new("10000", "5000", "0");
        let planner = HedgePlanner::new(keeper);
        let net = net_target("0.009", "0.015", "0.006");

        let flat_target = planner.plan(net);

        // Change only the current venue position (equity fixed): the target must not move.
        for venue in ["7500", "-9999", "10000", "0"] {
            planner.keeper.venue.set(dec(venue));
            assert_eq!(
                planner.keeper.venue_position(),
                Notional::new(dec(venue)),
                "keeper reports the changed venue position"
            );
            assert_eq!(
                planner.plan(net),
                flat_target,
                "target is stateless wrt current venue position {venue}"
            );
        }
    }

    /// AC #2: the equity/margin view matches keeper truth, and tracks a keeper change.
    #[test]
    fn capital_view_matches_keeper_truth() {
        let keeper = FakeKeeper::new("10000", "5000", "3000");
        let planner = HedgePlanner::new(keeper);

        assert_eq!(planner.capital_view().equity, n("10000"));
        assert_eq!(planner.capital_view().available_margin, n("5000"));

        // Keeper moves → the view tracks it.
        planner.keeper.equity.set(dec("12345"));
        planner.keeper.margin.set(dec("6789"));
        assert_eq!(planner.capital_view().equity, n("12345"));
        assert_eq!(planner.capital_view().available_margin, n("6789"));
    }

    /// The absolute target is `net.net × equity`, read fresh so it tracks equity.
    #[test]
    fn plan_scales_net_fraction_by_equity() {
        let keeper = FakeKeeper::new("10000", "5000", "0");
        let planner = HedgePlanner::new(keeper);
        let net = net_target("0.009", "0.015", "0.006");

        assert_eq!(planner.plan(net).notional, n("90")); // 0.009 * 10_000
                                                         // Gross scales `long + short` (0.015 + 0.006) by equity, independent of the net.
        assert_eq!(planner.plan(net).gross, n("210")); // 0.021 * 10_000

        // Doubling equity doubles the target (equity is read fresh each plan).
        planner.keeper.equity.set(dec("20000"));
        assert_eq!(planner.plan(net).notional, n("180"));
        assert_eq!(planner.plan(net).gross, n("420"));
    }

    /// A hedged book (equal-and-opposite legs) nets small but carries true gross: `gross > |net|`.
    #[test]
    fn plan_carries_gross_above_net_for_a_hedged_book() {
        let keeper = FakeKeeper::new("10000", "5000", "0");
        let planner = HedgePlanner::new(keeper);
        // net 0, gross 0.04 (long 0.02 + short 0.02).
        let hedged = net_target("0", "0.02", "0.02");
        let target = planner.plan(hedged);
        assert_eq!(target.notional, Notional::ZERO);
        assert_eq!(target.gross, n("400")); // 0.04 * 10_000
        assert!(target.gross.get() > target.notional.get().abs());
    }

    /// The target's sign encodes direction; a zero net is flat.
    #[test]
    fn plan_sign_encodes_direction() {
        let keeper = FakeKeeper::new("10000", "5000", "0");
        let planner = HedgePlanner::new(keeper);

        let long = planner.plan(net_target("0.02", "0.02", "0"));
        assert!(long.notional.get() > Decimal::ZERO);
        assert_eq!(long.direction(), Some(Direction::Long));

        let short = planner.plan(net_target("-0.03", "0", "0.03"));
        assert!(short.notional.get() < Decimal::ZERO);
        assert_eq!(short.direction(), Some(Direction::Short));

        let flat = planner.plan(net_target("0", "0", "0"));
        assert_eq!(flat.notional, Notional::ZERO);
        assert_eq!(flat.direction(), None);
    }
}
