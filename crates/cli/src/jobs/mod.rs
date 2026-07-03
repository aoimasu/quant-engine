//! Runnable CLI jobs (QE-251): the deterministic pipelines the admin server later spawns as
//! subprocesses. Each job writes artefacts into a `--run-dir` and streams JSON-line progress on
//! stdout. No async, no wall-clock, no RNG in any output.

pub mod backtest;
pub mod datetime;
pub mod features;
pub mod ingest;
pub mod metrics;
pub mod result;
pub mod train;

use std::io::{self, Write};

use serde::Serialize;
use thiserror::Error;

/// One JSON-line progress record on stdout. The stream is a sequence of `progress` lines followed by
/// exactly one terminal `done` or `error` line (see [`emit_progress`], [`emit_done`], [`emit_error`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ProgressLine {
    /// An intermediate progress update.
    Progress {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Coarse stage label (`load|scan|features|simulate|report`).
        stage: String,
        /// Human-readable line.
        msg: String,
    },
    /// One MAP-Elites search generation (QE-260 train job). Carries the archive coverage and best-so-far
    /// fitness so the training monitor (QE-261) can render the generation → coverage → fitness trace.
    Gen {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"search"`).
        stage: String,
        /// The generation just completed (`1..=generations`).
        generation: usize,
        /// Total generations in the budget.
        generations: usize,
        /// Total occupied MAP-Elites cells across both directions (`qe_wfo::regularise::coverage` sum).
        coverage: usize,
        /// Occupied cells in the Long archive.
        coverage_long: usize,
        /// Occupied cells in the Short archive.
        coverage_short: usize,
        /// Best archive fitness seen so far (`f64::NEG_INFINITY` before any accepted elite).
        best_fitness: f64,
    },
    /// The ensemble (portfolio) construction result (QE-260). Carries the CV fold count.
    Ensemble {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"ensemble"`).
        stage: String,
        /// Cross-validation folds the portfolio search scored over.
        folds: usize,
        /// Number of chromosomes selected into the ensemble.
        members: usize,
        /// The converged cross-validated robust-basin score.
        score: f64,
    },
    /// The G1 gate verdict (QE-260/QE-134). `promoted` is the pass/fail; `failed` names the blocking
    /// criteria (empty iff promoted).
    Gate {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"gate"`).
        stage: String,
        /// Whether the vintage cleared every G1 criterion.
        promoted: bool,
        /// The names of the criteria that failed (empty iff promoted).
        failed: Vec<String>,
        /// In-sample (train-window) net-of-cost Sharpe.
        in_sample_sharpe: f64,
        /// Holdout (untouched OOS) net-of-cost Sharpe.
        holdout_sharpe: f64,
        /// Deflated Sharpe Ratio the DSR criterion evaluated.
        dsr: f64,
        /// White's Reality Check / SPA p-value.
        spa_pvalue: f64,
        /// Effective number of trials the DSR deflated against.
        n_trials: usize,
    },
    /// Terminal success: the artefact filename written into the run dir.
    Done {
        /// The result artefact name (`result.json`).
        result: String,
        /// The sealed vintage id, when a terminal produces one (train job). Omitted for the backtest job
        /// so its `{"t":"done","result":"result.json"}` shape is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vintage: Option<String>,
    },
    /// Terminal failure.
    Error {
        /// The failure message.
        msg: String,
    },
}

/// Write one `progress` line to `w`, newline-terminated. Deterministic (no timestamp).
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_progress(w: &mut impl Write, pct: u8, stage: &str, msg: &str) -> io::Result<()> {
    let line = ProgressLine::Progress {
        pct,
        stage: stage.to_owned(),
        msg: msg.to_owned(),
    };
    write_line(w, &line)
}

/// Write the terminal `done` line (no vintage — the backtest/ingest form).
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_done(w: &mut impl Write, result: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            vintage: None,
        },
    )
}

/// Write the terminal `done` line naming the sealed `vintage` (the train form).
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_train_done(w: &mut impl Write, result: &str, vintage: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            vintage: Some(vintage.to_owned()),
        },
    )
}

/// Write the terminal `error` line.
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_error(w: &mut impl Write, msg: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Error {
            msg: msg.to_owned(),
        },
    )
}

fn write_line(w: &mut impl Write, line: &ProgressLine) -> io::Result<()> {
    let json = serde_json::to_string(line).map_err(io::Error::other)?;
    writeln!(w, "{json}")
}

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

    /// A filesystem failure.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying error.
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_serialises_with_t_tag() {
        let mut buf = Vec::new();
        emit_progress(&mut buf, 50, "features", "assembling").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.trim_end(),
            r#"{"t":"progress","pct":50,"stage":"features","msg":"assembling"}"#
        );
    }

    #[test]
    fn done_and_error_lines() {
        let mut buf = Vec::new();
        emit_done(&mut buf, "result.json").unwrap();
        emit_error(&mut buf, "boom").unwrap();
        let s = String::from_utf8(buf).unwrap();
        let mut lines = s.lines();
        assert_eq!(
            lines.next().unwrap(),
            r#"{"t":"done","result":"result.json"}"#
        );
        assert_eq!(lines.next().unwrap(), r#"{"t":"error","msg":"boom"}"#);
    }
}
