//! QE-009 — risk-limit & kill-switch **contract** (shared types).
//!
//! The order path is *born* with hard caps and an out-of-band halt. This crate defines the
//! vocabulary every downstream order-submitting component must honour — it does **not** enforce it
//! (limit-checking maths is QE-215/216):
//! - [`limit`] — [`LimitKind`], the per-kind [`LimitOutcome`] policy, validated [`Leverage`]/
//!   [`Fraction`] caps, and the [`RiskLimits`] cap set;
//! - [`kill`] — the out-of-band, latching [`KillSwitch`] / [`KillHandle`];
//! - [`gate`] — the [`OrderGate`] contract (every order path holds a [`KillHandle`]) with a reusable
//!   [`assert_honours_kill_switch`](gate::assert_honours_kill_switch) conformance check.
//!
//! A tripped kill or a `Halt`-outcome limit is expressed as a Fatal [`qe_error::QeError`]
//! (`disposition == Halt`) — the same kill/halt channel as QE-008.

pub mod gate;
pub mod kill;
pub mod limit;

pub use gate::{assert_honours_kill_switch, Admission, OrderGate, OrderIntent};
pub use kill::{KillHandle, KillSwitch};
pub use limit::{Fraction, Leverage, LimitBreach, LimitKind, LimitOutcome, RiskLimits};

use thiserror::Error;

/// Errors constructing risk-limit value types.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RiskError {
    /// A leverage cap was negative.
    #[error("leverage must be >= 0 (got {0})")]
    NegativeLeverage(String),

    /// A fraction was outside the inclusive range `[0, 1]`.
    #[error("fraction must be within [0, 1] (got {0})")]
    FractionOutOfRange(String),
}
