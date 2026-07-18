//! Runnable CLI jobs (QE-251): the deterministic pipelines the admin server later spawns as
//! subprocesses. Each job writes artefacts into a `--run-dir` and streams JSON-line progress on
//! stdout. No async, no wall-clock, no RNG in any output.

pub mod backtest;
pub mod datetime;
pub mod evolve;
pub mod features;
pub mod ingest;
pub mod metrics;
pub mod result;
pub mod train;

use std::io;

use thiserror::Error;

/// The run-protocol wire types (QE-406) live in the dependency-free `qe-run-protocol` leaf crate so the
/// admin server parses **exactly** what this binary emits. Re-exported here so the existing
/// `qe_cli::jobs::{ProgressLine, emit_progress, emit_done, emit_train_done, emit_error}` import paths
/// are unchanged. See that crate for the single source of truth (progress lines + `PROTOCOL_VERSION`).
pub use qe_run_protocol::{
    emit_done, emit_error, emit_evolve_done, emit_ingest_done, emit_progress, emit_train_done,
    ProgressLine,
};

/// A backtest/ingest job failure. Distinct from [`crate::CliError`] (arg parsing / config): these are
/// runtime failures surfaced as the terminal `{"t":"error"}` line and a non-zero exit code.
#[derive(Debug, Error)]
pub enum RunError {
    /// The `--universe` was empty; v1 needs at least one instrument.
    #[error("empty universe: backtest needs at least one --universe symbol")]
    EmptyUniverse,

    /// A `YYYY-MM-DD` date could not be parsed.
    #[error("invalid date `{0}` (expected YYYY-MM-DD)")]
    BadDate(String),

    /// An instrument symbol was not a valid [`qe_domain::InstrumentId`].
    #[error("invalid instrument `{symbol}`: {source}")]
    Instrument {
        /// The offending symbol.
        symbol: String,
        /// The domain validation error.
        source: qe_domain::DomainError,
    },

    /// An unknown bar resolution.
    #[error("invalid resolution `{0}`")]
    BadResolution(String),

    /// The window yielded no bars for the instrument.
    #[error("no bars for `{symbol}` at `{resolution}` over the requested window")]
    NoBars {
        /// The instrument.
        symbol: String,
        /// The resolution.
        resolution: String,
    },

    /// A vintage chromosome is not valid against the catalogue schema the job builds — the vintage was
    /// evolved against a different catalogue than this build ships (see the design note, decision 1).
    #[error(
        "schema mismatch: chromosome #{index} is not valid against the catalogue schema \
         (len {schema_len}, states {num_states}) — vintage evolved against a different catalogue"
    )]
    SchemaMismatch {
        /// The offending chromosome index.
        index: usize,
        /// The schema feature count.
        schema_len: usize,
        /// The schema state count.
        num_states: u16,
    },

    /// The selected `--strategy` chromosome id was not found in the vintage.
    #[error("strategy `{0}` not found in vintage")]
    StrategyNotFound(String),

    /// The vintage carried no chromosomes.
    #[error("vintage has no chromosomes")]
    EmptyVintage,

    /// The MAP-Elites search produced no archive elites — nothing to build an ensemble from (the budget
    /// was too small, or every candidate was rejected as noise). Raise the budget / widen the window.
    #[error("search produced no elites: raise the budget or widen the training window")]
    NoElites,

    /// The ensemble portfolio search selected no members (empty mask) — no vintage could be sealed.
    #[error("ensemble search selected no strategies")]
    EmptyEnsemble,

    /// Funding coverage over the training window is below the configured floor (QE-403). Selecting,
    /// validating, and G1-gating on funding-free returns would admit exactly the funding-negative
    /// strategies QE-109 exists to reject, so the job refuses to seal.
    #[error(
        "funding coverage {coverage_pct}% over the training window is below the required \
         {threshold_pct}% (present {present} of expected {expected} 8h stamps): refusing to seal on \
         funding-free returns — ingest funding for this window (QE-103) or lower \
         `selection.funding_coverage_min`"
    )]
    FundingCoverage {
        /// Funding stamps actually present over the decision-bar span.
        present: usize,
        /// Expected 8h funding stamps over the decision-bar span.
        expected: usize,
        /// Realised coverage, as a whole-number percent (`present/expected`).
        coverage_pct: u32,
        /// The configured minimum coverage, as a whole-number percent.
        threshold_pct: u32,
    },

    /// A historical-source fetch/decode failure during ingest (the injectable `HistoricalSource`
    /// seam surfaced an error).
    #[error("ingest source failure: {0}")]
    Ingest(String),

    /// A storage-layer failure.
    #[error(transparent)]
    Storage(#[from] qe_storage::StorageError),

    /// A vintage load / verify / seal failure.
    #[error(transparent)]
    Vintage(#[from] qe_vintage::VintageError),

    /// The training window was too short to evolve over (fewer than two train bars after the holdout /
    /// embargo split).
    #[error(
        "training window too short: need at least two train bars after the holdout+embargo split"
    )]
    TrainWindowTooShort,

    /// A lineage-hashing failure while deriving the sealed vintage id.
    #[error(transparent)]
    Lineage(#[from] qe_determinism::LineageError),

    /// The freeze of the illuminated survivors into a `K ≤ 16` pool failed (QE-452 evolve job).
    #[error("formula-pool freeze failed: {0}")]
    Freeze(String),

    /// Sealing / writing the formula-pool artefact failed (QE-452 evolve job).
    #[error("formula-pool seal failed: {0}")]
    Pool(String),

    /// A filesystem failure.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying error.
        source: io::Error,
    },
}
