//! Out-of-band kill-switch at the venue adapter (QE-216) — the QE-009 kill contract, enforced where orders
//! actually leave for the venue.
//!
//! [`VenueKillGate`] wraps the QE-217 [`VenueSimulator`] with a latching [`KillHandle`]. Once the switch is
//! tripped — from **anywhere**, via a clone of the handle (a watchdog, the clock-skew guard, a manual
//! control), with no dependency on the cockpit or the Hedge Planner — the gate:
//! - **halts submission**: [`submit`](VenueKillGate::submit) returns `Err(KillHalt)` and sends nothing, and
//!   the QE-009 [`OrderGate::admit`] structurally returns [`Admission::FlattenAndHalt`]; and
//! - **flattens the position**: [`enforce_kill`](VenueKillGate::enforce_kill) submits the closing order
//!   (computed from the kept position alone — no planner target needed) once, then stays halted.
//!
//! That is the reviewer's requirement: a deterministic, out-of-band halt at the order-submission layer that
//! works even with the cockpit/planner down.

use rust_decimal::Decimal;

use qe_domain::{Price, Qty, Side};
use qe_risk::{Admission, KillHandle, KillSwitch, OrderGate};

use crate::edge::{OrderIntent, SimFill, VenueSimulator};

/// Returned by [`VenueKillGate::submit`] when the kill switch is tripped: nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillHalt {
    /// The latched kill reason.
    pub reason: String,
}

/// The outcome of enforcing the kill at the submission layer for one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillOutcome {
    /// The switch is live — normal trading.
    Live,
    /// The switch is tripped and the flatten was handled for this tick: `Some(fill)` when a closing order
    /// was actually submitted (the gate now **latches** halted), or `None` when the position was flat/not
    /// yet known — in which case the gate stays **armed** so a position learned on a later tick is still
    /// flattened. Submission is halted regardless.
    Flattened(Option<SimFill>),
    /// The switch is tripped and a real position has already been flattened — submission stays halted.
    Halted,
}

/// The closing order that flattens `current_qty` (opposite side, full size). `None` when already flat. This
/// is computed from the kept position alone — it does **not** need a mark or a planner target, so the kill
/// can always flatten.
fn flatten_intent(current_qty: Decimal) -> Option<OrderIntent> {
    if current_qty.is_zero() {
        return None;
    }
    let (side, mag) = if current_qty.is_sign_negative() {
        (Side::Buy, -current_qty)
    } else {
        (Side::Sell, current_qty)
    };
    Some(OrderIntent {
        side,
        qty: Qty::new(mag).expect("flatten magnitude is non-negative"),
    })
}

/// The venue adapter's kill gate: the QE-009 out-of-band halt, enforced at order submission.
pub struct VenueKillGate {
    kill: KillHandle,
    sim: VenueSimulator,
    /// Whether a real (non-flat) position has already been flattened, so the closing order is submitted at
    /// most once. Stays `false` while the position is flat/unknown, keeping the gate armed for a position
    /// that only becomes known on a later tick (e.g. after a keeper reconnect).
    flattened: bool,
}

impl VenueKillGate {
    /// A gate over `sim`, honouring `kill`.
    #[must_use]
    pub fn new(kill: KillHandle, sim: VenueSimulator) -> Self {
        Self {
            kill,
            sim,
            flattened: false,
        }
    }

    /// The held kill handle (clone it to trip out-of-band, or to observe).
    #[must_use]
    pub fn kill(&self) -> &KillHandle {
        &self.kill
    }

    /// The underlying simulator (read-only).
    #[must_use]
    pub fn simulator(&self) -> &VenueSimulator {
        &self.sim
    }

    /// Submit a normal order — **unless** the kill is tripped, in which case nothing is sent and this halts
    /// with `Err(KillHalt)`.
    ///
    /// # Errors
    /// [`KillHalt`] when the kill switch is tripped.
    pub fn submit(
        &mut self,
        intent: OrderIntent,
        fill_price: Price,
        event_time_ms: i64,
    ) -> Result<SimFill, KillHalt> {
        if self.kill.is_tripped() {
            // Reuse the QE-009 shared fallback so this halt reason matches `admit`'s `FlattenAndHalt`.
            return Err(KillHalt {
                reason: self.kill_reason(),
            });
        }
        Ok(self.sim.submit(intent, fill_price, event_time_ms))
    }

    /// Enforce the out-of-band kill for one tick. While the switch is tripped and a real position has not
    /// yet been flattened, it **flattens** the kept position (`current_qty`, signed contracts) by submitting
    /// the closing order directly to the simulator — the kill's own action, so it bypasses the submission
    /// halt. It latches halted only *after* a non-flat position has actually been flattened; a flat (or
    /// not-yet-known) `current_qty` leaves the gate armed, so a position that only becomes known on a later
    /// tick — e.g. after a QE-217 keeper reconnect that re-absorbs the authoritative snapshot — is still
    /// flattened. Driven only by the [`KillHandle`]; no planner target or cockpit is involved.
    pub fn enforce_kill(
        &mut self,
        current_qty: Decimal,
        fill_price: Price,
        event_time_ms: i64,
    ) -> KillOutcome {
        if !self.kill.is_tripped() {
            return KillOutcome::Live;
        }
        if self.flattened {
            return KillOutcome::Halted;
        }
        match flatten_intent(current_qty) {
            // A real position: flatten it and latch. Latching here also guards against a double-flatten
            // from keeper fill latency (a later tick still reporting the pre-flatten qty is ignored).
            Some(intent) => {
                let fill = self.sim.submit(intent, fill_price, event_time_ms);
                self.flattened = true;
                KillOutcome::Flattened(Some(fill))
            }
            // Flat, or the position is not yet known: stay armed so it is still flattened once learned.
            None => KillOutcome::Flattened(None),
        }
    }
}

impl OrderGate for VenueKillGate {
    fn kill_handle(&self) -> &KillHandle {
        &self.kill
    }
    // QE-216 is the kill; order sizing/limits are QE-215. The QE-009 default `admit` applies the kill
    // precheck structurally, so a tripped switch yields `FlattenAndHalt` before this is reached.
    fn admit_within_limits(&self, _intent: &qe_risk::OrderIntent) -> Admission {
        Admission::Admit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::VenueKeeper;
    use qe_domain::{InstrumentId, Notional};
    use qe_risk::assert_honours_kill_switch;
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
    fn gate() -> VenueKillGate {
        VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument()))
    }

    /// AC (contract): the gate satisfies the QE-009 order-gate conformance — untripped is live; tripped makes
    /// `admit` flatten-and-halt and `ensure_live` a Halt disposition.
    #[test]
    fn gate_honours_kill_switch_conformance() {
        assert_honours_kill_switch(&gate());
    }

    /// AC (behaviour): tripping the kill flattens the position and halts submission — no planner involved.
    #[test]
    fn kill_flattens_position_and_halts_submission() {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));
        keeper.observe_mark(price("50000"));
        let mut gate = gate();

        // Establish a long position through the gate (pre-kill).
        let fill = gate
            .submit(
                OrderIntent {
                    side: Side::Buy,
                    qty: qty("0.2"),
                },
                price("50000"),
                1,
            )
            .expect("submits while live");
        keeper.apply(&fill.event);
        assert_eq!(keeper.signed_qty(), dec("0.2"));

        // Trip the kill DIRECTLY (out-of-band) — no planner / cockpit.
        gate.kill().trip("manual stop");

        // Enforcing the kill flattens: a Sell 0.2 that returns the keeper to flat.
        let outcome = gate.enforce_kill(keeper.signed_qty(), keeper.mark(), 2);
        let flatten = match outcome {
            KillOutcome::Flattened(Some(f)) => f,
            other => panic!("expected a flattening fill, got {other:?}"),
        };
        assert_eq!(flatten.order.side, Side::Sell);
        assert_eq!(flatten.order.qty, qty("0.2"));
        keeper.apply(&flatten.event);
        assert_eq!(
            keeper.signed_qty(),
            dec("0"),
            "position is flat after the kill"
        );

        // A second enforcement just stays halted (flatten happens once).
        assert_eq!(
            gate.enforce_kill(keeper.signed_qty(), keeper.mark(), 3),
            KillOutcome::Halted
        );

        // Submission is halted, and the order gate reports flatten-and-halt.
        let halt = gate
            .submit(
                OrderIntent {
                    side: Side::Buy,
                    qty: qty("0.1"),
                },
                price("50000"),
                4,
            )
            .expect_err("submission is halted");
        assert_eq!(halt.reason, "manual stop");
        assert!(matches!(
            gate.admit(&conformance_like_intent()),
            Admission::FlattenAndHalt(_)
        ));
    }

    /// The trigger is out-of-band: a *clone* of the handle (as a watchdog holds) trips the gate, with no
    /// planner/cockpit call, and the gate flattens-and-halts.
    #[test]
    fn out_of_band_trip_via_cloned_handle_flattens() {
        let mut gate = gate();
        let watchdog = gate.kill().clone(); // a separate holder of the same kill state

        assert_eq!(
            gate.enforce_kill(dec("-0.5"), price("50000"), 1),
            KillOutcome::Live
        );
        watchdog.trip("watchdog: staleness");

        // Short 0.5 → flatten with a Buy 0.5.
        match gate.enforce_kill(dec("-0.5"), price("50000"), 2) {
            KillOutcome::Flattened(Some(f)) => {
                assert_eq!(f.order.side, Side::Buy);
                assert_eq!(f.order.qty, qty("0.5"));
            }
            other => panic!("expected flatten, got {other:?}"),
        }
        assert!(gate.kill().is_tripped());
    }

    /// A kill while the position is flat submits no order but leaves the gate **armed**: if a real position
    /// is only learned on a later tick — e.g. a watchdog trips during a reconnect window where the QE-217
    /// keeper has reset and not yet re-absorbed the authoritative snapshot — it must still be flattened
    /// (F1 regression). Only after that real flatten does the gate latch to `Halted`.
    #[test]
    fn flat_first_call_stays_armed_and_still_flattens_a_later_position() {
        let mut gate = gate();
        gate.kill().trip("halt");

        // Flat (or not-yet-known) at the first post-trip call: no order, and NOT latched.
        assert_eq!(
            gate.enforce_kill(dec("0"), price("50000"), 1),
            KillOutcome::Flattened(None)
        );
        assert_eq!(
            gate.simulator().orders_submitted(),
            0,
            "no order for a flat position"
        );
        // Still flat → still armed, still no order.
        assert_eq!(
            gate.enforce_kill(dec("0"), price("50000"), 2),
            KillOutcome::Flattened(None)
        );

        // The keeper now knows the real position → it is flattened, and only now does the gate latch.
        match gate.enforce_kill(dec("0.2"), price("50000"), 3) {
            KillOutcome::Flattened(Some(f)) => {
                assert_eq!(f.order.side, Side::Sell);
                assert_eq!(f.order.qty, qty("0.2"));
            }
            other => panic!("expected the later position to be flattened, got {other:?}"),
        }
        assert_eq!(gate.simulator().orders_submitted(), 1);
        assert_eq!(
            gate.enforce_kill(dec("0.2"), price("50000"), 4),
            KillOutcome::Halted,
            "latched after the real flatten — no double-flatten on keeper fill latency"
        );
    }

    /// Submission works until the kill trips, then halts (and latches).
    #[test]
    fn submit_succeeds_until_killed_then_halts() {
        let mut gate = gate();
        let intent = OrderIntent {
            side: Side::Buy,
            qty: qty("0.1"),
        };
        assert!(gate.submit(intent, price("50000"), 1).is_ok());

        gate.kill().trip("stop");
        assert!(gate.submit(intent, price("50000"), 2).is_err());
        // Latched: still halted.
        assert!(gate.submit(intent, price("50000"), 3).is_err());
    }

    fn conformance_like_intent() -> qe_risk::OrderIntent {
        qe_risk::OrderIntent {
            instrument: instrument(),
            direction: qe_domain::Direction::Long,
            qty: qty("1"),
            price: price("50000"),
        }
    }
}
