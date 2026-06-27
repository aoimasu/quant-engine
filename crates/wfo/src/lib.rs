//! qe-wfo — walk-forward optimisation (QD MAP-Elites) and backtest realism.
//!
//! - [`friction`] (QE-109) — the execution-friction & funding cost model: fees (taker/maker),
//!   size-dependent slippage, funding accrued from the actual historical series, and a
//!   cost-sensitivity sweep, returning a decomposed `gross / fees / slippage / funding` P&L.
//! - [`genome`] (QE-110) — the strategy genome: a fixed-structure rule-bank representation over
//!   quantised feature states that the QD/DE search mutates, recombines, and niches; emits a per-bar
//!   `Decision` stream the backtester (QE-120) drives through `friction`.

pub mod friction;
pub mod genome;

pub use friction::{
    cost_sweep, simulate, Event, FeeSchedule, Fill, FrictionConfig, FundingStamp, Liquidity,
    PnlBreakdown, Position, SlippageModel,
};
pub use genome::{
    Clause, Decision, ExitParams, Genome, PositionState, RiskParams, RuleSet, CLAUSES_PER_SET,
    MAX_SIZE_BPS, REP_VERSION,
};

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-wfo"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-wfo");
    }
}
