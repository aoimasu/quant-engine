//! QE-008 — clock-skew / time-sync guard.
//!
//! A leveraged venue assumes a trustworthy local clock: funding stamps, the 60s mark EMA,
//! bar-close evaluation, and signed-request windows all break under skew. [`SkewGuard`] monitors
//! local-vs-reference time, surfaces a [`ClockHealth`] signal for the cockpit, and — when skew
//! exceeds the configured threshold — produces a Fatal [`qe_error::QeError`] whose
//! [`disposition`](qe_error::disposition) is [`Halt`](qe_error::Disposition::Halt), so the runtime's
//! existing kill/halt routing (QE-009) stops trading rather than silently continuing.
//!
//! The guard is pure: it evaluates `(local, reference)` [`Timestamp`](qe_domain::Timestamp) samples
//! a caller supplies from its clock sources, which is exactly what makes skew testable. Fetching
//! reference time (NTP / venue server time) is a later integration ticket.

pub mod skew;

pub use skew::{record_skew, ClockHealth, SkewGuard, SkewReading, DEFAULT_MAX_SKEW_MS};

use thiserror::Error;

/// Errors constructing a [`SkewGuard`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClockError {
    /// The configured skew threshold was not strictly positive.
    #[error("skew threshold must be > 0 ms (got {0})")]
    InvalidThreshold(i64),
}
