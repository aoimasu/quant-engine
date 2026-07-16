//! qe-edge — the Edge gateway ⑥: the venue adapter / position keeper / kill gate / order submission (QE-426).
//!
//! Split out of `qe-runtime` so the **order-submitting** code compiles as its own crate and the gRPC seam
//! (QE-218) to the Hedge Planner ⑤ (`qe-hedger`) is a **crate** boundary. This crate is the deployment /
//! security boundary — the only thing that submits orders — so its dependency surface is deliberately tight
//! (`qe-domain`/`qe-venue`/`qe-risk`/`qe-error` + the shared [`qe_runtime_core`] contract) and its
//! order-emission modules carry the QE-268 panic-free lint scope (`edge`, `kill_gate`).
//!
//! - [`edge`] — `plan_delta` (the stateless→stateful bridge), [`VenueKeeper`](edge::VenueKeeper) (the QE-217
//!   position keeper, `impl`s [`PositionKeeper`](qe_runtime_core::PositionKeeper)), and
//!   [`VenueSimulator`](edge::VenueSimulator).
//! - [`kill_gate`] — [`VenueKillGate`](kill_gate::VenueKillGate), the QE-216 out-of-band kill on submission.
//! - [`transport`] — [`PlannerAdapterLink`](transport::PlannerAdapterLink), the adapter side of the QE-218
//!   planner↔adapter stream (backpressure / reconnection / journal-append).
//! - [`reconciliation`] — [`ReconciliationGuard`](reconciliation::ReconciliationGuard), the QE-221 divergence
//!   alarm.
//! - [`shadow`] — [`ShadowGateway`](shadow::ShadowGateway) / [`ShadowRun`](shadow::ShadowRun), the QE-222 G2
//!   dry-run.

pub mod edge;
pub mod kill_gate;
pub mod reconciliation;
pub mod shadow;
pub mod transport;

// QE-421 recoverability taxonomy: the edge-side `Classified` impls (kill / transport / append). No public
// items — the module exists to compile the trait impls in the crate that owns the error types.
mod classify;

pub use edge::{plan_delta, Order, OrderIntent, OrderState, SimFill, VenueKeeper, VenueSimulator};
pub use kill_gate::{KillHalt, KillOutcome, VenueKillGate};
pub use reconciliation::{AlarmAction, Divergence, ReconOutcome, ReconciliationGuard};
pub use shadow::{ShadowGateway, ShadowReport, ShadowRun, WouldBeOrder};
pub use transport::{
    AdapterReport, AppendError, AppendSink, NullAppendSink, PlannerAdapterLink, TargetRevision,
    TransportError, VenueHealth,
};

/// The runtime's live order-submission port.
///
/// It is an [`OrderGate`](qe_risk::OrderGate) by definition, so every component on the live order
/// path is *born* holding a [`KillHandle`](qe_risk::KillHandle) (QE-009 contract) and can be
/// flattened-and-halted out-of-band — independently of the cockpit and the Hedge Planner. Concrete ports and
/// limit enforcement land in later tickets (QE-215/216); this is the interface they must satisfy.
pub trait OrderPort: qe_risk::OrderGate {
    /// A stable name for this port, for logging and health.
    fn port_name(&self) -> &str;
}
