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

    /// Feed the latest mark to **both** the shadow edge and the reference keeper — the aligned case, where the
    /// live pipeline's mark equals venue truth. Shorthand for `observe_marks(mark, mark)`.
    pub fn observe_mark(&mut self, mark: Price) {
        self.observe_marks(mark, mark);
    }

    /// Feed the shadow edge (`shadow_mark`, as the **live pipeline under test** computes it) and the reference
    /// keeper (`reference_mark`, **venue truth**) marks **independently**. When the two differ — a mark-EMA
    /// drift, a stitched/duplicated bar, a stale tick — the shadow sizes its would-be orders differently from
    /// the simulator, and a subsequent [`observe`](Self::observe) diverges: exactly the pipeline fault this
    /// gate exists to catch, reachable through the gate's own API. `observe_mark` is the aligned shorthand.
    pub fn observe_marks(&mut self, shadow_mark: Price, reference_mark: Price) {
        self.shadow.observe_mark(shadow_mark);
        self.reference.observe_mark(reference_mark);
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
    use crate::transport::AdapterReport;
    use qe_runtime_core::TargetPosition;
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
            target: TargetPosition::single(Notional::new(dec(target))),
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

    /// The gate's **red state is reachable through its own API** and reports a real pipeline divergence: when
    /// the shadow's mark pipeline (live-data-derived) drifts from venue truth, the shadow sizes its would-be
    /// order differently from the simulator and `ShadowRun` reports `reconciled == false`. This is the fault
    /// the capital-blocking gate exists to catch — driven end-to-end through `ShadowRun`, not a hand-wired guard.
    #[test]
    fn gate_reports_a_mark_pipeline_divergence_through_shadow_run() {
        let mut run = ShadowRun::new(instrument(), price("50000"), dec("0.0001"));
        // The shadow's mark pipeline drifts to 40 000 (a mark-EMA / stale-tick bug) while venue truth is 50 000.
        run.observe_marks(price("40000"), price("50000"));
        run.observe(&rev(0, "10000", 1));

        let report = run.report();
        // shadow: 10000 / 40000 = 0.25; simulator: 10000 / 50000 = 0.20.
        assert!(
            !report.reconciled,
            "the gate must report the mark-pipeline divergence"
        );
        assert_eq!(report.max_divergence, dec("0.05"), "0.25 vs 0.20");
        assert!(
            report.max_divergence > dec("0.0001"),
            "the divergence exceeds tolerance"
        );
        assert_eq!(report.shadow_position, dec("0.25"));
        assert_eq!(report.sim_position, dec("0.2"));
        // Even on the fail path, the dry-run edge still submits nothing.
        assert_eq!(report.orders_submitted, 0);
    }

    /// `reconciled` is a **run-level latch**: once any step diverges, the run is flagged red for the whole run
    /// even if a later step re-converges. A transient fault must not be forgotten by the gate — a go/no-go
    /// reviewer reads a single verdict for the period, so one bad step condemns the run.
    #[test]
    fn a_divergence_latches_the_run_red() {
        let mut run = ShadowRun::new(instrument(), price("50000"), dec("0.0001"));
        run.observe_marks(price("40000"), price("50000"));
        run.observe(&rev(0, "10000", 1)); // diverge: shadow 0.25 vs sim 0.20

        // The mark realigns and the next rebalance re-converges both to 0.20 — a *reconciled* step …
        run.observe_mark(price("50000"));
        run.observe(&rev(1, "10000", 2));

        // … yet the run stays red: the earlier divergence is latched, and max_divergence holds its peak.
        let report = run.report();
        assert!(
            !report.reconciled,
            "one diverged step latches the whole run red"
        );
        assert_eq!(
            report.max_divergence,
            dec("0.05"),
            "the peak divergence is retained"
        );
        assert_eq!(
            report.shadow_position,
            dec("0.2"),
            "positions did re-converge"
        );
        assert_eq!(report.sim_position, dec("0.2"));
    }
}
