//! qe-telemetry — structured logging & tracing for the platform.
//!
//! Provides a configurable `tracing` subscriber (JSON or pretty, per-module levels via an
//! `EnvFilter` directive, optional non-blocking writer) and helpers for the standard correlation
//! fields (`run_id`, `vintage_hash`, `instrument`, `window_id`).
//!
//! Hot-path guarantee: hot-path emissions should use the [`HOT_PATH_TARGET`] target at
//! `trace`/`debug`. Production filters disable them, so the macro short-circuits before any
//! formatting or I/O. When enabled, [`init`] with `non_blocking = true` routes writes through a
//! background worker, keeping the emitting thread off synchronous I/O.

use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::{self, writer::BoxMakeWriter};
use tracing_subscriber::EnvFilter;

/// Tracing target for hot-path log events.
///
/// Production filters disable this target so emissions short-circuit on the level check before any
/// formatting or I/O. Emitters and filter directives must reference this constant rather than the
/// bare string, so a typo can't silently mis-filter and defeat the disable-in-production strategy.
pub const HOT_PATH_TARGET: &str = "qe::hot_path";

/// Errors raised while installing telemetry.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The `level` directive could not be parsed as an `EnvFilter`.
    #[error("invalid log filter `{directive}`: {message}")]
    Filter {
        /// The offending directive.
        directive: String,
        /// Parser message.
        message: String,
    },
    /// A global subscriber was already installed (or otherwise could not be set).
    #[error("failed to install global subscriber: {0}")]
    Init(String),
}

/// Output format for log records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Machine-readable JSON (one object per record).
    Json,
    /// Human-readable, colourless multi-line.
    Pretty,
}

/// Telemetry settings.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// `EnvFilter` directive, e.g. `"info,qe_wfo=debug"` — gives per-module levels.
    pub level: String,
    /// Output format.
    pub format: LogFormat,
    /// Route writes through a background worker thread (keeps emitters off blocking I/O).
    ///
    /// Uses `tracing-appender`'s default **lossy** mode: under sustained backpressure it *drops*
    /// records rather than blocking the emitter. This is the right tradeoff for the order-emission
    /// hot path (never block the critical path), but means logs can be silently lost under load —
    /// relevant for audit. Only the *disabled* hot-path (zero-I/O) case has a test; the enabled
    /// non-blocking path is guaranteed by construction.
    pub non_blocking: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Json,
            non_blocking: true,
        }
    }
}

/// Keeps the background writer worker alive. Hold it for the program's lifetime; dropping it
/// flushes buffered records.
#[must_use = "dropping the guard flushes and stops the background log writer"]
pub struct TelemetryGuard {
    _worker: Option<WorkerGuard>,
}

/// Standard correlation context attached to a stage span.
#[derive(Debug, Clone, Copy)]
pub struct Correlation<'a> {
    /// Unique id for this process run.
    pub run_id: &'a str,
    /// Content hash of the active vintage (`"-"` when not yet known).
    pub vintage_hash: &'a str,
    /// Instrument symbol (`"-"` for whole-run stages).
    pub instrument: &'a str,
    /// Walk-forward window id (`"-"` when not applicable).
    pub window_id: &'a str,
}

impl<'a> Correlation<'a> {
    /// A correlation context with all optional fields set to `"-"`.
    #[must_use]
    pub fn run(run_id: &'a str, vintage_hash: &'a str) -> Self {
        Self {
            run_id,
            vintage_hash,
            instrument: "-",
            window_id: "-",
        }
    }
}

/// Open an `info`-level span named `stage` carrying the correlation fields. Events emitted while
/// the span is entered inherit this context.
#[must_use]
pub fn stage_span(stage: &str, c: &Correlation) -> tracing::Span {
    tracing::info_span!(
        "stage",
        stage = stage,
        run_id = c.run_id,
        vintage_hash = c.vintage_hash,
        instrument = c.instrument,
        window_id = c.window_id,
    )
}

/// Install the global telemetry subscriber from `cfg`.
///
/// # Errors
/// Returns [`TelemetryError::Filter`] if `cfg.level` is not a valid filter directive, or
/// [`TelemetryError::Init`] if a global subscriber is already installed.
pub fn init(cfg: &TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    let filter = EnvFilter::try_new(&cfg.level).map_err(|e| TelemetryError::Filter {
        directive: cfg.level.clone(),
        message: e.to_string(),
    })?;

    let (writer, worker): (BoxMakeWriter, Option<WorkerGuard>) = if cfg.non_blocking {
        let (nb, guard) = tracing_appender::non_blocking(std::io::stdout());
        (BoxMakeWriter::new(nb), Some(guard))
    } else {
        (BoxMakeWriter::new(std::io::stdout), None)
    };

    let builder = fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_writer(writer);

    match cfg.format {
        LogFormat::Json => set_global(builder.json().finish())?,
        // `.pretty()` gives the multi-line human format; `.with_ansi(false)` keeps it colourless
        // (the writer is boxed/non-blocking so fmt can't TTY-detect and would otherwise emit raw
        // ANSI escapes into redirected logs).
        LogFormat::Pretty => set_global(builder.pretty().with_ansi(false).finish())?,
    }

    Ok(TelemetryGuard { _worker: worker })
}

fn set_global<S>(subscriber: S) -> Result<(), TelemetryError>
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| TelemetryError::Init(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// In-memory capture writer for assertions (avoids the process-global `init`).
    #[derive(Clone, Default)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl BufWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("lock").clone()).expect("utf8")
        }
        fn is_empty(&self) -> bool {
            self.0.lock().expect("lock").is_empty()
        }
    }

    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn json_subscriber(filter: &str, buf: &BufWriter) -> impl tracing::Subscriber + Send + Sync {
        fmt::Subscriber::builder()
            .with_env_filter(EnvFilter::new(filter))
            .json()
            .with_writer(buf.clone())
            .finish()
    }

    fn pretty_subscriber(filter: &str, buf: &BufWriter) -> impl tracing::Subscriber + Send + Sync {
        fmt::Subscriber::builder()
            .with_env_filter(EnvFilter::new(filter))
            .pretty()
            .with_ansi(false)
            .with_writer(buf.clone())
            .finish()
    }

    #[test]
    fn stage_span_emits_correlation_fields() {
        let buf = BufWriter::default();
        let subscriber = json_subscriber("info", &buf);
        tracing::subscriber::with_default(subscriber, || {
            let corr = Correlation {
                run_id: "run-1",
                vintage_hash: "vh123",
                instrument: "BTCUSDT",
                window_id: "w0",
            };
            let span = stage_span("ingest", &corr);
            let _enter = span.enter();
            tracing::info!(phase = "start", "ingest stage started");
        });

        let out = buf.contents();
        for needle in [
            "\"stage\":\"ingest\"",
            "\"run_id\":\"run-1\"",
            "\"vintage_hash\":\"vh123\"",
            "\"instrument\":\"BTCUSDT\"",
            "\"window_id\":\"w0\"",
        ] {
            assert!(out.contains(needle), "missing {needle} in: {out}");
        }
    }

    #[test]
    fn disabled_hot_path_event_performs_no_write() {
        let buf = BufWriter::default();
        // `info` filter disables `trace` → the hot-path event short-circuits, no I/O.
        let subscriber = json_subscriber("info", &buf);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: HOT_PATH_TARGET, order_id = 42, "order emitted");
        });
        assert!(
            buf.is_empty(),
            "a disabled hot-path event must produce zero output (no blocking I/O), got: {}",
            buf.contents()
        );
    }

    #[test]
    fn enabled_hot_path_target_does_write_when_level_permits() {
        // Sanity counterpart: when the target/level is enabled, the event is recorded — proving
        // the previous test's emptiness is due to filtering, not a broken pipe.
        let buf = BufWriter::default();
        let subscriber = json_subscriber(&format!("{HOT_PATH_TARGET}=trace"), &buf);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: HOT_PATH_TARGET, order_id = 42, "order emitted");
        });
        assert!(buf.contents().contains("order emitted"));
    }

    #[test]
    fn pretty_format_is_multiline_and_colourless() {
        // Locks the LogFormat::Pretty fix: pretty output is multi-line and carries no ANSI escapes.
        let buf = BufWriter::default();
        let subscriber = pretty_subscriber("info", &buf);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(phase = "start", "pretty event");
        });
        let out = buf.contents();
        assert!(out.contains("pretty event"));
        assert!(
            out.lines().count() > 1,
            "pretty format should be multi-line, got: {out:?}"
        );
        assert!(
            !out.contains('\u{1b}'),
            "pretty output must be colourless (no ANSI), got: {out:?}"
        );
    }

    #[test]
    fn invalid_filter_is_rejected() {
        // A target directive with a bogus level fails `EnvFilter` parsing, so `init` returns the
        // Filter error *before* installing any global subscriber (safe to call here).
        let cfg = TelemetryConfig {
            level: "qe=not_a_level".to_owned(),
            format: LogFormat::Json,
            non_blocking: false,
        };
        assert!(matches!(init(&cfg), Err(TelemetryError::Filter { .. })));
    }
}
