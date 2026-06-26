//! AC #2 — a skew event is logged with the correlation fields and the health state.

use std::sync::{Arc, Mutex};

use qe_clock::{record_skew, SkewGuard};
use qe_domain::Timestamp;
use qe_telemetry::Correlation;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

/// In-memory writer so we can assert on the emitted JSON without touching the global subscriber.
#[derive(Clone, Default)]
struct BufWriter(Arc<Mutex<Vec<u8>>>);

impl BufWriter {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().expect("lock").clone()).expect("utf8")
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

#[test]
fn record_skew_emits_correlation_and_health() {
    let buf = BufWriter::default();
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(EnvFilter::new("qe::clock=info"))
        .json()
        .with_writer(buf.clone())
        .finish();

    let guard = SkewGuard::new(1_000).unwrap();
    let reading = guard.evaluate(Timestamp::from_millis(5_000), Timestamp::from_millis(0));
    let corr = Correlation {
        run_id: "run-42",
        vintage_hash: "vh-abc",
        instrument: "BTCUSDT",
        window_id: "w7",
    };

    tracing::subscriber::with_default(subscriber, || record_skew(&reading, &corr));

    let line = buf.contents();
    let json: serde_json::Value = serde_json::from_str(line.lines().next().expect("a log line"))
        .expect("valid JSON log line");
    let fields = &json["fields"];

    // All four correlation fields present...
    assert_eq!(fields["run_id"], "run-42");
    assert_eq!(fields["vintage_hash"], "vh-abc");
    assert_eq!(fields["instrument"], "BTCUSDT");
    assert_eq!(fields["window_id"], "w7");
    // ...plus the skew magnitude and the exposed health state.
    assert_eq!(fields["skew_ms"], 5_000);
    assert_eq!(fields["health"], "skewed");
    // A breach logs at WARN level.
    assert_eq!(json["level"], "WARN");
}

#[test]
fn record_skew_in_sync_logs_at_info_with_health() {
    let buf = BufWriter::default();
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(EnvFilter::new("qe::clock=info"))
        .json()
        .with_writer(buf.clone())
        .finish();

    let guard = SkewGuard::new(1_000).unwrap();
    let reading = guard.evaluate(Timestamp::from_millis(100), Timestamp::from_millis(50));
    let corr = Correlation {
        run_id: "run-1",
        vintage_hash: "-",
        instrument: "-",
        window_id: "-",
    };

    tracing::subscriber::with_default(subscriber, || record_skew(&reading, &corr));

    let line = buf.contents();
    let json: serde_json::Value =
        serde_json::from_str(line.lines().next().expect("a log line")).expect("valid JSON");
    // In sync: INFO level, health exposed, skew reported.
    assert_eq!(json["level"], "INFO");
    assert_eq!(json["fields"]["health"], "in_sync");
    assert_eq!(json["fields"]["skew_ms"], 50);
    assert_eq!(json["fields"]["run_id"], "run-1");
}
