//! Runnable CLI jobs (QE-251): the deterministic pipelines the admin server later spawns as
//! subprocesses. Each job writes artefacts into a `--run-dir` and streams JSON-line progress on
//! stdout. No async, no wall-clock, no RNG in any output.

pub mod backtest;
pub mod datetime;
pub mod features;
pub mod ingest;
pub mod metrics;
pub mod result;

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
    /// Terminal success: the artefact filename written into the run dir.
    Done {
        /// The result artefact name (`result.json`).
        result: String,
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

/// Write the terminal `done` line.
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_done(w: &mut impl Write, result: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
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

    /// A historical-source fetch/decode failure during ingest (the injectable `HistoricalSource`
    /// seam surfaced an error).
    #[error("ingest source failure: {0}")]
    Ingest(String),

    /// A storage-layer failure.
    #[error(transparent)]
    Storage(#[from] qe_storage::StorageError),

    /// A vintage load / verify failure.
    #[error(transparent)]
    Vintage(#[from] qe_vintage::VintageError),

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
