//! Clock-skew evaluation, health, halt decision, and structured logging.

use std::fmt;

use qe_domain::Timestamp;
use qe_error::QeError;
use qe_telemetry::Correlation;
use serde::{Deserialize, Serialize};

use crate::ClockError;

/// Default maximum tolerated absolute skew: 1 second (mark price refreshes @1s; well under a typical
/// 5s signed-request `recvWindow`).
pub const DEFAULT_MAX_SKEW_MS: i64 = 1_000;

/// The `tracing` target for clock-health events.
pub const TARGET: &str = "qe::clock";

/// Clock health surfaced to the cockpit (QE-304).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockHealth {
    /// Local clock is within the skew threshold of the reference.
    InSync,
    /// Skew exceeds the threshold — trading must halt.
    Skewed,
}

impl fmt::Display for ClockHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClockHealth::InSync => "in_sync",
            ClockHealth::Skewed => "skewed",
        })
    }
}

/// The outcome of one skew evaluation — always produced, always loggable, regardless of breach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkewReading {
    /// Signed skew `local - reference`, in milliseconds (positive = local clock ahead).
    pub skew_ms: i64,
    /// The threshold this reading was judged against, in milliseconds.
    pub threshold_ms: i64,
    /// The resulting health signal.
    pub health: ClockHealth,
}

impl SkewReading {
    /// Whether this reading is a threshold breach.
    #[must_use]
    pub fn is_breach(&self) -> bool {
        self.health == ClockHealth::Skewed
    }

    /// The Fatal error to route to the halt path on a breach, or `None` when in sync.
    ///
    /// The returned error is `ErrorClass::Fatal`, so `qe_error::disposition` maps it to
    /// [`Halt`](qe_error::Disposition::Halt).
    #[must_use]
    pub fn breach(&self) -> Option<QeError> {
        self.is_breach().then(|| {
            QeError::fatal(format!(
                "clock skew {}ms exceeds ±{}ms threshold",
                self.skew_ms, self.threshold_ms
            ))
        })
    }
}

/// Monitors local-vs-reference clock skew against a fixed threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkewGuard {
    max_abs_skew_ms: i64,
}

impl SkewGuard {
    /// Construct a guard tolerating up to `max_abs_skew_ms` of absolute skew.
    ///
    /// # Errors
    /// [`ClockError::InvalidThreshold`] if `max_abs_skew_ms <= 0`.
    pub fn new(max_abs_skew_ms: i64) -> Result<Self, ClockError> {
        if max_abs_skew_ms <= 0 {
            return Err(ClockError::InvalidThreshold(max_abs_skew_ms));
        }
        Ok(SkewGuard { max_abs_skew_ms })
    }

    /// The configured threshold, in milliseconds.
    #[must_use]
    pub fn threshold_ms(&self) -> i64 {
        self.max_abs_skew_ms
    }

    /// Evaluate one `(local, reference)` sample into a [`SkewReading`]. Pure and panic-free.
    #[must_use]
    pub fn evaluate(&self, local: Timestamp, reference: Timestamp) -> SkewReading {
        // saturating_sub + unsigned_abs avoid any overflow/`abs()` panic on extreme instants.
        let skew_ms = local.millis().saturating_sub(reference.millis());
        let health = if skew_ms.unsigned_abs() > self.max_abs_skew_ms.unsigned_abs() {
            ClockHealth::Skewed
        } else {
            ClockHealth::InSync
        };
        SkewReading {
            skew_ms,
            threshold_ms: self.max_abs_skew_ms,
            health,
        }
    }

    /// Evaluate, returning `Ok(reading)` when in sync and `Err(Fatal)` on a breach.
    ///
    /// The error's [`disposition`](qe_error::disposition) is [`Halt`](qe_error::Disposition::Halt).
    ///
    /// **This does not log.** It performs only the halt decision and drops the [`SkewReading`] on a
    /// breach (the skew detail survives in the error message). For the full "log *and* halt" path —
    /// the one a runtime/kill-path call site (QE-009) should use — call
    /// [`check_and_log`](Self::check_and_log), or pair this with [`record_skew`] yourself.
    ///
    /// # Errors
    /// A Fatal [`QeError`] when `|local - reference|` exceeds the threshold.
    pub fn check(&self, local: Timestamp, reference: Timestamp) -> qe_error::Result<SkewReading> {
        let reading = self.evaluate(local, reference);
        match reading.breach() {
            Some(err) => Err(err),
            None => Ok(reading),
        }
    }

    /// Evaluate, **log** the reading with the correlation context, then halt on a breach.
    ///
    /// This is the recommended call site for the live runtime / kill path: it always emits the
    /// clock-health event (via [`record_skew`] — `info` in sync, `warn` on breach) *and* returns
    /// `Err(Fatal)` on a breach, so a breach is logged and routed to the halt path in one call —
    /// never halted without a trace and never silently continued.
    ///
    /// # Errors
    /// A Fatal [`QeError`] when `|local - reference|` exceeds the threshold.
    pub fn check_and_log(
        &self,
        local: Timestamp,
        reference: Timestamp,
        c: &Correlation,
    ) -> qe_error::Result<SkewReading> {
        let reading = self.evaluate(local, reference);
        record_skew(&reading, c);
        match reading.breach() {
            Some(err) => Err(err),
            None => Ok(reading),
        }
    }
}

impl Default for SkewGuard {
    fn default() -> Self {
        SkewGuard {
            max_abs_skew_ms: DEFAULT_MAX_SKEW_MS,
        }
    }
}

/// Emit a structured clock-health event carrying the correlation fields, the skew, and the health
/// state — `warn` on a breach, `info` when in sync. Satisfies "skew is logged with the correlation
/// fields and exposed as health state".
pub fn record_skew(reading: &SkewReading, c: &Correlation) {
    if reading.is_breach() {
        tracing::warn!(
            target: TARGET,
            run_id = c.run_id,
            vintage_hash = c.vintage_hash,
            instrument = c.instrument,
            window_id = c.window_id,
            skew_ms = reading.skew_ms,
            threshold_ms = reading.threshold_ms,
            health = %reading.health,
            "clock skew exceeds threshold — halting",
        );
    } else {
        tracing::info!(
            target: TARGET,
            run_id = c.run_id,
            vintage_hash = c.vintage_hash,
            instrument = c.instrument,
            window_id = c.window_id,
            skew_ms = reading.skew_ms,
            threshold_ms = reading.threshold_ms,
            health = %reading.health,
            "clock skew within threshold",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_error::{disposition, Disposition};

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn new_rejects_nonpositive_threshold() {
        assert_eq!(SkewGuard::new(0), Err(ClockError::InvalidThreshold(0)));
        assert_eq!(SkewGuard::new(-5), Err(ClockError::InvalidThreshold(-5)));
        assert!(SkewGuard::new(1).is_ok());
        assert_eq!(SkewGuard::default().threshold_ms(), DEFAULT_MAX_SKEW_MS);
    }

    #[test]
    fn in_sync_at_and_below_threshold_both_signs() {
        let guard = SkewGuard::new(1_000).unwrap();
        // exactly at threshold is in sync (breach is strictly greater)
        assert_eq!(guard.evaluate(at(1_000), at(0)).health, ClockHealth::InSync);
        assert_eq!(guard.evaluate(at(0), at(1_000)).health, ClockHealth::InSync);
        assert_eq!(guard.evaluate(at(500), at(0)).health, ClockHealth::InSync);
    }

    #[test]
    fn breach_beyond_threshold_both_signs() {
        let guard = SkewGuard::new(1_000).unwrap();
        assert_eq!(guard.evaluate(at(1_001), at(0)).health, ClockHealth::Skewed); // local ahead
        assert_eq!(guard.evaluate(at(0), at(1_001)).health, ClockHealth::Skewed);
        // local behind
    }

    #[test]
    fn check_returns_fatal_halt_on_breach() {
        let guard = SkewGuard::new(1_000).unwrap();
        let err = guard.check(at(5_000), at(0)).unwrap_err();
        assert!(err.is_fatal());
        assert_eq!(disposition(&err), Disposition::Halt); // a halt, not a silent continue
    }

    #[test]
    fn check_ok_within_threshold() {
        let guard = SkewGuard::new(1_000).unwrap();
        let reading = guard.check(at(100), at(50)).unwrap();
        assert_eq!(reading.skew_ms, 50);
        assert_eq!(reading.health, ClockHealth::InSync);
        assert!(reading.breach().is_none());
    }

    #[test]
    fn check_and_log_matches_check_decision() {
        // check_and_log must make the same Ok/Err decision as check (it only adds logging).
        let guard = SkewGuard::new(1_000).unwrap();
        let corr = Correlation::run("r", "-");
        assert!(guard.check_and_log(at(5_000), at(0), &corr).is_err());
        let ok = guard.check_and_log(at(100), at(50), &corr).unwrap();
        assert_eq!(ok.health, ClockHealth::InSync);
    }

    #[test]
    fn evaluate_does_not_panic_on_extreme_opposite_instants() {
        let guard = SkewGuard::new(1_000).unwrap();
        // i64::MIN vs i64::MAX would overflow a naive subtraction / abs(); must not panic.
        let reading = guard.evaluate(at(i64::MIN), at(i64::MAX));
        assert_eq!(reading.health, ClockHealth::Skewed);
    }

    #[test]
    fn health_serialises_snake_case() {
        assert_eq!(
            serde_json::to_string(&ClockHealth::Skewed).unwrap(),
            "\"skewed\""
        );
        assert_eq!(ClockHealth::InSync.to_string(), "in_sync");
    }
}
