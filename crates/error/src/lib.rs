//! qe-error — shared error taxonomy and result conventions.
//!
//! The recoverability dimension ([`ErrorClass`]) drives control flow: `Transient` errors are
//! retried, `Data` errors skip the offending datum and continue, and `Fatal` errors halt the
//! runtime (never panic). [`disposition`] maps an error to the action the runtime loop takes.
//!
//! Crate-specific `thiserror` enums opt into the taxonomy via the [`Classified`] trait
//! (`fn class(&self) -> ErrorClass`), so the runtime's live order loop can route **any** order-path error
//! through [`Classified::disposition`] uniformly — the cross-cutting halt-vs-retry-vs-skip strategy (QE-421).
//!
//! ## Hot-path lint convention
//! Modules on the order-emission path must reject `unwrap`/`expect`/`panic`. Copy this attribute
//! block at the top of such a module:
//! ```ignore
//! #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! ```
//! The CI clippy gate (QE-005) then fails the build if any of these appear there. The
//! [`hot_path`] module is a clean demonstrator; `tests/hot_path_lint.rs` proves clippy rejects an
//! `unwrap()` in such a module.

use thiserror::Error;

/// Recoverability class of an error — the dimension that drives control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Retryable (timeout, rate-limit, transient I/O).
    Transient,
    /// Skip/quarantine the offending datum and continue (bad row, parse error).
    Data,
    /// Unrecoverable — the runtime must halt (not panic).
    Fatal,
}

impl ErrorClass {
    /// The runtime action this class dispositions to — the single source of truth for the mapping
    /// (`Transient→Retry`, `Data→Continue`, `Fatal→Halt`). `Fatal` **always** halts (never panics).
    #[must_use]
    pub fn disposition(self) -> Disposition {
        match self {
            ErrorClass::Transient => Disposition::Retry,
            ErrorClass::Data => Disposition::Continue,
            ErrorClass::Fatal => Disposition::Halt,
        }
    }
}

/// A crate-local error type that carries a recoverability [`ErrorClass`], so the runtime's live loop can
/// uniformly disposition it (halt-vs-retry-vs-skip) without knowing the concrete type.
///
/// Every error reachable on the order-emission path implements this (QE-421): the supervisor routes each
/// one through [`Classified::disposition`] rather than ad-hoc per-variant handling, and a `Fatal` error
/// always drives to [`Disposition::Halt`] — the halt-not-panic guarantee, on the path where it matters most.
pub trait Classified {
    /// This error's recoverability class.
    fn class(&self) -> ErrorClass;

    /// The runtime action this error dispositions to. Defaults to [`ErrorClass::disposition`]; overriding
    /// is rarely needed since the class already determines the action.
    fn disposition(&self) -> Disposition {
        self.class().disposition()
    }
}

impl Classified for QeError {
    fn class(&self) -> ErrorClass {
        self.class
    }
}

/// The platform's standard error: a class, a human-readable context, and an optional source.
#[derive(Debug, Error)]
#[error("{class:?}: {context}")]
pub struct QeError {
    class: ErrorClass,
    context: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl QeError {
    fn new(class: ErrorClass, context: impl Into<String>) -> Self {
        Self {
            class,
            context: context.into(),
            source: None,
        }
    }

    /// Construct a retryable error.
    pub fn transient(context: impl Into<String>) -> Self {
        Self::new(ErrorClass::Transient, context)
    }

    /// Construct a skip/quarantine (data) error.
    pub fn data(context: impl Into<String>) -> Self {
        Self::new(ErrorClass::Data, context)
    }

    /// Construct an unrecoverable (fatal) error.
    pub fn fatal(context: impl Into<String>) -> Self {
        Self::new(ErrorClass::Fatal, context)
    }

    /// Attach an underlying source error, preserving the chain.
    #[must_use]
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The error's recoverability class.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        self.class
    }

    /// True for `Fatal` errors (the runtime must halt).
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        self.class == ErrorClass::Fatal
    }

    /// True for `Transient` errors (safe to retry).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.class == ErrorClass::Transient
    }
}

/// Crate-wide result alias defaulting to [`QeError`].
pub type Result<T, E = QeError> = std::result::Result<T, E>;

/// The action the runtime loop should take in response to an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Skip the offending datum and keep going.
    Continue,
    /// Retry the failed operation.
    Retry,
    /// Halt the runtime (routed to the kill/halt path; never a panic).
    Halt,
}

/// Map an error to the runtime's response. `Fatal` always routes to [`Disposition::Halt`].
#[must_use]
pub fn disposition(err: &QeError) -> Disposition {
    err.class().disposition()
}

/// Demonstrator for the hot-path lint convention (see crate docs). Modules on the order-emission
/// path copy the attribute block so clippy rejects `unwrap`/`expect`/`panic`.
pub mod hot_path {
    #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    /// Example hot-path helper written without `unwrap`/`expect`/`panic`.
    #[must_use]
    pub fn clamp_nonneg(x: i64) -> i64 {
        if x < 0 {
            0
        } else {
            x
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_routes_to_halt() {
        assert_eq!(disposition(&QeError::fatal("disk gone")), Disposition::Halt);
    }

    #[test]
    fn transient_retries_and_data_continues() {
        assert_eq!(
            disposition(&QeError::transient("timeout")),
            Disposition::Retry
        );
        assert_eq!(
            disposition(&QeError::data("bad row")),
            Disposition::Continue
        );
    }

    #[test]
    fn classification_helpers() {
        assert!(QeError::fatal("x").is_fatal());
        assert!(!QeError::fatal("x").is_retryable());
        assert!(QeError::transient("x").is_retryable());
        assert!(!QeError::transient("x").is_fatal());
        assert_eq!(QeError::data("x").class(), ErrorClass::Data);
    }

    #[test]
    fn with_source_preserves_chain() {
        let io = std::io::Error::other("underlying");
        let err = QeError::fatal("load failed").with_source(io);
        let src = std::error::Error::source(&err).expect("source present");
        assert!(src.to_string().contains("underlying"));
        assert!(err.to_string().contains("load failed"));
    }

    #[test]
    fn error_class_disposition_matches_free_function() {
        // The free `disposition` and `ErrorClass::disposition` are one mapping (single source of truth).
        for err in [
            QeError::fatal("x"),
            QeError::transient("x"),
            QeError::data("x"),
        ] {
            assert_eq!(disposition(&err), err.class().disposition());
        }
        assert_eq!(ErrorClass::Fatal.disposition(), Disposition::Halt);
        assert_eq!(ErrorClass::Transient.disposition(), Disposition::Retry);
        assert_eq!(ErrorClass::Data.disposition(), Disposition::Continue);
    }

    #[test]
    fn qeerror_is_classified_and_fatal_halts() {
        // `Classified` lets generic routing treat a `QeError` uniformly with crate-specific enums.
        fn route<E: Classified>(e: &E) -> Disposition {
            e.disposition()
        }
        assert_eq!(route(&QeError::fatal("disk gone")), Disposition::Halt);
        assert_eq!(route(&QeError::transient("timeout")), Disposition::Retry);
        assert_eq!(route(&QeError::data("bad row")), Disposition::Continue);
    }

    #[test]
    fn hot_path_demonstrator_is_usable() {
        assert_eq!(hot_path::clamp_nonneg(-3), 0);
        assert_eq!(hot_path::clamp_nonneg(5), 5);
    }
}
