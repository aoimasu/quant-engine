//! gRPC transport between the Hedge Planner and the Edge gateway (QE-218).
//!
//! Decisions flow **planner → adapter** as absolute [`TargetRevision`]s; **fills, position reports, and
//! heartbeat/venue-health** flow back as [`AdapterReport`]s. The real tonic/gRPC bidi stream is a thin
//! adapter in the runtime binary (deferred, exactly as QE-202's real websocket is); QE-218's tested core is
//! the transport **semantics** — message model, **backpressure**, **reconnection**, and the guarantee that
//! the QE-301 journal-append path **never gates** the dispatch — modelled single-threaded and pull-based via
//! [`PlannerAdapterLink`], with no `tokio`/`tonic`/`prost` and no new workspace dependency.
//!
//! Two properties of QE-214's **absolute** [`TargetPosition`] carry the design:
//! - **Backpressure = coalesce-to-latest** ([`PlannerAdapterLink::submit_target`]): the send queue holds at
//!   most one revision; a newer one supersedes an unsent older one. Lossless, because a superseded absolute
//!   target carries nothing the latest lacks.
//! - **Reconnection = re-snapshot + re-send latest** ([`PlannerAdapterLink::reconnect`]): re-sending the
//!   latest absolute target after a reconnect is exactly idempotent — `plan_delta` against the authoritative
//!   kept position yields a zero delta, so the position is never doubled.

use qe_venue::userdata::PositionReport;

use crate::edge::{plan_delta, SimFill, VenueKeeper};
use crate::hedger::TargetPosition;
use crate::kill_gate::VenueKillGate;
use qe_risk::KillHandle;

/// A monotonic, **absolute** target revision the planner emits over the transport. A later [`seq`](Self::seq)
/// supersedes an earlier one; the mark is not carried (it is venue truth held by the [`VenueKeeper`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetRevision {
    /// The planner's monotonic revision number.
    pub seq: u64,
    /// The absolute, signed target position.
    pub target: TargetPosition,
    /// Venue event time to stamp resulting fills/reports (epoch ms).
    pub event_time_ms: i64,
}

/// Venue-health carried on the heartbeat back-channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueHealth {
    /// The venue is accepting orders normally.
    Ok,
    /// The venue is reachable but impaired (reason).
    Degraded(String),
    /// Submission is halted (reason) — e.g. the QE-216 kill switch is tripped.
    Down(String),
}

/// One report the adapter streams back to the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterReport {
    /// A venue-confirmed fill for a dispatched delta.
    Fill(SimFill),
    /// An authoritative position report (venue truth).
    Position(PositionReport),
    /// A heartbeat: the last applied revision `seq` (if any) plus current venue health.
    Heartbeat {
        /// The `seq` of the revision this pump applied, if one was applied.
        ack_seq: Option<u64>,
        /// Current venue health.
        health: VenueHealth,
        /// Venue event time (epoch ms).
        event_time_ms: i64,
    },
}

/// Why a transport call could not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The link is disconnected — the planner must await [`reconnect`](PlannerAdapterLink::reconnect).
    Disconnected,
}

/// The QE-301 journal append failed. Non-gating: it can never change what the planner receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendError(pub String);

/// The QE-301 journal-append seam. The dispatcher **offers** the already-produced reports to the sink; its
/// result is recorded but **cannot alter the dispatch** — that is the AC's "append never gates dispatch".
pub trait AppendSink {
    /// Append `rev` + its `reports` to the journal. A returned `Err` is counted, never propagated to the
    /// planner.
    ///
    /// # Errors
    /// [`AppendError`] when the (future) journal write fails; the transport ignores it structurally.
    fn append(
        &mut self,
        rev: &TargetRevision,
        reports: &[AdapterReport],
    ) -> Result<(), AppendError>;
}

/// The default sink: journalling is not wired yet (QE-301). Accepts everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAppendSink;

impl AppendSink for NullAppendSink {
    fn append(
        &mut self,
        _rev: &TargetRevision,
        _reports: &[AdapterReport],
    ) -> Result<(), AppendError> {
        Ok(())
    }
}

/// The in-process model of the planner ↔ adapter gRPC bidi stream (single-threaded, pull-based). Owns the
/// adapter state — the authoritative [`VenueKeeper`] and the kill-gated [`VenueKillGate`] — plus the
/// transport state (a coalescing send queue, the retained latest revision, and the connection flag).
pub struct PlannerAdapterLink<A: AppendSink = NullAppendSink> {
    keeper: VenueKeeper,
    gate: VenueKillGate,
    /// The coalescing send queue: at most one pending revision (backpressure, D4).
    pending: Option<TargetRevision>,
    /// The latest revision the adapter applied — retained so the planner can re-send it on reconnect (D5).
    last_applied: Option<TargetRevision>,
    connected: bool,
    append: A,
    append_failures: u64,
    dropped_superseded: u64,
}

impl PlannerAdapterLink<NullAppendSink> {
    /// A connected link over `keeper` + `gate`, with journalling not yet wired ([`NullAppendSink`]).
    #[must_use]
    pub fn new(keeper: VenueKeeper, gate: VenueKillGate) -> Self {
        Self::with_append(keeper, gate, NullAppendSink)
    }
}

impl<A: AppendSink> PlannerAdapterLink<A> {
    /// A connected link over `keeper` + `gate`, journalling to `append`.
    #[must_use]
    pub fn with_append(keeper: VenueKeeper, gate: VenueKillGate, append: A) -> Self {
        Self {
            keeper,
            gate,
            pending: None,
            last_applied: None,
            connected: true,
            append,
            append_failures: 0,
            dropped_superseded: 0,
        }
    }

    /// The authoritative position keeper (read-only).
    #[must_use]
    pub fn keeper(&self) -> &VenueKeeper {
        &self.keeper
    }

    /// The keeper, mutable — the market/account streams (QE-204/208) feed mark + balances through here; the
    /// transport only reads it for `plan_delta`.
    pub fn keeper_mut(&mut self) -> &mut VenueKeeper {
        &mut self.keeper
    }

    /// The held kill handle (clone it to trip the out-of-band halt, QE-216).
    #[must_use]
    pub fn kill(&self) -> &KillHandle {
        self.gate.kill()
    }

    /// The latest revision the adapter applied, if any (what the planner re-sends on reconnect).
    #[must_use]
    pub fn latest_target(&self) -> Option<TargetRevision> {
        self.last_applied
    }

    /// Whether the transport is connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// How many superseded revisions the backpressure coalescing dropped.
    #[must_use]
    pub fn dropped_superseded(&self) -> u64 {
        self.dropped_superseded
    }

    /// How many append offers the journal sink rejected (non-gating).
    #[must_use]
    pub fn append_failures(&self) -> u64 {
        self.append_failures
    }

    /// The underlying simulator (read-only): order count / sim position for accounting.
    #[must_use]
    pub fn orders_submitted(&self) -> u64 {
        self.gate.simulator().orders_submitted()
    }

    /// Planner → transport: enqueue an absolute target revision.
    ///
    /// **Backpressure (D4):** the queue holds at most one revision; if one was already pending it is dropped
    /// (`dropped_superseded += 1`) — lossless, because the newer absolute target subsumes it.
    ///
    /// # Errors
    /// [`TransportError::Disconnected`] while the link is disconnected (nothing is enqueued).
    pub fn submit_target(&mut self, rev: TargetRevision) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        if self.pending.replace(rev).is_some() {
            self.dropped_superseded += 1;
        }
        Ok(())
    }

    /// Adapter server tick: apply the pending revision (if any) to the edge and return the resulting reports
    /// (fills + authoritative position + heartbeat). Returns empty when disconnected or nothing is pending.
    ///
    /// The reports are produced **before** the journal sink is offered them, and the sink's result cannot
    /// change them — so the QE-301 append path never gates this dispatch (D2, AC).
    pub fn pump(&mut self) -> Vec<AdapterReport> {
        if !self.connected {
            return Vec::new();
        }
        let Some(rev) = self.pending.take() else {
            return Vec::new();
        };
        self.last_applied = Some(rev);
        let reports = self.apply_revision(rev);
        // Offer the already-produced reports to the journal. A failure is counted, never propagated.
        if self.append.append(&rev, &reports).is_err() {
            self.append_failures += 1;
        }
        reports
    }

    /// Disconnect the transport: `submit_target` errors and `pump` is inert until [`reconnect`].
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// Reconnect: re-establish position truth and return an authoritative [`AdapterReport::Position`]
    /// snapshot. The planner then re-sends [`latest_target`](Self::latest_target); because the target is
    /// absolute and `plan_delta` reads the authoritative kept position, that re-send is idempotent — a
    /// reconnect never doubles the position (D5).
    pub fn reconnect(&mut self) -> Vec<AdapterReport> {
        self.connected = true;
        let event_time_ms = self.last_applied.map_or(0, |r| r.event_time_ms);
        vec![AdapterReport::Position(
            self.gate.simulator().position_report(event_time_ms),
        )]
    }

    /// Apply one revision to the edge: translate to a delta vs the kept position, submit through the kill
    /// gate, absorb the fill, and assemble the return reports.
    fn apply_revision(&mut self, rev: TargetRevision) -> Vec<AdapterReport> {
        let mark = self.keeper.mark();
        let current = self.keeper.signed_qty();
        let mut reports = Vec::new();
        let mut health = VenueHealth::Ok;

        if let Some(intent) = plan_delta(rev.target.notional, current, mark) {
            match self.gate.submit(intent, mark, rev.event_time_ms) {
                Ok(fill) => {
                    self.keeper.apply(&fill.event);
                    reports.push(AdapterReport::Fill(fill));
                }
                // QE-216 kill tripped: submission is halted on the wire — no fill, health Down.
                Err(halt) => health = VenueHealth::Down(halt.reason),
            }
        }

        reports.push(AdapterReport::Position(
            self.gate.simulator().position_report(rev.event_time_ms),
        ));
        reports.push(AdapterReport::Heartbeat {
            ack_seq: Some(rev.seq),
            health,
            event_time_ms: rev.event_time_ms,
        });
        reports
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::VenueSimulator;
    use qe_domain::{Direction, InstrumentId, Notional, Price, Qty, Side};
    use qe_risk::KillSwitch;
    use rust_decimal::Decimal;
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

    /// A link over a keeper marked at `mark_px` with `equity`, and a kill-gated simulator.
    fn link_at(mark_px: &str, equity: &str) -> PlannerAdapterLink {
        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec(equity)));
        keeper.observe_mark(price(mark_px));
        let gate = VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument()));
        PlannerAdapterLink::new(keeper, gate)
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

    fn find_fill(reports: &[AdapterReport]) -> Option<&SimFill> {
        reports.iter().find_map(|r| match r {
            AdapterReport::Fill(f) => Some(f),
            _ => None,
        })
    }
    fn find_position(reports: &[AdapterReport]) -> Option<&PositionReport> {
        reports.iter().find_map(|r| match r {
            AdapterReport::Position(p) => Some(p),
            _ => None,
        })
    }
    fn heartbeat(reports: &[AdapterReport]) -> (Option<u64>, VenueHealth) {
        reports
            .iter()
            .find_map(|r| match r {
                AdapterReport::Heartbeat {
                    ack_seq, health, ..
                } => Some((*ack_seq, health.clone())),
                _ => None,
            })
            .expect("a heartbeat is always reported")
    }

    /// A sink that always fails — to prove the append path never gates dispatch.
    struct FailingAppendSink {
        calls: u64,
    }
    impl AppendSink for FailingAppendSink {
        fn append(
            &mut self,
            _rev: &TargetRevision,
            _reports: &[AdapterReport],
        ) -> Result<(), AppendError> {
            self.calls += 1;
            Err(AppendError("journal unavailable".to_owned()))
        }
    }

    /// AC: a target revision reaches the adapter and fills + positions return.
    #[test]
    fn target_revision_reaches_adapter_and_fills_return() {
        let mut link = link_at("50000", "100000");
        link.submit_target(rev(0, "10000", 1)).unwrap();

        let reports = link.pump();

        let fill = find_fill(&reports).expect("a fill returns");
        assert_eq!(fill.order.side, Side::Buy);
        assert_eq!(fill.order.qty, Qty::new(dec("0.2")).unwrap());

        let pos = find_position(&reports).expect("a position report returns");
        assert_eq!(pos.direction, Some(Direction::Long));
        assert_eq!(pos.qty, Qty::new(dec("0.2")).unwrap());

        assert_eq!(heartbeat(&reports), (Some(0), VenueHealth::Ok));
        assert_eq!(link.keeper().signed_qty(), dec("0.2"));
    }

    /// AC: the QE-301 append path never gates dispatch — a failing sink yields the identical reports as the
    /// null sink, and the failure is merely counted.
    #[test]
    fn append_never_gates_dispatch() {
        let baseline = {
            let mut link = link_at("50000", "100000");
            link.submit_target(rev(0, "10000", 1)).unwrap();
            link.pump()
        };

        let mut keeper = VenueKeeper::new(instrument(), Notional::new(dec("100000")));
        keeper.observe_mark(price("50000"));
        let gate = VenueKillGate::new(KillHandle::new(), VenueSimulator::new(instrument()));
        let mut link =
            PlannerAdapterLink::with_append(keeper, gate, FailingAppendSink { calls: 0 });
        link.submit_target(rev(0, "10000", 1)).unwrap();

        let reports = link.pump();
        assert_eq!(
            reports, baseline,
            "a failing journal must not change the dispatched reports"
        );
        assert_eq!(
            link.append_failures(),
            1,
            "the failure is counted, not propagated"
        );
        assert_eq!(link.keeper().signed_qty(), dec("0.2"));
    }

    /// Backpressure coalesces to the latest absolute target: superseded revisions drop (losslessly), and a
    /// single pump converges the position to the newest target with exactly one order.
    #[test]
    fn backpressure_coalesces_to_latest_absolute_target() {
        let mut link = link_at("50000", "100000");
        link.submit_target(rev(0, "10000", 1)).unwrap();
        link.submit_target(rev(1, "20000", 2)).unwrap();
        link.submit_target(rev(2, "5000", 3)).unwrap();
        assert_eq!(
            link.dropped_superseded(),
            2,
            "two superseded revisions dropped"
        );

        let reports = link.pump();
        // 5000 / 50000 = 0.1 → a single Buy to the latest target, nothing intermediate.
        let fill = find_fill(&reports).expect("one fill to the latest target");
        assert_eq!(fill.order.side, Side::Buy);
        assert_eq!(fill.order.qty, Qty::new(dec("0.1")).unwrap());
        assert_eq!(link.keeper().signed_qty(), dec("0.1"));
        assert_eq!(link.orders_submitted(), 1, "coalesced to a single order");
        assert_eq!(heartbeat(&reports), (Some(2), VenueHealth::Ok));
    }

    /// Reconnection is idempotent: after a disconnect, re-sending the latest absolute target flattens to a
    /// zero delta — no duplicate order, position unchanged.
    #[test]
    fn reconnect_resends_latest_target_without_double_filling() {
        let mut link = link_at("50000", "100000");
        link.submit_target(rev(7, "10000", 1)).unwrap();
        link.pump();
        assert_eq!(link.keeper().signed_qty(), dec("0.2"));
        assert_eq!(link.orders_submitted(), 1);

        link.disconnect();
        assert!(!link.is_connected());
        assert_eq!(
            link.submit_target(rev(8, "10000", 2)),
            Err(TransportError::Disconnected),
            "submission errors while disconnected"
        );

        let snap = link.reconnect();
        let pos = find_position(&snap).expect("reconnect re-snapshots the position");
        assert_eq!(pos.qty, Qty::new(dec("0.2")).unwrap());

        // The planner re-sends its latest absolute target — a no-op against the kept position.
        let latest = link.latest_target().expect("a latest target is retained");
        link.submit_target(latest).unwrap();
        let reports = link.pump();

        assert!(find_fill(&reports).is_none(), "no duplicate fill on resume");
        assert_eq!(link.orders_submitted(), 1, "no new order after reconnect");
        assert_eq!(link.keeper().signed_qty(), dec("0.2"), "position unchanged");
    }

    /// The QE-216 kill switch halts submission on the transport path: a tripped kill yields no fill and a
    /// `Down` heartbeat, with no order sent.
    #[test]
    fn kill_tripped_dispatch_halts_submission_on_the_wire() {
        let mut link = link_at("50000", "100000");
        link.kill().trip("watchdog: staleness");

        link.submit_target(rev(0, "10000", 1)).unwrap();
        let reports = link.pump();

        assert!(
            find_fill(&reports).is_none(),
            "no fill once the kill is tripped"
        );
        assert_eq!(link.orders_submitted(), 0, "nothing submitted to the venue");
        let (ack, health) = heartbeat(&reports);
        assert_eq!(ack, Some(0));
        assert_eq!(health, VenueHealth::Down("watchdog: staleness".to_owned()));
        assert_eq!(link.keeper().signed_qty(), dec("0"), "position stays flat");
    }

    /// A pump with nothing pending produces no traffic; an at-target revision acks with `Ok` health and no
    /// fill.
    #[test]
    fn idle_pump_is_silent_and_at_target_revision_acks_without_a_fill() {
        let mut link = link_at("50000", "100000");
        assert!(link.pump().is_empty(), "no pending revision → no reports");

        // Reach the target, then submit the same absolute target again → zero delta, no fill.
        link.submit_target(rev(0, "10000", 1)).unwrap();
        link.pump();
        link.submit_target(rev(1, "10000", 2)).unwrap();
        let reports = link.pump();
        assert!(find_fill(&reports).is_none(), "already at target → no fill");
        assert_eq!(heartbeat(&reports), (Some(1), VenueHealth::Ok));
    }
}
