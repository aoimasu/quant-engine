//! qe-runtime — runtime pipeline (bootstrap, live, hedge planning).
//!
//! Scaffold crate established in QE-001; real APIs land in later tickets. QE-009 wires in the
//! risk/kill-switch contract: the runtime's order port is, by its type, an order gate that holds an
//! out-of-band kill handle. QE-205 adds the live kline source ([`live_kline`]): REST-prime + wss-stitch
//! feeding the shared QE-106 reconstructor, so live coarser bars match batch reconstruction exactly.
//! QE-206 adds the live factor join ([`factor_join`]): an as-of join of live scalar context onto base bars
//! driving the shared QE-107/108 catalogue, so live factor rows match offline feature vectors exactly.

pub mod factor_join;
pub mod live_kline;

pub use factor_join::LiveFactorJoin;
pub use live_kline::LiveKlineSource;
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
