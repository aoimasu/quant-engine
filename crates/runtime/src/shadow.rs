//! GATE G2 — live shadow / dry-run (QE-222).
//!
//! Before any capital, the full loop runs against live data computing **would-be** orders with **no
//! submission**, reconciled against the simulator — catching wss-stitch, mark-EMA, netting, and cutover bugs.
//!
//! - [`ShadowGateway`] is the Edge gateway in **dry-run**: it runs the same `plan_delta` the live edge does
//!   but **logs** the resulting order and advances a shadow position *as-if-filled* instead of submitting.
//!   It has no submit path at all — `orders_submitted()` is a literal `0`.
//! - [`ShadowRun`] is the gate: it drives each [`TargetRevision`] through both the shadow gateway **and** a
//!   submitting QE-218 [`PlannerAdapterLink`] (the simulator expectation), reconciles the two positions with
//!   the QE-221 [`ReconciliationGuard`] (in `AlarmOnly` — a dry-run reports, it does not halt), and reports
//!   whether the run reconciled within tolerance with nothing submitted.
//!
//! Both paths share `plan_delta`, so the happy path agrees exactly; the gate's value is that **any** pipeline
//! discrepancy makes the shadow position diverge from the simulator and the guard reports it.

use rust_decimal::Decimal;

use qe_domain::{InstrumentId, Notional, Price, Qty, Side};
use qe_risk::KillHandle;

use crate::edge::{plan_delta, VenueKeeper, VenueSimulator};
use crate::kill_gate::VenueKillGate;
use crate::reconciliation::{AlarmAction, ReconOutcome, ReconciliationGuard};
use crate::transport::{PlannerAdapterLink, TargetRevision};

/// An order the dry-run edge **would** have submitted — logged, not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WouldBeOrder {
    /// The target revision that produced it.
    pub seq: u64,
    /// Buy (increase) or sell (decrease).
    pub side: Side,
    /// The delta quantity (always positive).
    pub qty: Qty,
    /// Venue event time (epoch ms).
    pub event_time_ms: i64,
}

/// The Edge gateway in **dry-run**: computes the order it *would* submit for each target and logs it, advances
/// a shadow position as-if-filled, and submits **nothing**.
pub struct ShadowGateway {
    mark: Price,
    /// The position the shadow would hold if its would-be orders had filled (signed contracts).
    shadow_qty: Decimal,
    would_be: Vec<WouldBeOrder>,
}

impl ShadowGateway {
    /// A flat dry-run gateway marked at `mark`.
    #[must_use]
    pub fn new(mark: Price) -> Self {
        Self {
            mark,
            shadow_qty: Decimal::ZERO,
            would_be: Vec::new(),
        }
    }

    /// Feed the latest mark price (venue truth).
    pub fn observe_mark(&mut self, mark: Price) {
        self.mark = mark;
    }

    /// The shadow position (signed contracts) the logged would-be orders would have produced.
    #[must_use]
    pub fn shadow_position(&self) -> Decimal {
        self.shadow_qty
    }

    /// The would-be orders logged so far.
    #[must_use]
    pub fn would_be_orders(&self) -> &[WouldBeOrder] {
        &self.would_be
    }

    /// Real orders submitted — always `0`: the dry-run edge has no submit path.
    #[must_use]
    pub fn orders_submitted(&self) -> u64 {
        0
    }

    /// Compute the would-be order for `rev` (the same `plan_delta` the live edge runs), **log** it, and
    /// advance the shadow position as-if-filled — submitting nothing. `None` when already at target.
    pub fn observe(&mut self, rev: &TargetRevision) -> Option<&WouldBeOrder> {
        let intent = plan_delta(rev.target.notional, self.shadow_qty, self.mark)?;
        match intent.side {
            Side::Buy => self.shadow_qty += intent.qty.get(),
            Side::Sell => self.shadow_qty -= intent.qty.get(),
        }
        self.would_be.push(WouldBeOrder {
            seq: rev.seq,
            side: intent.side,
            qty: intent.qty,
            event_time_ms: rev.event_time_ms,
        });
        self.would_be.last()
    }
}

/// A summary of a shadow run — the G2 gate evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowReport {
    /// How many would-be orders the dry-run edge logged.
    pub would_be_orders: usize,
    /// The shadow (would-be) position at the end of the run.
    pub shadow_position: Decimal,
    /// The reference simulator's position at the end of the run.
    pub sim_position: Decimal,
    /// Real orders the dry-run edge submitted — must be `0`.
    pub orders_submitted: u64,
    /// Whether every step stayed within tolerance (shadow reconciled with the simulator throughout).
    pub reconciled: bool,
    /// The largest shadow-vs-simulator divergence seen during the run.
    pub max_divergence: Decimal,
}

/// The G2 gate: drives a target stream through the dry-run edge and a submitting reference simulator, and
/// reconciles the two.
pub struct ShadowRun {
    shadow: ShadowGateway,
    reference: PlannerAdapterLink,
    guard: ReconciliationGuard,
    reconciled: bool,
    max_divergence: Decimal,
}

impl ShadowRun {
    /// A gate for `instrument` marked at `mark`, reconciling within `tolerance` (absolute contracts). The
    /// reference is a fresh submitting simulator; the guard is `AlarmOnly` — a dry-run reports, never halts.
    #[must_use]
    pub fn new(instrument: InstrumentId, mark: Price, tolerance: Decimal) -> Self {
        let mut keeper =
            VenueKeeper::new(instrument.clone(), Notional::new(Decimal::from(1_000_000)));
        keeper.observe_mark(mark);
        let gate = VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument));
        Self {
            shadow: ShadowGateway::new(mark),
            reference: PlannerAdapterLink::new(keeper, gate),
            guard: ReconciliationGuard::new(tolerance, AlarmAction::AlarmOnly, KillHandle::new()),
            reconciled: true,
            max_divergence: Decimal::ZERO,
        }
    }

    /// Feed the latest mark to **both** the shadow edge and the reference keeper (venue truth).
    pub fn observe_mark(&mut self, mark: Price) {
        self.shadow.observe_mark(mark);
        self.reference.observe_mark(mark);
    }

    /// Drive one target revision through both paths and reconcile: the shadow logs a would-be order and
    /// advances its shadow position; the reference submits to the simulator and advances the keeper; the guard
    /// compares the two positions, tracking whether the run stayed within tolerance and the max divergence.
    pub fn observe(&mut self, rev: &TargetRevision) {
        self.shadow.observe(rev);
        self.reference
            .submit_target(*rev)
            .expect("the reference link is connected");
        self.reference.pump();

        let shadow_q = self.shadow.shadow_position();
        let sim_q = self.reference.keeper().signed_qty();
        if let ReconOutcome::Diverged(d) = self.guard.check_qty(shadow_q, sim_q) {
            self.reconciled = false;
            self.max_divergence = self.max_divergence.max(d.delta);
        }
    }

    /// The would-be orders logged by the dry-run edge.
    #[must_use]
    pub fn would_be_orders(&self) -> &[WouldBeOrder] {
        self.shadow.would_be_orders()
    }

    /// How many real orders the **reference** simulator submitted (proves the reference actually traded).
    #[must_use]
    pub fn reference_orders_submitted(&self) -> u64 {
        self.reference.orders_submitted()
    }

    /// The gate evidence for this run.
    #[must_use]
    pub fn report(&self) -> ShadowReport {
        ShadowReport {
            would_be_orders: self.shadow.would_be_orders().len(),
            shadow_position: self.shadow.shadow_position(),
            sim_position: self.reference.keeper().signed_qty(),
            orders_submitted: self.shadow.orders_submitted(),
            reconciled: self.reconciled,
            max_divergence: self.max_divergence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hedger::TargetPosition;
    use crate::transport::AdapterReport;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn price(s: &str) -> Price {
        Price::new(dec(s)).unwrap()
    }
    fn instrument() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }
    fn rev(seq: u64, target: &str, t: i64) -> TargetRevision {
        TargetRevision {
            seq,
            target: TargetPosition {
                notional: Notional::new(dec(target)),
            },
            event_time_ms: t,
        }
    }

    /// A defined "live period" of absolute targets at mark 50 000: flat → +10 000 → +20 000 → +5 000 → flat.
    fn period() -> Vec<TargetRevision> {
        vec![
            rev(0, "10000", 1),
            rev(1, "20000", 2),
            rev(2, "5000", 3),
            rev(3, "0", 4),
        ]
    }

    /// AC: a shadow run reconciles with the simulator within tolerance and submits nothing.
    #[test]
    fn shadow_run_reconciles_with_simulator_and_submits_nothing() {
        let mut run = ShadowRun::new(instrument(), price("50000"), dec("0.0001"));
        for r in period() {
            run.observe(&r);
        }
        let report = run.report();

        assert!(
            report.reconciled,
            "shadow reconciled with the simulator throughout"
        );
        assert_eq!(
            report.max_divergence,
            dec("0"),
            "no divergence in the happy path"
        );
        assert_eq!(
            report.orders_submitted, 0,
            "the dry-run edge submits nothing"
        );
        assert!(report.would_be_orders > 0, "would-be orders were produced");
        assert_eq!(
            report.shadow_position, report.sim_position,
            "shadow position matches the simulator"
        );
        // The reference is a genuine trading path — a vacuous all-flat pass is ruled out.
        assert!(
            run.reference_orders_submitted() > 0,
            "the reference simulator actually traded"
        );
    }

    /// Each would-be order matches the simulator's corresponding fill exactly (same side and quantity).
    #[test]
    fn would_be_orders_match_simulator_fills() {
        let mut shadow = ShadowGateway::new(price("50000"));
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("1000000")));
        keeper.observe_mark(price("50000"));
        let mut link = PlannerAdapterLink::new(
            keeper,
            VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument())),
        );
        link.observe_mark(price("50000"));

        for r in period() {
            let would_be = shadow.observe(&r).cloned();
            link.submit_target(r).unwrap();
            let fill = link.pump().into_iter().find_map(|report| match report {
                AdapterReport::Fill(f) => Some(f),
                _ => None,
            });

            match (would_be, fill) {
                (Some(w), Some(f)) => {
                    assert_eq!(w.side, f.order.side, "would-be side matches the fill");
                    assert_eq!(w.qty, f.order.qty, "would-be qty matches the fill");
                }
                (None, None) => {} // both at target → no order either side
                (w, f) => panic!("would-be / fill mismatch: {w:?} vs {f:?}"),
            }
        }
    }

    /// A revision already at the shadow position logs no would-be order.
    #[test]
    fn at_target_revision_logs_no_would_be_order() {
        let mut shadow = ShadowGateway::new(price("50000"));
        assert!(shadow.observe(&rev(0, "10000", 1)).is_some()); // flat → long: an order
        assert!(
            shadow.observe(&rev(1, "10000", 2)).is_none(),
            "already at target → no would-be order"
        );
        assert_eq!(shadow.would_be_orders().len(), 1);
        assert_eq!(shadow.orders_submitted(), 0);
    }

    /// The gate bites: a pipeline discrepancy (the shadow sees a stale mark, the reference the fresh one)
    /// makes the shadow position diverge from the simulator, and the reconciliation flags it.
    #[test]
    fn reconciliation_catches_a_pipeline_divergence() {
        // Shadow prices the same +10 000 target at a stale mark (40 000 → 0.25), the reference at 50 000 → 0.2.
        let mut shadow = ShadowGateway::new(price("40000"));
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("1000000")));
        keeper.observe_mark(price("50000"));
        let mut link = PlannerAdapterLink::new(
            keeper,
            VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument())),
        );
        link.observe_mark(price("50000"));
        let mut guard =
            ReconciliationGuard::new(dec("0.0001"), AlarmAction::AlarmOnly, KillHandle::new());

        let target = rev(0, "10000", 1);
        shadow.observe(&target);
        link.submit_target(target).unwrap();
        link.pump();

        let shadow_q = shadow.shadow_position(); // 10000 / 40000 = 0.25
        let sim_q = link.keeper().signed_qty(); // 10000 / 50000 = 0.20
        assert_ne!(shadow_q, sim_q, "the stale mark makes the sizings differ");

        match guard.check_qty(shadow_q, sim_q) {
            ReconOutcome::Diverged(d) => {
                assert_eq!(d.delta, dec("0.05"), "0.25 vs 0.20");
            }
            other => panic!("the gate must catch this divergence, got {other:?}"),
        }
        assert_eq!(guard.alarms(), 1, "the reconciliation raised an alarm");
    }
}
