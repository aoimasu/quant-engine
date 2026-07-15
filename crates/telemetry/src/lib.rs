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

/// Destination stream for log records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// Standard output. The default; used by the server composition root.
    Stdout,
    /// Standard error. The CLI **must** use this: the server reads the CLI
    /// child's stdout as the `ProgressLine` run protocol, so telemetry writing to
    /// stdout would corrupt it. Routing to stderr keeps stdout a clean channel.
    Stderr,
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
    /// Destination stream for records. Defaults to [`OutputStream::Stdout`]; the
    /// CLI overrides it to [`OutputStream::Stderr`] to protect its stdout protocol.
    pub writer: OutputStream,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Json,
            non_blocking: true,
            writer: OutputStream::Stdout,
        }
    }
}

impl TelemetryConfig {
    /// Build a config from the environment, so operators can retune logging
    /// without recompiling. Fields not backed by an env var keep their
    /// [`Default`] value.
    ///
    /// - **level** — the first non-empty of `RUST_LOG`, then `QE_LOG`, else
    ///   `"info"`. The standard `RUST_LOG` wins; `QE_LOG` is a project-specific
    ///   fallback. The value is an `EnvFilter` directive
    ///   (e.g. `"info,qe_wfo=debug"`); [`init`] validates it.
    /// - **format** — `QE_LOG_FORMAT`: `"pretty"` (case-insensitive) selects
    ///   [`LogFormat::Pretty`]; anything else (including unset) selects
    ///   [`LogFormat::Json`].
    ///
    /// `non_blocking` and `writer` keep their defaults; callers override `writer`
    /// (the CLI forces [`OutputStream::Stderr`]).
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        let level = env_non_empty("RUST_LOG")
            .or_else(|| env_non_empty("QE_LOG"))
            .unwrap_or(default.level);
        let format = match env_non_empty("QE_LOG_FORMAT") {
            Some(v) if v.eq_ignore_ascii_case("pretty") => LogFormat::Pretty,
            _ => LogFormat::Json,
        };
        Self {
            level,
            format,
            non_blocking: default.non_blocking,
            writer: default.writer,
        }
    }
}

/// Read an env var, returning `None` when it is unset or empty (so an empty value
/// falls through to the next source rather than becoming a bogus directive).
fn env_non_empty(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
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

    let (writer, worker): (BoxMakeWriter, Option<WorkerGuard>) =
        match (cfg.non_blocking, cfg.writer) {
            (true, OutputStream::Stdout) => {
                let (nb, guard) = tracing_appender::non_blocking(std::io::stdout());
                (BoxMakeWriter::new(nb), Some(guard))
            }
            (true, OutputStream::Stderr) => {
                let (nb, guard) = tracing_appender::non_blocking(std::io::stderr());
                (BoxMakeWriter::new(nb), Some(guard))
            }
            (false, OutputStream::Stdout) => (BoxMakeWriter::new(std::io::stdout), None),
            (false, OutputStream::Stderr) => (BoxMakeWriter::new(std::io::stderr), None),
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
            writer: OutputStream::Stdout,
        };
        assert!(matches!(init(&cfg), Err(TelemetryError::Filter { .. })));
    }

    /// Restores (or clears) an env var on drop, so env-mutating assertions don't
    /// leak into each other. Env access is process-global; these cases run
    /// sequenced in one test so cargo's parallel harness can't race them.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
        fn clear(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn from_env_resolves_level_format_and_defaults() {
        // Defaults when nothing is set.
        {
            let _r = EnvGuard::clear("RUST_LOG");
            let _q = EnvGuard::clear("QE_LOG");
            let _f = EnvGuard::clear("QE_LOG_FORMAT");
            let cfg = TelemetryConfig::from_env();
            assert_eq!(cfg.level, "info");
            assert_eq!(cfg.format, LogFormat::Json);
            assert_eq!(cfg.writer, OutputStream::Stdout);
            assert!(cfg.non_blocking);
        }

        // `RUST_LOG` wins over `QE_LOG`; `QE_LOG_FORMAT=pretty` selects Pretty.
        {
            let _r = EnvGuard::set("RUST_LOG", "warn,qe_wfo=debug");
            let _q = EnvGuard::set("QE_LOG", "trace");
            let _f = EnvGuard::set("QE_LOG_FORMAT", "PRETTY");
            let cfg = TelemetryConfig::from_env();
            assert_eq!(cfg.level, "warn,qe_wfo=debug");
            assert_eq!(cfg.format, LogFormat::Pretty);
        }

        // Empty `RUST_LOG` falls through to `QE_LOG`; unknown format ⇒ Json.
        {
            let _r = EnvGuard::set("RUST_LOG", "");
            let _q = EnvGuard::set("QE_LOG", "error");
            let _f = EnvGuard::set("QE_LOG_FORMAT", "yaml");
            let cfg = TelemetryConfig::from_env();
            assert_eq!(cfg.level, "error");
            assert_eq!(cfg.format, LogFormat::Json);
        }
    }
}
