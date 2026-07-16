//! qe-runtime — runtime pipeline (bootstrap, live, hedge planning).
//!
//! Scaffold crate established in QE-001; real APIs land in later tickets. QE-009 wires in the
//! risk/kill-switch contract: the runtime's order port is, by its type, an order gate that holds an
//! out-of-band kill handle. QE-205 adds the live kline source ([`live_kline`]): REST-prime + wss-stitch
//! feeding the shared QE-106 reconstructor, so live coarser bars match batch reconstruction exactly.
//! QE-206 adds the live factor join ([`factor_join`]): an as-of join of live scalar context onto base bars
//! driving the shared QE-107/108 catalogue, so live factor rows match offline feature vectors exactly.
//! QE-207 adds the evaluator session ([`evaluator`]): one stateful object that runs a sealed vintage's
//! chromosomes through replay then live with no state copy, so decisions are continuous across the boundary.
//! QE-209 adds the bootstrap pipeline ([`bootstrap`]): a cold start replays the lookback window
//! (paginated+retried+cached REST → multi-resolution replay → factor merge) through that same evaluator in
//! replay mode, reconstructing per-strategy state deterministically to where a continuous planner would hold it.
//! QE-208 adds the mark EMA loop ([`live_mark`]): markPrice@1s samples are smoothed through QE-116's
//! [`MarkEma`](qe_risk::MarkEma) (τ½=60s) into a tick stream carrying both raw and smoothed marks, fed to a
//! [`MarkTickObserver`] — the seam the breaker layer (QE-212) consumes.
//! QE-210 adds the reconstructed state ([`boot_state`]): the cold-start anchor derived from the replay —
//! per-strategy positions, dormancy latches, and **true committed peak equity** (all-time, never windowed),
//! the last load-bearing for the drawdown breaker's anchor at live start.
//! QE-211 adds the in-process cutover ([`cutover`]): the warmed session is switched from replay to live
//! **in place** (no state copy) while enforcing bar continuity at the seam — overlap bars are dropped (no
//! duplicate), a skipped bar is surfaced as a gap, and post-cutover decisions match a continuous reference.
//! QE-212 adds the circuit-breaker layer ([`live_breakers`]): per-strategy + ensemble QE-116 breakers
//! calibrated from the vintage profile, latching gated scopes and clamping gated strategies to
//! [`Decision`](qe_signal::Decision)`::Exit` (flat) before netting — firing identically to the QE-116 replay.
//! QE-213 adds position netting ([`live_netter`]): the post-breaker per-strategy targets
//! (`weight × size_bps/10_000`, signed by direction) sum into one aggregate [`NetTarget`] per instrument —
//! gated strategies are flat post-breaker, so they contribute zero.
//! QE-214 adds the Hedge Planner ([`hedger`]): it scales that aggregate by equity into an **absolute**
//! [`TargetPosition`], sourced from a [`PositionKeeper`] seam. Emitting an absolute target (not a delta) makes
//! it **stateless** wrt the current venue position — the `target − current` delta is QE-217's concern.
//! QE-215 adds the pre-trade governor ([`pretrade`]): it enforces the QE-009 [`RiskLimits`](qe_risk::RiskLimits)
//! on a [`TargetPosition`] before it is sent — clamping (max notional/leverage), rejecting (gross/net,
//! liquidation-distance floor, margin ceiling), or halting by outcome severity.
//! QE-217 adds the edge gateway ([`edge`]): [`plan_delta`] translates an absolute target into a venue-native
//! order delta vs the kept position, [`VenueKeeper`] absorbs the QE-204 user-data feed as authoritative
//! position/account state (`impl`s the [`PositionKeeper`](hedger::PositionKeeper) seam), and [`VenueSimulator`]
//! runs the full loop in-memory with no real orders.
//! QE-216 adds the out-of-band kill-switch at the venue adapter ([`kill_gate`]): [`VenueKillGate`] wraps the
//! simulator with the QE-009 [`KillHandle`], so a trip from anywhere (watchdog / manual, cockpit-independent)
//! **halts submission** and **flattens** the kept position deterministically — satisfying the QE-009
//! [`OrderGate`](qe_risk::OrderGate) conformance.
//! QE-221 adds the reconciliation divergence alarm ([`reconciliation`]): [`ReconciliationGuard`] compares the
//! runtime's **expected** position against the venue's authoritative report each period and, once a divergence
//! beyond tolerance **persists** (a debounce that ignores in-flight-order skew), alarms and can trip the QE-216
//! kill out-of-band — the fast *detector* (attribution is QE-302).
//! QE-222 adds GATE G2 — live shadow / dry-run ([`shadow`]): [`ShadowGateway`] is the Edge gateway in
//! **dry-run** (runs `plan_delta` and **logs** the would-be order, advancing a shadow position as-if-filled,
//! submitting nothing), and [`ShadowRun`] drives a target stream through it **and** a submitting reference
//! [`PlannerAdapterLink`], reconciling the two via the QE-221 guard so a pipeline discrepancy is reported
//! before any live capital.

pub mod boot_state;
pub mod bootstrap;
pub mod classify;
pub mod cutover;
pub mod edge;
pub mod evaluator;
pub mod factor_join;
pub mod hedger;
pub mod kill_gate;
pub mod live_breakers;
pub mod live_kline;
pub mod live_mark;
pub mod live_netter;
pub mod pretrade;
pub mod reconciliation;
pub mod shadow;
pub mod transport;
pub mod vintage_rollover;

pub use boot_state::{
    BootStateError, CommittedPeak, DormancyLatch, ReconstructedState, StrategyState,
};
pub use bootstrap::{
    paginate_klines, paginate_series, BootstrapError, BootstrapPipeline, HistoricalSource,
    HistoricalWindow, Reconstructed,
};
pub use cutover::{Cutover, CutoverError, CutoverStep};
pub use edge::{plan_delta, Order, OrderIntent, OrderState, SimFill, VenueKeeper, VenueSimulator};
pub use evaluator::{ChromosomeDecision, EvalOutput, EvaluatorSession, SessionMode};
pub use factor_join::LiveFactorJoin;
pub use hedger::{CapitalView, HedgePlanner, PositionKeeper, TargetPosition};
pub use kill_gate::{KillHalt, KillOutcome, VenueKillGate};
pub use live_breakers::BreakerLayer;
pub use live_kline::LiveKlineSource;
pub use live_mark::{
    MarkEmaLoop, MarkTick, MarkTickObserver, DEFAULT_STALENESS_BOUND_SECS, DEFAULT_TICK_SECS,
};
pub use live_netter::{NetLeg, NetTarget, PositionNetter};
pub use pretrade::{PreTradeDecision, PreTradeGovernor, PreTradeVerdict};
pub use qe_risk::{KillHandle, KillSwitch};
pub use reconciliation::{AlarmAction, Divergence, ReconOutcome, ReconciliationGuard};
pub use shadow::{ShadowGateway, ShadowReport, ShadowRun, WouldBeOrder};
pub use transport::{
    AdapterReport, AppendError, AppendSink, NullAppendSink, PlannerAdapterLink, TargetRevision,
    TransportError, VenueHealth,
};
pub use vintage_rollover::{ActiveVintage, RolloverRecord};

/// The runtime's live order-submission port.
///
/// It is an [`OrderGate`](qe_risk::OrderGate) by definition, so every component on the live order
/// path is *born* holding a [`KillHandle`] (QE-009 contract) and can be flattened-and-halted
/// out-of-band — independently of the cockpit and the Hedge Planner. Concrete ports and limit
/// enforcement land in later tickets (QE-215/216); this is the interface they must satisfy.
pub trait OrderPort: qe_risk::OrderGate {
    /// A stable name for this port, for logging and health.
    fn port_name(&self) -> &str;
}

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-runtime"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-runtime");
    }
}
