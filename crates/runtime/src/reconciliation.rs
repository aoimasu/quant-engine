//! Real-time reconciliation divergence alarm (QE-221).
//!
//! Reconciliation is not post-hoc only: a live mismatch between the runtime's **expected** position and the
//! venue's **authoritative** position report is a *fast* safety check that can trip the QE-216 kill-switch.
//! [`ReconciliationGuard`] compares the two each period; beyond an absolute tolerance it **alarms** and,
//! under [`AlarmAction::Halt`], **trips the kill** — flattening-and-halting out-of-band, independent of the
//! cockpit/planner.
//!
//! It is the *detector*, not the *explainer*: attribution of *why* a divergence happened is QE-302.

use rust_decimal::Decimal;

use qe_domain::Direction;
use qe_risk::{KillHandle, KillSwitch};
use qe_venue::userdata::PositionReport;

/// What to do when a check diverges beyond tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmAction {
    /// Raise the alarm only — a human/operator decides. The kill is **not** tripped.
    AlarmOnly,
    /// Raise the alarm **and** trip the QE-216 kill-switch (auto flatten-and-halt).
    Halt,
}

/// A detected reconciliation divergence beyond tolerance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The runtime's expected signed position (contracts).
    pub expected: Decimal,
    /// The venue's authoritative signed position (contracts).
    pub venue: Decimal,
    /// The absolute divergence `|expected − venue|`.
    pub delta: Decimal,
    /// Whether this divergence tripped the kill (`true` only under [`AlarmAction::Halt`]).
    pub halted: bool,
}

/// The outcome of one reconciliation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconOutcome {
    /// Within tolerance — nothing raised.
    Reconciled,
    /// Beyond tolerance — an alarm was raised (and the kill tripped iff `Halt`).
    Diverged(Divergence),
}

/// The signed position (contracts) a venue [`PositionReport`] describes (`+` long, `−` short, `0` flat).
#[must_use]
fn signed_qty(report: &PositionReport) -> Decimal {
    match report.direction {
        Some(Direction::Long) => report.qty.get(),
        Some(Direction::Short) => -report.qty.get(),
        None => Decimal::ZERO,
    }
}

/// The fast reconciliation divergence detector: compares an expected position against venue truth each period
/// and, beyond tolerance, alarms and optionally trips the QE-216 kill.
pub struct ReconciliationGuard {
    /// Absolute divergence tolerance in contracts (clamped `≥ 0`).
    tolerance: Decimal,
    action: AlarmAction,
    /// A clone of the QE-216 kill handle — trippable out-of-band on a `Halt` divergence.
    kill: KillHandle,
    /// How many beyond-tolerance alarms have been raised.
    alarms: u64,
}

impl ReconciliationGuard {
    /// A guard tripping `kill` on a `Halt` divergence beyond `tolerance` (absolute contracts, clamped `≥ 0` so
    /// a mis-configured negative bound fails safe — it alarms more, never less).
    #[must_use]
    pub fn new(tolerance: Decimal, action: AlarmAction, kill: KillHandle) -> Self {
        Self {
            tolerance: tolerance.max(Decimal::ZERO),
            action,
            kill,
            alarms: 0,
        }
    }

    /// The configured tolerance (absolute contracts).
    #[must_use]
    pub fn tolerance(&self) -> Decimal {
        self.tolerance
    }

    /// The configured beyond-tolerance action.
    #[must_use]
    pub fn action(&self) -> AlarmAction {
        self.action
    }

    /// The held kill handle (clone it to observe, or trip elsewhere).
    #[must_use]
    pub fn kill(&self) -> &KillHandle {
        &self.kill
    }

    /// How many beyond-tolerance alarms this guard has raised.
    #[must_use]
    pub fn alarms(&self) -> u64 {
        self.alarms
    }

    /// Reconcile the runtime's `expected` signed position against the venue's authoritative `venue`
    /// [`PositionReport`]. See [`check_qty`](Self::check_qty).
    pub fn check(&mut self, expected: Decimal, venue: &PositionReport) -> ReconOutcome {
        self.check_qty(expected, signed_qty(venue))
    }

    /// Reconcile `expected` against an already-signed `venue_qty`. Within tolerance → [`Reconciled`]; beyond
    /// it → an **alarm** (`alarms += 1`) and, under [`AlarmAction::Halt`], the kill is tripped. `expected`
    /// must come from the caller's own accounting, **not** the keeper the venue report already updated, or the
    /// check is circular.
    ///
    /// [`Reconciled`]: ReconOutcome::Reconciled
    pub fn check_qty(&mut self, expected: Decimal, venue_qty: Decimal) -> ReconOutcome {
        let delta = (expected - venue_qty).abs();
        if delta <= self.tolerance {
            return ReconOutcome::Reconciled;
        }
        self.alarms += 1;
        let halted = matches!(self.action, AlarmAction::Halt);
        if halted {
            self.kill.trip(&format!(
                "reconciliation divergence: expected {expected}, venue {venue_qty}, |Δ| {delta} > tol {}",
                self.tolerance
            ));
        }
        ReconOutcome::Diverged(Divergence {
            expected,
            venue: venue_qty,
            delta,
            halted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{InstrumentId, Price, Qty};
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn instrument() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    /// A venue position report: `Some(dir), qty` or flat when `dir` is `None`.
    fn report(dir: Option<Direction>, qty: &str) -> PositionReport {
        PositionReport {
            instrument: instrument(),
            direction: dir,
            qty: Qty::new(dec(qty)).unwrap(),
            entry_price: Price::new(dec("50000")).unwrap(),
            event_time_ms: 1,
        }
    }

    fn guard(tol: &str, action: AlarmAction) -> ReconciliationGuard {
        ReconciliationGuard::new(dec(tol), action, KillHandle::new())
    }

    /// AC: a desync beyond tolerance raises an alarm and halts.
    #[test]
    fn divergence_beyond_tolerance_alarms_and_halts() {
        let mut g = guard("0.1", AlarmAction::Halt);
        let outcome = g.check(dec("0.5"), &report(Some(Direction::Long), "0.3"));

        assert_eq!(
            outcome,
            ReconOutcome::Diverged(Divergence {
                expected: dec("0.5"),
                venue: dec("0.3"),
                delta: dec("0.2"),
                halted: true,
            })
        );
        assert!(g.kill().is_tripped(), "a Halt divergence trips the kill");
        assert_eq!(g.alarms(), 1);
    }

    /// Within tolerance reconciles silently — no alarm, nothing tripped.
    #[test]
    fn within_tolerance_reconciles_without_alarm() {
        let mut g = guard("0.1", AlarmAction::Halt);
        assert_eq!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.45")),
            ReconOutcome::Reconciled
        );
        assert!(!g.kill().is_tripped());
        assert_eq!(g.alarms(), 0);
    }

    /// The tolerance bound is inclusive: an exact-tolerance delta reconciles.
    #[test]
    fn exactly_at_tolerance_reconciles() {
        let mut g = guard("0.2", AlarmAction::Halt);
        // |0.5 − 0.3| == 0.2 == tolerance.
        assert_eq!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")),
            ReconOutcome::Reconciled
        );
        assert!(!g.kill().is_tripped());
    }

    /// Alarm-only mode raises the alarm but does not trip the kill.
    #[test]
    fn alarm_only_mode_alarms_without_halting() {
        let mut g = guard("0.1", AlarmAction::AlarmOnly);
        match g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")) {
            ReconOutcome::Diverged(d) => assert!(!d.halted, "AlarmOnly must not halt"),
            other => panic!("expected divergence, got {other:?}"),
        }
        assert!(
            !g.kill().is_tripped(),
            "AlarmOnly leaves the kill untripped"
        );
        assert_eq!(g.alarms(), 1);
    }

    /// A sign flip (expected long, venue short) is a divergence of the summed magnitudes — a severe desync.
    #[test]
    fn sign_flip_is_a_divergence() {
        let mut g = guard("0.1", AlarmAction::Halt);
        match g.check(dec("0.2"), &report(Some(Direction::Short), "0.2")) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.venue, dec("-0.2"));
                assert_eq!(d.delta, dec("0.4"), "a flip sums the magnitudes");
            }
            other => panic!("expected divergence, got {other:?}"),
        }
        assert!(g.kill().is_tripped());
    }

    /// A flat venue report against an expected position is a divergence (a phantom position).
    #[test]
    fn flat_venue_report_vs_expected_position_diverges() {
        let mut g = guard("0.1", AlarmAction::Halt);
        match g.check(dec("0.3"), &report(None, "0")) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.venue, dec("0"));
                assert_eq!(d.delta, dec("0.3"));
            }
            other => panic!("expected divergence, got {other:?}"),
        }
    }

    /// Alarms accumulate across periods and the kill latches (first reason preserved).
    #[test]
    fn alarms_accumulate_and_kill_latches() {
        let mut g = guard("0.1", AlarmAction::Halt);
        g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")); // first: trips
        g.check(dec("0.9"), &report(Some(Direction::Long), "0.1")); // second: still counts

        assert_eq!(g.alarms(), 2);
        assert!(g.kill().is_tripped());
        // Latched: the first divergence's reason is preserved.
        assert!(g
            .kill()
            .reason()
            .expect("a reason is latched")
            .contains("expected 0.5"));
    }

    /// A negative tolerance is clamped to zero (fails safe: any nonzero delta alarms).
    #[test]
    fn negative_tolerance_clamped_to_zero() {
        let mut g = guard("-1", AlarmAction::AlarmOnly);
        assert_eq!(g.tolerance(), dec("0"));
        // An exact match still reconciles (delta 0 ≤ 0)…
        assert_eq!(
            g.check_qty(dec("0.2"), dec("0.2")),
            ReconOutcome::Reconciled
        );
        // …but any nonzero divergence alarms.
        assert!(matches!(
            g.check_qty(dec("0.2"), dec("0.19")),
            ReconOutcome::Diverged(_)
        ));
    }
}
