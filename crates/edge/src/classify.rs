//! Recoverability classification for the edge order-emission path (QE-421).
//!
//! Every edge order-path error type adopts the shared [`qe_error`] taxonomy via [`Classified`], so the live
//! dispatch loop ([`crate::transport::PlannerAdapterLink`]) routes each one through `disposition()` and a
//! fatal error (a tripped kill) drives deterministically to [`Halt`](qe_error::Disposition::Halt) — never a
//! panic (QE-268). QE-426 split these impls out of the single `qe-runtime::classify` module into the crate
//! that owns each error type (the orphan rule requires a foreign-trait impl to live with its type); the
//! hedger-side impls live in `qe_hedger::classify`, and the cross-cutting taxonomy test lives in the
//! `qe-runtime` facade.
//!
//! Mapping rationale (see `docs/architecture/qe-421-classified-taxonomy-design.md`): a **tripped kill** is
//! fatal (halt, don't trade on unsafe state); a **disconnected transport** is transient (retry after
//! reconnect); a **journal-append** failure is skippable (continue — it never gates dispatch, QE-301).

use qe_error::{Classified, ErrorClass};

use crate::kill_gate::KillHalt;
use crate::transport::{AppendError, TransportError};

impl Classified for KillHalt {
    fn class(&self) -> ErrorClass {
        // A tripped kill is the out-of-band halt: submission must stop deterministically. This is the
        // fatal that "halt not panic" exists for, on the exact path where it matters most.
        ErrorClass::Fatal
    }
}

impl Classified for TransportError {
    fn class(&self) -> ErrorClass {
        match self {
            // The link is down; the planner awaits `reconnect` and re-sends its latest absolute target.
            TransportError::Disconnected => ErrorClass::Transient,
        }
    }
}

impl Classified for AppendError {
    fn class(&self) -> ErrorClass {
        // The QE-301 journal append is non-gating: a failure is skipped/quarantined and dispatch continues.
        ErrorClass::Data
    }
}
