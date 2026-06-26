//! qe-error — shared error taxonomy and result conventions.
//!
//! The recoverability dimension ([`ErrorClass`]) drives control flow: `Transient` errors are
//! retried, `Data` errors skip the offending datum and continue, and `Fatal` errors halt the
//! runtime (never panic). [`disposition`] maps an error to the action the runtime loop takes.
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
    match err.class() {
        ErrorClass::Transient => Disposition::Retry,
        ErrorClass::Data => Disposition::Continue,
        ErrorClass::Fatal => Disposition::Halt,
    }
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
    fn hot_path_demonstrator_is_usable() {
        assert_eq!(hot_path::clamp_nonneg(-3), 0);
        assert_eq!(hot_path::clamp_nonneg(5), 5);
    }
}
