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

pub mod bootstrap;
pub mod evaluator;
pub mod factor_join;
pub mod live_kline;
pub mod live_mark;

pub use bootstrap::{
    paginate_klines, paginate_series, BootstrapError, BootstrapPipeline, HistoricalSource,
    HistoricalWindow, Reconstructed,
};
pub use evaluator::{ChromosomeDecision, EvalOutput, EvaluatorSession, SessionMode};
pub use factor_join::LiveFactorJoin;
pub use live_kline::LiveKlineSource;
pub use live_mark::{MarkEmaLoop, MarkTick, MarkTickObserver};
pub use qe_risk::{KillHandle, KillSwitch};

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
