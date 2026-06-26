//! AC #1 + AC #2 at the runtime layer — the runtime's `OrderPort` references the QE-009 contract,
//! and any order-submitting port must accept and honour a kill handle.

use qe_risk::{
    assert_honours_kill_switch, Admission, KillHandle, KillSwitch, OrderGate, OrderIntent,
};
use qe_runtime::OrderPort;

/// A sample live port, *constructed with* a kill handle — demonstrating "must accept a kill handle".
struct SamplePort {
    kill: KillHandle,
}

impl SamplePort {
    fn new(kill: KillHandle) -> Self {
        SamplePort { kill }
    }
}

impl OrderGate for SamplePort {
    fn kill_handle(&self) -> &KillHandle {
        &self.kill
    }
    fn admit(&self, _intent: &OrderIntent) -> Admission {
        self.kill_precheck().unwrap_or(Admission::Admit)
    }
}

impl OrderPort for SamplePort {
    fn port_name(&self) -> &str {
        "sample"
    }
}

#[test]
fn runtime_order_port_must_accept_and_honour_a_kill_handle() {
    // The port is born holding the out-of-band kill handle.
    let kill = KillHandle::new();
    let port = SamplePort::new(kill.clone());
    assert_eq!(port.port_name(), "sample");

    // The shared handle can be tripped out-of-band; the port honours it (flatten-and-halt + Halt).
    assert_honours_kill_switch(&port);
    assert!(kill.is_tripped()); // the conformance check tripped the shared handle
}
