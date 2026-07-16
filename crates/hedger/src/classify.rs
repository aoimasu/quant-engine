//! Recoverability classification for the hedger's bootstrap → handoff path (QE-421).
//!
//! The bootstrap/cutover/reconstruction error types adopt the shared [`qe_error`] taxonomy via
//! [`Classified`], so the live loop routes each through `disposition()` and a fatal (a broken
//! bootstrap/cutover/reconstruction invariant, or undecodable history) drives deterministically to
//! [`Halt`](qe_error::Disposition::Halt) — never a panic (QE-268). QE-426 split these impls out of the
//! single `qe-runtime::classify` module into the crate that owns each error type (the orphan rule requires
//! a foreign-trait impl to live with its type); the edge-side impls live in `qe_edge::classify`, and the
//! cross-cutting taxonomy test lives in the `qe-runtime` facade.
//!
//! Mapping rationale (see `docs/architecture/qe-421-classified-taxonomy-design.md`): a **broken
//! bootstrap/cutover/reconstruction invariant** and an **undecodable history** are fatal (halt, don't trade
//! on unsafe state).

use qe_error::{Classified, ErrorClass};

use crate::boot_state::BootStateError;
use crate::bootstrap::BootstrapError;
use crate::cutover::CutoverError;

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
