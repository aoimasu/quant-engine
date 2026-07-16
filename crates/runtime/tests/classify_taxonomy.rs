//! QE-421 recoverability taxonomy — the cross-cutting proof that spans both split crates.
//!
//! The per-crate `Classified` impls live in `qe_edge::classify` (kill / transport / append) and
//! `qe_hedger::classify` (bootstrap / cutover / reconstruction) — the orphan rule requires each foreign-trait
//! impl to live with its error type. This test exercises **both** sides plus the venue connectivity errors
//! the live loop consumes, so it lives in the `qe-runtime` facade (the only crate that links both). Moved
//! verbatim from the pre-split `qe-runtime::classify` unit tests (QE-426).

use qe_error::{Classified, Disposition};
use qe_runtime::{
    AppendError, BootStateError, BootstrapError, CutoverError, KillHalt, TransportError,
};
use qe_venue::{RestError, UserDataError, WsError};

/// Compile-time proof that every order-path error type — runtime **and** the venue types the live loop
/// consumes — implements `Classified`. Adding an order-path error type without an impl fails to compile.
fn _assert_classified<T: Classified>() {}
#[allow(dead_code)]
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
