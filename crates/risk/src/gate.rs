//! The order-submission contract: every order gate holds a kill handle and honours it first.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use qe_domain::{Direction, InstrumentId, Price, Qty};
use qe_error::QeError;

use crate::kill::{KillHandle, KillSwitch};

/// A proposed order, before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderIntent {
    /// The instrument to trade.
    pub instrument: InstrumentId,
    /// Long or short.
    pub direction: Direction,
    /// Requested quantity.
    pub qty: Qty,
    /// Requested limit price.
    pub price: Price,
}

/// The gate's verdict on an order intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Submit the order as-is.
    Admit,
    /// Submit, but first clamp the quantity to this maximum.
    Clamp(Qty),
    /// Reject this order; keep trading.
    Reject(String),
    /// Flatten all positions and halt trading (kill / `Halt`-outcome limit).
    FlattenAndHalt(String),
}

/// The contract every order-submitting component must satisfy.
///
/// Holding a [`KillHandle`] is a **compile-time requirement** (`kill_handle`), and the provided
/// [`kill_precheck`](OrderGate::kill_precheck) / [`ensure_live`](OrderGate::ensure_live) give every
/// gate the out-of-band flatten-and-halt behaviour for free. Enforcement of the size/margin limits
/// themselves lives in [`admit`](OrderGate::admit) and is implemented by QE-215/216.
pub trait OrderGate {
    /// The out-of-band kill handle this component honours. Every order path must hold one.
    fn kill_handle(&self) -> &KillHandle;

    /// The component's limit/sizing decision for `intent`, assuming the kill switch is **live**.
    ///
    /// Implement this, not [`admit`](OrderGate::admit): the kill check is applied *structurally* by
    /// `admit` before this is ever reached, so a gate cannot accidentally (or silently) submit while
    /// tripped. Enforcement of the size/margin limits here is QE-215/216.
    fn admit_within_limits(&self, intent: &OrderIntent) -> Admission;

    /// Final admission: the out-of-band kill switch is checked **first** (structurally), then the
    /// component's limit decision. A tripped kill always yields [`Admission::FlattenAndHalt`].
    ///
    /// The default is what guarantees the kill is honoured — override only with great care (the
    /// conformance check would then re-prove the override still flattens-and-halts when tripped).
    fn admit(&self, intent: &OrderIntent) -> Admission {
        match self.kill_precheck() {
            Some(halt) => halt,
            None => self.admit_within_limits(intent),
        }
    }

    /// The kill check every gate performs before anything else: `Some(FlattenAndHalt)` when the
    /// switch is tripped, else `None`.
    fn kill_precheck(&self) -> Option<Admission> {
        let kill = self.kill_handle();
        kill.is_tripped().then(|| {
            Admission::FlattenAndHalt(
                kill.reason()
                    .unwrap_or_else(|| "kill switch tripped".to_owned()),
            )
        })
    }

    /// `Err(Fatal)` (→ [`Halt`](qe_error::Disposition::Halt)) when the kill switch is tripped, else
    /// `Ok(())`. Lets a gate route the halt through the QE-004 disposition path.
    ///
    /// # Errors
    /// A Fatal [`QeError`] when the kill switch is tripped.
    fn ensure_live(&self) -> qe_error::Result<()> {
        let kill = self.kill_handle();
        if kill.is_tripped() {
            Err(QeError::fatal(format!(
                "kill switch tripped: {}",
                kill.reason()
                    .unwrap_or_else(|| "out-of-band halt".to_owned())
            )))
        } else {
            Ok(())
        }
    }
}

/// A representative benign order used by [`assert_honours_kill_switch`] to exercise the `admit` path.
fn conformance_intent() -> OrderIntent {
    OrderIntent {
        instrument: InstrumentId::new("BTCUSDT").expect("valid symbol"),
        direction: Direction::Long,
        qty: Qty::new(Decimal::ONE).expect("valid qty"),
        price: Price::new(Decimal::ONE).expect("valid price"),
    }
}

/// Reusable conformance check (AC #2): assert a gate accepts **and honours** its kill handle on the
/// actual order-submission path.
///
/// An untripped gate must not kill-precheck and must be live; after the shared handle is tripped, the
/// gate's `kill_precheck`, **its `admit` decision**, and `ensure_live` must all reflect the kill —
/// `admit` returns [`Admission::FlattenAndHalt`] and `ensure_live` is a
/// [`Halt`](qe_error::Disposition::Halt) disposition. Exercising `admit` (not just the helpers) is
/// what makes this prove the order path honours the kill, rather than merely that a handle is held.
///
/// # Panics
/// Panics if `gate` does not honour its kill handle on any of those paths.
pub fn assert_honours_kill_switch<G: OrderGate>(gate: &G) {
    let intent = conformance_intent();

    assert!(
        gate.kill_precheck().is_none(),
        "untripped gate must not kill-precheck"
    );
    assert!(gate.ensure_live().is_ok(), "untripped gate must be live");

    gate.kill_handle().trip("conformance");

    match gate.kill_precheck() {
        Some(Admission::FlattenAndHalt(_)) => {}
        other => panic!("kill_precheck ignored the kill switch: {other:?}"),
    }
    // The decisive check: the actual admission path must flatten-and-halt once tripped.
    match gate.admit(&intent) {
        Admission::FlattenAndHalt(_) => {}
        other => panic!("admit ignored the kill switch when tripped: {other:?}"),
    }
    let err = gate
        .ensure_live()
        .expect_err("tripped gate must not be live");
    assert_eq!(
        qe_error::disposition(&err),
        qe_error::Disposition::Halt,
        "kill must route to Halt"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// A minimal gate: admits everything unless the kill switch is tripped. Stands in for a real
    /// order-submitting component for the conformance check.
    struct SampleGate {
        kill: KillHandle,
    }

    impl OrderGate for SampleGate {
        fn kill_handle(&self) -> &KillHandle {
            &self.kill
        }
        // Implements only the limit decision; the kill check is applied structurally by `admit`.
        fn admit_within_limits(&self, _intent: &OrderIntent) -> Admission {
            Admission::Admit
        }
    }

    fn intent() -> OrderIntent {
        OrderIntent {
            instrument: InstrumentId::new("BTCUSDT").unwrap(),
            direction: Direction::Long,
            qty: Qty::new(Decimal::from_str("1").unwrap()).unwrap(),
            price: Price::new(Decimal::from_str("50000").unwrap()).unwrap(),
        }
    }

    #[test]
    fn sample_gate_passes_conformance() {
        let gate = SampleGate {
            kill: KillHandle::new(),
        };
        assert_honours_kill_switch(&gate);
    }

    #[test]
    fn admit_flattens_and_halts_once_killed() {
        let gate = SampleGate {
            kill: KillHandle::new(),
        };
        assert_eq!(gate.admit(&intent()), Admission::Admit);
        gate.kill_handle().trip("manual stop");
        match gate.admit(&intent()) {
            Admission::FlattenAndHalt(reason) => assert_eq!(reason, "manual stop"),
            other => panic!("expected flatten-and-halt, got {other:?}"),
        }
    }
}
