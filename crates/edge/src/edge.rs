//! Venue adapter / Position keeper / order lifecycle + simulator (QE-217) — the edge gateway.
//!
//! Three pieces that turn an **absolute** target (QE-214) into venue action and keep authoritative account
//! state:
//! - [`plan_delta`] — the stateless→stateful bridge: `target − kept position`, expressed as a venue-native
//!   [`OrderIntent`]. This is the *only* place the current position enters, exactly as QE-214's statelessness
//!   split intended.
//! - [`VenueKeeper`] — the position keeper. It absorbs the QE-204 [`UserDataEvent`] feed (fills, position
//!   reports, snapshots) as **ground truth** and **never infers** position from its own orders. It `impl`s the
//!   QE-214 [`PositionKeeper`](qe_runtime_core::PositionKeeper) seam, so the Hedge Planner runs over the real
//!   keeper.
//! - [`VenueSimulator`] — an in-memory venue for paper/sim mode: it accepts an [`OrderIntent`], drives an
//!   [`Order`] through its lifecycle with an immediate fill, and emits the [`Fill`] event the keeper absorbs —
//!   the full loop with **no real orders**.

// Order-emission path (QE-268): reject `unwrap`/`expect`/`panic` — a panic here is a live-trading fault.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rust_decimal::Decimal;

use qe_domain::{Direction, InstrumentId, Notional, Price, Qty, Side};
use qe_venue::userdata::{Fill, PositionReport, UserDataEvent};

// QE-426: the keeper seam + capital view live in the shared `qe-runtime-core` contract, so the edge⑥
// implements the planner's seam without depending on `qe-hedger`.
use qe_runtime_core::CapitalView;

/// The lifecycle state of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Created, not yet sent.
    New,
    /// Sent to the venue, awaiting fills.
    Submitted,
    /// Some quantity filled, more outstanding.
    PartiallyFilled,
    /// Fully filled.
    Filled,
    /// Rejected by the venue.
    Rejected,
    /// Cancelled.
    Cancelled,
}

/// A venue order and its running fill state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// Venue order id.
    pub id: u64,
    /// Buy or sell.
    pub side: Side,
    /// Ordered quantity.
    pub qty: Qty,
    /// Cumulative filled quantity.
    pub filled: Qty,
    /// Lifecycle state.
    pub state: OrderState,
}

impl Order {
    /// A new (unsent) order.
    #[must_use]
    pub fn new(id: u64, side: Side, qty: Qty) -> Self {
        Self {
            id,
            side,
            qty,
            filled: Qty::ZERO,
            state: OrderState::New,
        }
    }

    /// Whether the order has reached a terminal state (no further transitions).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            OrderState::Filled | OrderState::Rejected | OrderState::Cancelled
        )
    }

    /// Mark the order submitted (`New → Submitted`).
    pub fn submit(&mut self) {
        if self.state == OrderState::New {
            self.state = OrderState::Submitted;
        }
    }

    /// Absorb a fill of `q`, advancing to `PartiallyFilled` or `Filled`. A no-op on a terminal order.
    pub fn on_fill(&mut self, q: Qty) {
        if self.is_terminal() {
            return;
        }
        // Sum of two non-negative quantities is non-negative — a total `Qty + Qty`, no re-validation.
        self.filled = self.filled + q;
        self.state = if self.filled.get() >= self.qty.get() {
            OrderState::Filled
        } else {
            OrderState::PartiallyFilled
        };
    }

    /// Mark the order rejected by the venue. A no-op on a terminal order.
    pub fn reject(&mut self) {
        if !self.is_terminal() {
            self.state = OrderState::Rejected;
        }
    }

    /// Mark the order cancelled. A no-op on a terminal order.
    pub fn cancel(&mut self) {
        if !self.is_terminal() {
            self.state = OrderState::Cancelled;
        }
    }
}

/// A venue-native order to move the position by a delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderIntent {
    /// Buy (increase) or sell (decrease).
    pub side: Side,
    /// The delta quantity (always positive).
    pub qty: Qty,
}

/// Translate an absolute `target` notional into a venue-native order **delta** against the `current_qty`
/// (signed, contracts) kept position, at `mark`. Returns `None` when already at target (`delta == 0`) or when
/// `mark` is zero (cannot translate pre-mark). This is the only place the current position enters the flow.
#[must_use]
pub fn plan_delta(target: Notional, current_qty: Decimal, mark: Price) -> Option<OrderIntent> {
    let m = mark.get();
    if m.is_zero() {
        return None;
    }
    let target_qty = target.get() / m;
    let delta = target_qty - current_qty;
    if delta.is_zero() {
        return None;
    }
    let side = if delta.is_sign_negative() {
        Side::Sell
    } else {
        Side::Buy
    };
    Some(OrderIntent {
        side,
        // The order magnitude is `|delta|` — a total, always-non-negative `Qty`.
        qty: Qty::abs_of(delta),
    })
}

/// The signed quantity a position report describes (`+` long, `−` short, `0` flat).
fn signed_from_report(report: &PositionReport) -> Decimal {
    match report.direction {
        Some(Direction::Long) => report.qty.get(),
        Some(Direction::Short) => -report.qty.get(),
        None => Decimal::ZERO,
    }
}

/// The position keeper: authoritative per-instrument position + account view, fed by the venue's user-data
/// stream. It never infers position from its own orders — only from venue fills / reports / snapshots.
pub struct VenueKeeper {
    instrument: InstrumentId,
    /// Signed position quantity (contracts): `+` long, `−` short.
    signed_qty: Decimal,
    /// Last observed mark price (for notional / equity).
    mark: Price,
    /// Account equity (venue truth / sim ledger).
    equity: Notional,
    /// Available margin (venue truth / sim ledger).
    available_margin: Notional,
}

impl VenueKeeper {
    /// A flat keeper for `instrument` with an initial equity (and equal available margin).
    #[must_use]
    pub fn new(instrument: InstrumentId, initial_equity: Notional) -> Self {
        Self {
            instrument,
            signed_qty: Decimal::ZERO,
            mark: Price::ZERO,
            equity: initial_equity,
            available_margin: initial_equity,
        }
    }

    /// Absorb one venue user-data event as ground truth. Fills adjust the position; position reports and
    /// snapshots **set** it authoritatively (overriding any fill-derived value). Events for other instruments
    /// and non-position events (heartbeat / listen-key-expired) are ignored.
    pub fn apply(&mut self, event: &UserDataEvent) {
        match event {
            UserDataEvent::Fill(f) if f.instrument == self.instrument => match f.side {
                Side::Buy => self.signed_qty += f.qty.get(),
                Side::Sell => self.signed_qty -= f.qty.get(),
            },
            UserDataEvent::Position(r) if r.instrument == self.instrument => {
                self.signed_qty = signed_from_report(r);
            }
            UserDataEvent::Snapshot(s) => {
                self.signed_qty = s
                    .positions
                    .iter()
                    .find(|r| r.instrument == self.instrument)
                    .map_or(Decimal::ZERO, signed_from_report);
            }
            _ => {}
        }
    }

    /// Feed the latest mark price (venue truth).
    pub fn observe_mark(&mut self, mark: Price) {
        self.mark = mark;
    }

    /// Feed the latest account balances (venue truth / sim ledger).
    pub fn observe_balance(&mut self, equity: Notional, available_margin: Notional) {
        self.equity = equity;
        self.available_margin = available_margin;
    }

    /// The signed kept position quantity (contracts).
    #[must_use]
    pub fn signed_qty(&self) -> Decimal {
        self.signed_qty
    }

    /// The last observed mark price.
    #[must_use]
    pub fn mark(&self) -> Price {
        self.mark
    }

    /// The kept position as a signed notional (`signed_qty × mark`).
    #[must_use]
    pub fn position_notional(&self) -> Notional {
        Notional::new(self.signed_qty * self.mark.get())
    }
}

impl qe_runtime_core::PositionKeeper for VenueKeeper {
    fn capital(&self) -> CapitalView {
        CapitalView {
            equity: self.equity,
            available_margin: self.available_margin,
        }
    }
    fn venue_position(&self) -> Notional {
        self.position_notional()
    }
    // `equity()` intentionally not overridden — it stays `== capital().equity` (QE-214 forward obligation).
}

// QE-426: the `PositionKeeper for &K` blanket impl now lives in `qe-runtime-core` alongside the trait (the
// orphan rule requires it there), so a `HedgePlanner` can still *borrow* a keeper across plans.

/// The result of submitting an order to the simulator: the resolved order + the fill event to feed the keeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimFill {
    /// The order, driven to `Filled`.
    pub order: Order,
    /// The `Fill` user-data event the keeper absorbs (as if from the venue).
    pub event: UserDataEvent,
}

/// An in-memory venue for paper/sim mode. Accepts order intents, fills them immediately, and emits the
/// user-data events a real venue would — so the full loop runs with **no real orders**.
pub struct VenueSimulator {
    instrument: InstrumentId,
    next_order_id: u64,
    next_trade_id: u64,
    /// The simulator's own signed position (contracts).
    signed_qty: Decimal,
    /// Last fill price (used as the reported entry price).
    last_price: Price,
    /// Count of orders submitted (for "no real orders" accounting).
    submitted: u64,
}

impl VenueSimulator {
    /// A fresh simulator for `instrument`.
    #[must_use]
    pub fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            next_order_id: 1,
            next_trade_id: 1,
            signed_qty: Decimal::ZERO,
            last_price: Price::ZERO,
            submitted: 0,
        }
    }

    /// Submit `intent`, filling it immediately at `fill_price`. Advances the sim position and returns the
    /// resolved [`Order`] plus the [`Fill`] event to feed the keeper.
    pub fn submit(
        &mut self,
        intent: OrderIntent,
        fill_price: Price,
        event_time_ms: i64,
    ) -> SimFill {
        let order_id = self.next_order_id;
        self.next_order_id += 1;
        let trade_id = self.next_trade_id;
        self.next_trade_id += 1;
        self.submitted += 1;
        self.last_price = fill_price;

        let mut order = Order::new(order_id, intent.side, intent.qty);
        order.submit();
        order.on_fill(intent.qty); // immediate full fill

        match intent.side {
            Side::Buy => self.signed_qty += intent.qty.get(),
            Side::Sell => self.signed_qty -= intent.qty.get(),
        }

        let event = UserDataEvent::Fill(Fill {
            instrument: self.instrument.clone(),
            side: intent.side,
            price: fill_price,
            qty: intent.qty,
            order_id,
            trade_id,
            event_time_ms,
        });
        SimFill { order, event }
    }

    /// The simulator's authoritative position report (for reconciliation).
    #[must_use]
    pub fn position_report(&self, event_time_ms: i64) -> PositionReport {
        let (direction, qty) = if self.signed_qty.is_zero() {
            (None, Qty::ZERO)
        } else if self.signed_qty.is_sign_negative() {
            (Some(Direction::Short), Qty::abs_of(self.signed_qty))
        } else {
            (Some(Direction::Long), Qty::abs_of(self.signed_qty))
        };
        PositionReport {
            instrument: self.instrument.clone(),
            direction,
            qty,
            entry_price: self.last_price,
            event_time_ms,
        }
    }

    /// The simulator's signed position (contracts).
    #[must_use]
    pub fn signed_qty(&self) -> Decimal {
        self.signed_qty
    }

    /// How many orders have been submitted.
    #[must_use]
    pub fn orders_submitted(&self) -> u64 {
        self.submitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_hedger::live_netter::NetTarget;
    use qe_hedger::HedgePlanner;
    use qe_runtime_core::PositionKeeper;
    use qe_venue::userdata::PositionSnapshot;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn price(s: &str) -> Price {
        Price::new(dec(s)).unwrap()
    }
    fn qty(s: &str) -> Qty {
        Qty::new(dec(s)).unwrap()
    }
    fn instrument() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    /// AC #1: an absolute target becomes the correct venue delta vs the kept position.
    #[test]
    fn target_becomes_correct_delta_vs_kept_position() {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));
        keeper.observe_mark(price("50000"));
        let mut sim = VenueSimulator::new(instrument());

        // Flat → target +10_000 notional: buy 10_000/50_000 = 0.2.
        let intent = plan_delta(
            Notional::new(dec("10000")),
            keeper.signed_qty(),
            keeper.mark(),
        )
        .unwrap();
        assert_eq!(intent.side, Side::Buy);
        assert_eq!(intent.qty, qty("0.2"));

        let fill = sim.submit(intent, price("50000"), 1);
        keeper.apply(&fill.event);
        assert_eq!(keeper.signed_qty(), dec("0.2"));
        assert_eq!(keeper.position_notional(), Notional::new(dec("10000")));

        // Reduce the target to +5_000: sell 0.2 − 0.1 = 0.1.
        let reduce = plan_delta(
            Notional::new(dec("5000")),
            keeper.signed_qty(),
            keeper.mark(),
        )
        .unwrap();
        assert_eq!(reduce.side, Side::Sell);
        assert_eq!(reduce.qty, qty("0.1"));

        // Already at target → no order.
        assert!(plan_delta(
            keeper.position_notional(),
            keeper.signed_qty(),
            keeper.mark()
        )
        .is_none());

        // A net-short target from flat crosses to a Sell.
        let short = plan_delta(Notional::new(dec("-15000")), dec("0"), keeper.mark()).unwrap();
        assert_eq!(short.side, Side::Sell);
        assert_eq!(short.qty, qty("0.3"));
    }

    /// AC #2: the keeper tracks venue reports authoritatively — a position report overrides fills ("never
    /// infers"), a snapshot re-sets, and other instruments are ignored.
    #[test]
    fn keeper_tracks_venue_reports_authoritatively() {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));

        // A fill moves the position.
        keeper.apply(&UserDataEvent::Fill(Fill {
            instrument: instrument(),
            side: Side::Buy,
            price: price("50000"),
            qty: qty("0.5"),
            order_id: 1,
            trade_id: 1,
            event_time_ms: 1,
        }));
        assert_eq!(keeper.signed_qty(), dec("0.5"));

        // The venue then REPORTS a different position — the report is ground truth, overriding the fill.
        keeper.apply(&UserDataEvent::Position(PositionReport {
            instrument: instrument(),
            direction: Some(Direction::Short),
            qty: qty("0.3"),
            entry_price: price("49000"),
            event_time_ms: 2,
        }));
        assert_eq!(
            keeper.signed_qty(),
            dec("-0.3"),
            "the venue report wins over the fill"
        );

        // An event for a different instrument is ignored.
        keeper.apply(&UserDataEvent::Fill(Fill {
            instrument: InstrumentId::new("ETHUSDT").unwrap(),
            side: Side::Buy,
            price: price("3000"),
            qty: qty("10"),
            order_id: 2,
            trade_id: 2,
            event_time_ms: 3,
        }));
        assert_eq!(
            keeper.signed_qty(),
            dec("-0.3"),
            "other instruments do not affect this keeper"
        );

        // A snapshot re-sets the position; absence from the snapshot means flat.
        keeper.apply(&UserDataEvent::Snapshot(PositionSnapshot {
            positions: vec![PositionReport {
                instrument: instrument(),
                direction: Some(Direction::Long),
                qty: qty("1.0"),
                entry_price: price("50000"),
                event_time_ms: 4,
            }],
            event_time_ms: 4,
        }));
        assert_eq!(keeper.signed_qty(), dec("1.0"));

        keeper.apply(&UserDataEvent::Snapshot(PositionSnapshot {
            positions: vec![],
            event_time_ms: 5,
        }));
        assert_eq!(
            keeper.signed_qty(),
            dec("0"),
            "absence from a snapshot means flat"
        );
    }

    /// AC #3: the full loop (target → delta → sim fill → keeper) converges using only the simulator.
    #[test]
    fn sim_runs_full_loop_with_no_real_orders() {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));
        keeper.observe_mark(price("50000"));
        let mut sim = VenueSimulator::new(instrument());

        for target in ["20000", "8000", "-6000"] {
            let want = Notional::new(dec(target));
            if let Some(intent) = plan_delta(want, keeper.signed_qty(), keeper.mark()) {
                let fill = sim.submit(intent, price("50000"), 1);
                assert_eq!(fill.order.state, OrderState::Filled);
                keeper.apply(&fill.event);
            }
            // The kept position now equals the target (to the mark).
            assert_eq!(
                keeper.position_notional(),
                want,
                "loop converges to target {target}"
            );
        }
        // The keeper and the simulator agree, and it was all in-memory.
        assert_eq!(keeper.signed_qty(), sim.signed_qty());
        assert_eq!(sim.orders_submitted(), 3);
    }

    /// End-to-end: a `HedgePlanner` over the real `VenueKeeper` plans an absolute target that `plan_delta`
    /// turns into a sim-filled delta — the QE-213→217 stack, proving the keeper satisfies the QE-214 seam.
    #[test]
    fn hedge_planner_over_venue_keeper_end_to_end() {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));
        keeper.observe_mark(price("50000"));
        keeper.observe_balance(Notional::new(dec("100000")), Notional::new(dec("100000")));

        // The keeper is a valid PositionKeeper: the planner reads its equity + venue position.
        assert_eq!(
            PositionKeeper::venue_position(&keeper),
            Notional::new(dec("0"))
        );

        let net = NetTarget {
            net: dec("0.05"),
            long: dec("0.05"),
            short: dec("0"),
        };
        let target = {
            let planner = HedgePlanner::new(&keeper);
            planner.plan(net) // 0.05 × 100_000 = 5_000
        };
        assert_eq!(target.notional, Notional::new(dec("5000")));

        let mut sim = VenueSimulator::new(instrument());
        let intent = plan_delta(target.notional, keeper.signed_qty(), keeper.mark()).unwrap();
        let fill = sim.submit(intent, price("50000"), 1);
        keeper.apply(&fill.event);
        assert_eq!(keeper.position_notional(), Notional::new(dec("5000")));
    }

    /// The order lifecycle transitions correctly, including a partial then a completing fill.
    #[test]
    fn order_lifecycle_transitions() {
        let mut order = Order::new(1, Side::Buy, qty("1.0"));
        assert_eq!(order.state, OrderState::New);
        order.submit();
        assert_eq!(order.state, OrderState::Submitted);
        order.on_fill(qty("0.4"));
        assert_eq!(order.state, OrderState::PartiallyFilled);
        assert_eq!(order.filled, qty("0.4"));
        order.on_fill(qty("0.6"));
        assert_eq!(order.state, OrderState::Filled);

        // A terminal (Filled) order ignores further transitions.
        assert!(order.is_terminal());
        order.on_fill(qty("0.5"));
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(
            order.filled,
            qty("1.0"),
            "a filled order does not over-fill"
        );
        order.reject();
        assert_eq!(
            order.state,
            OrderState::Filled,
            "a filled order cannot be rejected"
        );

        let mut rejected = Order::new(2, Side::Sell, qty("1.0"));
        rejected.reject();
        assert_eq!(rejected.state, OrderState::Rejected);
        // A rejected order is terminal: cancel/fill are no-ops.
        rejected.cancel();
        rejected.on_fill(qty("1.0"));
        assert_eq!(rejected.state, OrderState::Rejected);

        let mut cancelled = Order::new(3, Side::Sell, qty("1.0"));
        cancelled.cancel();
        assert_eq!(cancelled.state, OrderState::Cancelled);
    }
}
