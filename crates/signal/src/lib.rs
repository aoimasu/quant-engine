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

pub mod feature;
pub mod indicator;
pub mod reconstruct;

pub use feature::{assemble_batch, FeatureAssembler, FeatureSchema, FeatureVector};
pub use indicator::{
    catalogue, compute_batch, max_lookback, CatalogueConfig, Indicator, IndicatorSpec, QState,
    Quantiser, Sample, CATALOGUE_VERSION,
};
pub use reconstruct::{reconstruct_batch, reconstruct_tiers, BarReconstructor, ReconError};

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
