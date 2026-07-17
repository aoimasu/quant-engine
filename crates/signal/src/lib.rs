//! qe-signal — indicator catalogue and bar reconstruction shared by training and runtime.
//!
//! Storage-free by design: it runs on the runtime hot path, so it depends on `qe-domain` only and
//! never pulls in a database (QE-003 "no database on the critical path").
//!
//! - [`reconstruct`] (QE-106) — deterministic multi-resolution bar roll-up (5m → 30m/4h…), with one
//!   incremental fold shared by batch and streaming for byte-identical parity (QE-206).
//! - [`indicator`] (QE-107) — the quantised indicator catalogue (≥20 indicators), finite-lookback +
//!   batch/streaming-identical, the substrate the strategy genome reasons over.
//! - [`feature`] (QE-108) — per-bar feature-vector assembly from the catalogue states (the rows
//!   WFO/DE consume), batch/streaming-identical.
//! - [`regime`] (QE-125) — volatility / trend-vs-chop regime labels over history plus a per-regime
//!   expectancy table, the regime tags QE-127's DE objective and QE-133's reporting read.
//! - [`genome`] (QE-110) — the strategy genome representation + its pure `decide` (feature vector →
//!   trading decision). It lives here, not in `qe-wfo`, because it is the one piece of strategy logic
//!   **both** training (search) and the live runtime must run identically: the QE-001 decoupling
//!   invariant requires train/live shared code to cross only through `signal`/`domain`. `qe-wfo`
//!   re-exports it so the search side's API is unchanged.

pub mod feature;
pub mod genome;
pub mod indicator;
pub mod reconstruct;
pub mod regime;

pub use feature::{
    assemble_batch, CatalogueIdentity, FeatureAssembler, FeatureSchema, FeatureVector,
};
pub use genome::{
    graded_strength_floor, Clause, Decision, ExitParams, Genome, PositionState, RiskParams,
    RuleSet, CLAUSES_PER_SET, MAX_SIZE_BPS, REP_VERSION,
};
pub use indicator::{
    catalogue, compute_batch, max_lookback, CatalogueConfig, Indicator, IndicatorSpec, QState,
    Quantiser, Sample, CATALOGUE_VERSION,
};
pub use reconstruct::{reconstruct_batch, reconstruct_tiers, BarReconstructor, ReconError};
pub use regime::{
    expectancy_table, label_regimes, ExpectancyTable, Regime, RegimeConfig, RegimeExpectancy,
    TrendState, VolState, DEFAULT_REGIME_WINDOW, DEFAULT_TREND_THRESHOLD,
};

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-signal"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-signal");
    }
}
