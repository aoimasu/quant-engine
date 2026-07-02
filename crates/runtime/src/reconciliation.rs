//! Real-time reconciliation divergence alarm (QE-221).
//!
//! Reconciliation is not post-hoc only: a live mismatch between the runtime's **expected** position and the
//! venue's **authoritative** position report is a *fast* safety check that can trip the QE-216 kill-switch.
//! [`ReconciliationGuard`] compares the two each period; beyond an absolute tolerance it **alarms** and,
//! under [`AlarmAction::HaltAfter`], trips the kill once the divergence has **persisted** for the configured
//! number of consecutive checks — flattening-and-halting out-of-band, independent of the cockpit/planner.
//!
//! **Why a debounce and not a single-check halt.** Venue `PositionReport`s are eventually-consistent: a
//! periodic check will routinely fire while an order is *in flight* — `expected` already reflects an order the
//! venue has not yet reported filled, so `delta` briefly equals the in-flight quantity. Auto-halting the whole
//! book on that benign one-period skew makes the control unusable. A genuine desync **persists** across
//! periods, whereas a propagation blip clears on the next check (which resets the streak), so
//! [`HaltAfter { consecutive }`](AlarmAction::HaltAfter) halts a *sustained* divergence while ignoring a
//! transient one. `consecutive == 1` (see [`AlarmAction::halt_immediately`]) restores single-check halt and is
//! only safe when `check` is invoked at quiescent points with no in-flight orders.
//!
//! It is the *detector*, not the *explainer*: attribution of *why* a divergence happened is QE-302.

use rust_decimal::Decimal;

use qe_domain::Direction;
use qe_risk::{KillHandle, KillSwitch};
use qe_venue::userdata::PositionReport;

/// What to do when a check diverges beyond tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmAction {
    /// Raise the alarm only — a human/operator decides. The kill is **never** tripped.
    AlarmOnly,
    /// Raise the alarm and trip the QE-216 kill once the divergence has persisted for `consecutive`
    /// consecutive beyond-tolerance checks (debounce; treated as `≥ 1`). A single reconciled check in between
    /// resets the streak, so a one-period report/fill skew does not halt — only a *sustained* desync does.
    HaltAfter {
        /// Consecutive beyond-tolerance checks required before halting.
        consecutive: u32,
    },
}

impl AlarmAction {
    /// Halt on the **first** beyond-tolerance check. Only safe when `check` runs at quiescent points with no
    /// in-flight orders (otherwise routine report/fill skew would false-halt — prefer a `HaltAfter { ≥ 2 }`).
    #[must_use]
    pub const fn halt_immediately() -> Self {
        Self::HaltAfter { consecutive: 1 }
    }
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
    /// How many consecutive checks (including this one) have diverged — the debounce streak.
    pub consecutive: u32,
    /// Whether this divergence tripped the kill (`true` only once the `HaltAfter` streak is reached).
    pub halted: bool,
}

/// The outcome of one reconciliation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconOutcome {
    /// Within tolerance — nothing raised, and the divergence streak is reset.
    Reconciled,
    /// Beyond tolerance — an alarm was raised (and the kill tripped once the `HaltAfter` streak is reached).
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
/// and, once a divergence beyond tolerance **persists**, alarms and (under `HaltAfter`) trips the QE-216 kill.
pub struct ReconciliationGuard {
    /// Absolute divergence tolerance in contracts (clamped `≥ 0`).
    tolerance: Decimal,
    action: AlarmAction,
    /// A clone of the QE-216 kill handle — trippable out-of-band on a sustained divergence.
    kill: KillHandle,
    /// How many beyond-tolerance alarms have been raised (counts every breach, from the first).
    alarms: u64,
    /// Consecutive beyond-tolerance checks so far — reset by any reconciled check (the debounce).
    streak: u32,
}

impl ReconciliationGuard {
    /// A guard tripping `kill` per `action` on divergence beyond `tolerance` (absolute contracts, clamped
    /// `≥ 0` so a mis-configured negative bound fails safe — it alarms more, never less).
    #[must_use]
    pub fn new(tolerance: Decimal, action: AlarmAction, kill: KillHandle) -> Self {
        Self {
            tolerance: tolerance.max(Decimal::ZERO),
            action,
            kill,
            alarms: 0,
            streak: 0,
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

    /// How many beyond-tolerance alarms this guard has raised (every breach, from the first).
    #[must_use]
    pub fn alarms(&self) -> u64 {
        self.alarms
    }

    /// The current consecutive-breach streak (0 after any reconciled check).
    #[must_use]
    pub fn consecutive_breaches(&self) -> u32 {
        self.streak
    }

    /// Reconcile the runtime's `expected` signed position against the venue's authoritative `venue`
    /// [`PositionReport`]. See [`check_qty`](Self::check_qty).
    pub fn check(&mut self, expected: Decimal, venue: &PositionReport) -> ReconOutcome {
        self.check_qty(expected, signed_qty(venue))
    }

    /// Reconcile `expected` against an already-signed `venue_qty`. Within tolerance → [`Reconciled`] (and the
    /// streak resets); beyond it → an **alarm** (`alarms += 1`, streak `+= 1`) and, under
    /// [`AlarmAction::HaltAfter`], the kill is tripped once the streak reaches `consecutive`. `expected` must
    /// come from the caller's own accounting, **not** the keeper the venue report already updated, or the
    /// check is circular.
    ///
    /// [`Reconciled`]: ReconOutcome::Reconciled
    pub fn check_qty(&mut self, expected: Decimal, venue_qty: Decimal) -> ReconOutcome {
        let delta = (expected - venue_qty).abs();
        if delta <= self.tolerance {
            self.streak = 0; // a reconciled check clears the debounce
            return ReconOutcome::Reconciled;
        }
        self.streak += 1;
        self.alarms += 1;
        let halted = match self.action {
            AlarmAction::AlarmOnly => false,
            // A sustained divergence: trip only once the streak reaches the (≥ 1) threshold.
            AlarmAction::HaltAfter { consecutive } => self.streak >= consecutive.max(1),
        };
        if halted {
            self.kill.trip(&format!(
                "reconciliation divergence: expected {expected}, venue {venue_qty}, |Δ| {delta} > tol {} \
                 for {} consecutive checks",
                self.tolerance, self.streak
            ));
        }
        ReconOutcome::Diverged(Divergence {
            expected,
            venue: venue_qty,
            delta,
            consecutive: self.streak,
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

    /// AC: a *sustained* desync beyond tolerance raises alarms and halts once the debounce threshold is met.
    #[test]
    fn sustained_divergence_alarms_and_halts_after_threshold() {
        let mut g = guard("0.1", AlarmAction::HaltAfter { consecutive: 2 });

        // First breach: alarmed, streak 1, not yet halted.
        match g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.delta, dec("0.2"));
                assert_eq!(d.consecutive, 1);
                assert!(!d.halted, "one breach must not halt under HaltAfter{{2}}");
            }
            other => panic!("expected divergence, got {other:?}"),
        }
        assert!(!g.kill().is_tripped());

        // Second consecutive breach: streak 2 → halts.
        match g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.consecutive, 2);
                assert!(d.halted, "a sustained divergence halts at the threshold");
            }
            other => panic!("expected divergence, got {other:?}"),
        }
        assert!(
            g.kill().is_tripped(),
            "the sustained divergence trips the kill"
        );
        assert_eq!(g.alarms(), 2);
    }

    /// F1 regression: a one-period skew (a breach that clears on the next check) must **not** halt — the
    /// standard in-flight-order false-halt the debounce exists to prevent.
    #[test]
    fn transient_single_period_skew_does_not_halt() {
        let mut g = guard("0.1", AlarmAction::HaltAfter { consecutive: 2 });

        // Breach (order in flight): streak 1.
        assert!(matches!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")),
            ReconOutcome::Diverged(_)
        ));
        assert_eq!(g.consecutive_breaches(), 1);

        // Next check reconciles (the fill was reported): streak resets.
        assert_eq!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.5")),
            ReconOutcome::Reconciled
        );
        assert_eq!(g.consecutive_breaches(), 0);

        // Another isolated breach: streak back to 1, still below the threshold.
        assert!(matches!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")),
            ReconOutcome::Diverged(_)
        ));
        assert!(
            !g.kill().is_tripped(),
            "isolated one-period skews separated by a reconcile never reach the streak → no false-halt"
        );
        assert_eq!(g.alarms(), 2, "both breaches still alarm for observability");
    }

    /// `halt_immediately()` (`HaltAfter{1}`) halts on the first breach — the quiescent-point mode.
    #[test]
    fn immediate_halt_trips_on_first_breach() {
        let mut g = guard("0.1", AlarmAction::halt_immediately());
        match g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")) {
            ReconOutcome::Diverged(d) => assert!(d.halted),
            other => panic!("expected divergence, got {other:?}"),
        }
        assert!(g.kill().is_tripped());
    }

    /// Within tolerance reconciles silently — no alarm, nothing tripped, streak stays 0.
    #[test]
    fn within_tolerance_reconciles_without_alarm() {
        let mut g = guard("0.1", AlarmAction::halt_immediately());
        assert_eq!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.45")),
            ReconOutcome::Reconciled
        );
        assert!(!g.kill().is_tripped());
        assert_eq!(g.alarms(), 0);
        assert_eq!(g.consecutive_breaches(), 0);
    }

    /// The tolerance bound is inclusive: an exact-tolerance delta reconciles.
    #[test]
    fn exactly_at_tolerance_reconciles() {
        let mut g = guard("0.2", AlarmAction::halt_immediately());
        // |0.5 − 0.3| == 0.2 == tolerance.
        assert_eq!(
            g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")),
            ReconOutcome::Reconciled
        );
        assert!(!g.kill().is_tripped());
    }

    /// Alarm-only mode alarms on every breach but never halts, even when sustained.
    #[test]
    fn alarm_only_never_halts() {
        let mut g = guard("0.1", AlarmAction::AlarmOnly);
        for _ in 0..3 {
            match g.check(dec("0.5"), &report(Some(Direction::Long), "0.3")) {
                ReconOutcome::Diverged(d) => assert!(!d.halted),
                other => panic!("expected divergence, got {other:?}"),
            }
        }
        assert!(!g.kill().is_tripped(), "AlarmOnly never trips the kill");
        assert_eq!(g.alarms(), 3);
    }

    /// A sign flip (expected long, venue short) is a divergence of the summed magnitudes — a severe desync.
    #[test]
    fn sign_flip_is_a_divergence() {
        let mut g = guard("0.1", AlarmAction::halt_immediately());
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
        let mut g = guard("0.1", AlarmAction::halt_immediately());
        match g.check(dec("0.3"), &report(None, "0")) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.venue, dec("0"));
                assert_eq!(d.delta, dec("0.3"));
            }
            other => panic!("expected divergence, got {other:?}"),
        }
    }

    /// The latched kill preserves the first triggering reason (which records the streak length).
    #[test]
    fn halt_latches_first_reason() {
        let mut g = guard("0.1", AlarmAction::halt_immediately());
        g.check(dec("0.5"), &report(Some(Direction::Long), "0.3"));
        g.check(dec("0.9"), &report(Some(Direction::Long), "0.1"));
        assert_eq!(g.alarms(), 2);
        let reason = g.kill().reason().expect("a reason is latched");
        assert!(
            reason.contains("expected 0.5"),
            "first reason wins: {reason}"
        );
    }

    /// A negative tolerance is clamped to zero (fails safe: any nonzero delta alarms).
    #[test]
    fn negative_tolerance_clamped_to_zero() {
        let mut g = guard("-1", AlarmAction::AlarmOnly);
        assert_eq!(g.tolerance(), dec("0"));
        assert_eq!(
            g.check_qty(dec("0.2"), dec("0.2")),
            ReconOutcome::Reconciled
        );
        assert!(matches!(
            g.check_qty(dec("0.2"), dec("0.19")),
            ReconOutcome::Diverged(_)
        ));
    }
}
