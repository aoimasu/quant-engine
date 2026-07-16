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
//!
//! QE-116 adds the calibration + breaker model the kill switch's decision rides on:
//! - [`breaker`] — the smoothed-mark [`MarkEma`] observer and the three-tier [`CircuitBreaker`],
//!   runnable on history (calibration replay) and live (QE-212);
//! - [`calibration`] — the per-vintage [`CalibrationProfile`] sidecar and observed-behaviour
//!   [`calibrate_threshold`](calibration::calibrate_threshold).

pub mod breaker;
pub mod calibration;
pub mod gate;
pub mod kill;
pub mod limit;

pub use breaker::{
    replay, time_aware_alpha, BreakerThresholds, BreakerTier, CircuitBreaker, MarkEma,
    DEFAULT_FAST_WINDOW,
};
pub use calibration::{
    calibrate_threshold, calibrate_thresholds, default_calibration_margin, drawdown_distribution,
    fast_drop_distribution, quantize_calibration, CalibrationProfile, CohortThresholds,
    CALIBRATION_SCALE, DEFAULT_FAST_QUANTILE, DEFAULT_MED_QUANTILE, DEFAULT_SLOW_QUANTILE,
};
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

impl qe_error::Classified for RiskError {
    fn class(&self) -> qe_error::ErrorClass {
        // A malformed risk cap is a configuration/invariant fault: the order path must not run with an
        // unvalidated limit set, so it halts rather than continuing (QE-421).
        match self {
            RiskError::NegativeLeverage(_) | RiskError::FractionOutOfRange(_) => {
                qe_error::ErrorClass::Fatal
            }
        }
    }
}

#[cfg(test)]
mod classify_tests {
    use super::RiskError;
    use qe_error::{Classified, Disposition};

    fn _assert_classified<T: Classified>() {}
    fn _risk_error_is_classified() {
        _assert_classified::<RiskError>();
    }

    /// Exhaustive: every `RiskError` variant is a fatal config fault → halt.
    #[test]
    fn risk_error_dispositions_to_halt() {
        for e in [
            RiskError::NegativeLeverage("-1".into()),
            RiskError::FractionOutOfRange("2".into()),
        ] {
            assert_eq!(e.disposition(), Disposition::Halt);
        }
    }
}
