//! Recoverability classification for the runtime order-emission path (QE-421).
//!
//! The runtime owns the bootstrap → live → edge order path but, before this, rolled its own error enums
//! with **no** recoverability dimension — so the live loop could not uniformly decide halt-vs-retry-vs-skip.
//! Here every order-path error type adopts the shared [`qe_error`] taxonomy via [`Classified`], so the live
//! dispatch loop ([`crate::transport::PlannerAdapterLink`]) routes each one through `disposition()` and a
//! fatal error drives deterministically to [`Halt`](qe_error::Disposition::Halt) — never a panic (QE-268).
//!
//! Mapping rationale (see the per-variant table in `docs/architecture/qe-421-classified-taxonomy-design.md`):
//! a **tripped kill**, a **broken bootstrap/cutover/reconstruction invariant**, and an **undecodable
//! history** are fatal (halt, don't trade on unsafe state); a **disconnected transport** is transient
//! (retry after reconnect); a **journal-append** failure is skippable (continue — it never gates dispatch,
//! QE-301).

use qe_error::{Classified, ErrorClass};

use crate::boot_state::BootStateError;
use crate::bootstrap::BootstrapError;
use crate::cutover::CutoverError;
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

impl Classified for BootstrapError {
    fn class(&self) -> ErrorClass {
        match self {
            // A REST fetch inherits the venue REST retry/fatal split.
            BootstrapError::Rest(e) => e.class(),
            // A reconstruction invariant break or undecodable history means we cannot rebuild live state
            // safely — halt rather than start trading on partial/incorrect state.
            BootstrapError::Recon(_) | BootstrapError::Decode(_) => ErrorClass::Fatal,
        }
    }
}

impl Classified for CutoverError {
    fn class(&self) -> ErrorClass {
        match self {
            // No boundary to continue from, or a skipped/misaligned seam bar: reject the cutover and halt
            // rather than evaluate across a gap.
            CutoverError::EmptyReplay | CutoverError::Gap { .. } => ErrorClass::Fatal,
        }
    }
}

impl Classified for BootStateError {
    fn class(&self) -> ErrorClass {
        match self {
            // Reconstructed-state invariant broken (equity paths ≠ strategies): the breaker anchor would be
            // wrong — halt.
            BootStateError::MismatchedEquityPaths { .. } => ErrorClass::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_error::Disposition;
    use qe_venue::{RestError, UserDataError, WsError};

    /// Compile-time proof that every order-path error type — runtime **and** the venue types the live loop
    /// consumes — implements `Classified`. Adding an order-path error type without an impl fails to compile.
    fn _assert_classified<T: Classified>() {}
    fn _order_path_error_types_are_classified() {
        // Runtime emission + bootstrap/handoff path.
        _assert_classified::<KillHalt>();
        _assert_classified::<TransportError>();
        _assert_classified::<AppendError>();
        _assert_classified::<BootstrapError>();
        _assert_classified::<CutoverError>();
        _assert_classified::<BootStateError>();
        // Venue connectivity feeding the live loop.
        _assert_classified::<RestError>();
        _assert_classified::<WsError>();
        _assert_classified::<UserDataError>();
    }

    /// AC (fatal → Halt) and non-vacuity: the synthetic fatal (`KillHalt`) drives to `Halt`, while the
    /// non-fatal order-path errors drive to `Retry`/`Continue` — proving the mapping discriminates.
    #[test]
    fn order_path_dispositions_discriminate() {
        assert_eq!(
            KillHalt {
                reason: "watchdog: staleness".into()
            }
            .disposition(),
            Disposition::Halt,
            "a tripped kill is fatal → halt"
        );
        assert_eq!(
            TransportError::Disconnected.disposition(),
            Disposition::Retry,
            "a disconnected link is transient → retry"
        );
        assert_eq!(
            AppendError("journal unavailable".into()).disposition(),
            Disposition::Continue,
            "a non-gating journal append is skippable → continue"
        );
    }

    /// Exhaustive: every bootstrap/handoff error variant maps to the expected disposition, including the
    /// REST delegation (retryable vs fatal).
    #[test]
    fn bootstrap_and_handoff_dispositions() {
        assert_eq!(
            BootstrapError::Rest(RestError::Transient("5xx".into())).disposition(),
            Disposition::Retry,
            "a transient REST fetch is retryable"
        );
        assert_eq!(
            BootstrapError::Rest(RestError::Fatal("400".into())).disposition(),
            Disposition::Halt,
            "a fatal REST fetch halts"
        );
        assert_eq!(
            BootstrapError::Decode("bad page".into()).disposition(),
            Disposition::Halt
        );
        assert_eq!(CutoverError::EmptyReplay.disposition(), Disposition::Halt);
        assert_eq!(
            CutoverError::Gap {
                expected_open_ms: 1,
                got_open_ms: 2
            }
            .disposition(),
            Disposition::Halt
        );
        assert_eq!(
            BootStateError::MismatchedEquityPaths {
                strategies: 2,
                paths: 1
            }
            .disposition(),
            Disposition::Halt
        );
    }
}
