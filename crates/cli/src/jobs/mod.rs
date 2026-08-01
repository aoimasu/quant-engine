//! Runnable CLI jobs (QE-251): the deterministic pipelines the admin server later spawns as
//! subprocesses. Each job writes artefacts into a `--run-dir` and streams JSON-line progress on
//! stdout. No async, no wall-clock, no RNG in any output.

pub mod backtest;
pub mod datetime;
pub mod evolve;
pub mod features;
pub mod ingest;
pub mod metrics;
pub mod pool_intake;
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

    /// QE-458: a steered `--indicator` id is not in the catalogue. Rejected (never a silent full-catalogue
    /// fallback) so a steered request that misnames an indicator errors rather than running un-steered.
    #[error("unknown steer indicator `{id}`: not in the catalogue")]
    UnknownIndicator {
        /// The unrecognised catalogue-indicator id.
        id: String,
    },

    /// QE-458 (design §6.1a): a steered search collapsed the MAP-Elites quality-diversity archive below the
    /// minimum-occupied-niches floor. Surfaced as a hard error (never silently sealed) so steering cannot
    /// flatten the QD archive — widen the indicator subset / raise the budget.
    #[error(
        "steered search collapsed the archive to {occupied} occupied niche(s), below the \
         minimum-occupied-niches floor {floor}: widen the indicator subset or raise the budget"
    )]
    ArchiveCoverageCollapsed {
        /// Occupied MAP-Elites niches the steered search achieved.
        occupied: usize,
        /// The compiled minimum-occupied-niches floor.
        floor: usize,
    },

    /// QE-460 (design §4 (b)): a composite-flow (`--flow`) train carved a frozen holdout that spans fewer
    /// than the minimum distinct QE-125 regime labels — a single-regime holdout is a lucky trailing window,
    /// not a regime-stratified OOS verdict. Surfaced as a hard error (never silently sealed) so the holdout
    /// geometry stays regime-diverse — widen the flow window or lower the holdout size.
    #[error(
        "flow holdout spans {labels} distinct regime label(s) over {bars} bars, below the required \
         minimum {floor}: the frozen holdout is not regime-stratified — widen the flow window so the \
         holdout covers more market regimes (QE-125)"
    )]
    HoldoutRegimeCoverage {
        /// Distinct QE-125 regime labels the holdout spanned.
        labels: usize,
        /// Labelled holdout bars.
        bars: usize,
        /// The compiled minimum distinct-regime floor (`MIN_HOLDOUT_REGIMES`).
        floor: usize,
    },

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

    // ---- QE-499 Phase B — `--pool` train-intake governance (B5) + §4 majors ------------------------
    /// The `--pool` id could not be loaded/verified from the sandbox pool repository (a missing pool
    /// artefact ⇒ hard error; a tampered pool fails its content-hash verify) — the B4 fail-closed boundary.
    #[error("formula pool `{pool_id}` could not be loaded/verified: {reason}")]
    PoolLoad {
        /// The requested `--pool` id.
        pool_id: String,
        /// The underlying formula-pool error (missing file, hash mismatch, deserialise).
        reason: String,
    },
    /// B5: only a **Sandbox** pool may enter the catalogue-injection bridge; a Production (or other) pool
    /// must go through the QE-454 production seal path, not this research mechanism.
    #[error("formula pool `{pool_id}` is not Sandbox mode ({mode}); the --pool bridge admits Sandbox pools only")]
    PoolNotSandbox {
        /// The requested `--pool` id.
        pool_id: String,
        /// The pool's sealed mode.
        mode: String,
    },
    /// §4 major: a pool whose deflation summary is **not GP-aware** (`gp_aware == false`) is a HARD ERROR at
    /// intake — its trial basis was the blind analytic floor, not the real GP-aware trial counter, so its
    /// composed deflation would be dishonest.
    #[error("formula pool `{pool_id}` has gp_aware=false — the GP-aware trial basis is a hard requirement for the --pool bridge (§4)")]
    PoolNotGpAware {
        /// The requested `--pool` id.
        pool_id: String,
    },
    /// B5: the pool carries no per-formula `gate_evidence`, or a formula lacks a passing evidence row —
    /// formulas may enter a vintage only with present-and-passing tradability/parsimony evidence (§13.5).
    #[error("formula pool `{pool_id}` gate_evidence is absent or incomplete (formula `{formula_hash}` unevidenced) — required present-and-passing for the --pool bridge (B5)")]
    PoolGateEvidenceMissing {
        /// The requested `--pool` id.
        pool_id: String,
        /// The formula lacking passing evidence.
        formula_hash: String,
    },
    /// B5: a per-formula `gate_evidence` row is present but does **not** pass all of hard-blocks 5–8.
    #[error("formula pool gate_evidence for `{formula_hash}` does not pass hard-blocks 5–8 (B5)")]
    PoolGateEvidenceFailed {
        /// The failing formula.
        formula_hash: String,
    },
    /// A sealed `PoolFormula` sexpr did not parse back to an `Expr` (B6 reconstruction failed).
    #[error("formula pool sexpr for `{formula_hash}` failed to parse: {reason}")]
    PoolFormulaParse {
        /// The offending formula.
        formula_hash: String,
        /// The parse error.
        reason: String,
    },
    /// A sealed `PoolFormula`'s recomputed canonical hash does not match its stored `formula_hash` — the
    /// sexpr and hash disagree (a tampered or malformed pool entry). Fail closed.
    #[error("formula pool sexpr/hash mismatch: stored `{stored}`, recomputed `{recomputed}`")]
    PoolFormulaHashMismatch {
        /// The stored (claimed) formula hash.
        stored: String,
        /// The hash recomputed from the stored sexpr.
        recomputed: String,
    },
}
