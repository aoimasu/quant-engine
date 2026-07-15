//! QE-406 agreement test — the CI guard that the emit side and the parse side of the run protocol
//! agree on the wire, and that the wire is frozen.
//!
//! Each case pins the **exact** on-wire JSON for a `ProgressLine` variant. Serializing the value must
//! produce that string (catches an emit-side field rename / reorder / tag change), and deserializing
//! that string must reproduce the value (catches a parse-side rename). Because `qe-cli` emits and
//! `qe-server` parses **this same type**, freezing the bytes here freezes the contract for both — a
//! field rename in either crate would change this type and fail a case below.

use qe_run_protocol::{
    emit_done, emit_error, emit_progress, emit_train_done, ProgressLine, PROTOCOL_VERSION,
};

/// Round-trip a value against its exact wire string: value → JSON == wire, and wire → value == value.
fn assert_wire(value: &ProgressLine, wire: &str) {
    let serialized = serde_json::to_string(value).expect("serialize");
    assert_eq!(serialized, wire, "emit side drifted from the frozen wire");
    let parsed: ProgressLine = serde_json::from_str(wire).expect("deserialize");
    assert_eq!(&parsed, value, "parse side drifted from the frozen wire");
}

#[test]
fn progress_wire_is_frozen() {
    assert_wire(
        &ProgressLine::Progress {
            pct: 50,
            stage: "features".to_owned(),
            msg: "assembling".to_owned(),
        },
        r#"{"t":"progress","pct":50,"stage":"features","msg":"assembling"}"#,
    );
}

#[test]
fn gen_wire_is_frozen_with_finite_best_fitness() {
    assert_wire(
        &ProgressLine::Gen {
            pct: 30,
            stage: "search".to_owned(),
            generation: 1,
            generations: 2,
            coverage: 3,
            coverage_long: 2,
            coverage_short: 1,
            best_fitness: Some(1.5),
        },
        r#"{"t":"gen","pct":30,"stage":"search","generation":1,"generations":2,"coverage":3,"coverage_long":2,"coverage_short":1,"best_fitness":1.5}"#,
    );
}

#[test]
fn ensemble_and_gate_wire_are_frozen() {
    assert_wire(
        &ProgressLine::Ensemble {
            pct: 75,
            stage: "ensemble".to_owned(),
            folds: 4,
            members: 3,
            score: Some(0.42),
        },
        r#"{"t":"ensemble","pct":75,"stage":"ensemble","folds":4,"members":3,"score":0.42}"#,
    );
    assert_wire(
        &ProgressLine::Gate {
            pct: 85,
            stage: "gate".to_owned(),
            promoted: true,
            failed: vec![],
            in_sample_sharpe: Some(1.5),
            holdout_sharpe: Some(1.1),
            dsr: Some(0.8),
            spa_pvalue: Some(0.03),
            n_trials: 12,
        },
        r#"{"t":"gate","pct":85,"stage":"gate","promoted":true,"failed":[],"in_sample_sharpe":1.5,"holdout_sharpe":1.1,"dsr":0.8,"spa_pvalue":0.03,"n_trials":12}"#,
    );
}

#[test]
fn done_and_error_wire_are_frozen() {
    // Backtest form (no vintage) — carries the current protocol version.
    assert_wire(
        &ProgressLine::Done {
            result: "result.json".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: None,
        },
        r#"{"t":"done","result":"result.json","protocol_version":1}"#,
    );
    // Train form — names the sealed vintage.
    assert_wire(
        &ProgressLine::Done {
            result: "result.json".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: Some("vintage-abc123".to_owned()),
        },
        r#"{"t":"done","result":"result.json","protocol_version":1,"vintage":"vintage-abc123"}"#,
    );
    assert_wire(
        &ProgressLine::Error {
            msg: "boom".to_owned(),
        },
        r#"{"t":"error","msg":"boom"}"#,
    );
}

/// The server's tolerance of non-finite floats: `serde_json` renders a non-finite `f64` as `null` on
/// serialize, and the `Option<f64>` field round-trips it as `None` (a required `f64` would drop the
/// whole line). This is the exact behaviour live run-monitoring depends on.
#[test]
fn non_finite_floats_serialize_to_null_and_parse_to_none() {
    let line = ProgressLine::Gen {
        pct: 10,
        stage: "search".to_owned(),
        generation: 1,
        generations: 4,
        coverage: 0,
        coverage_long: 0,
        coverage_short: 0,
        best_fitness: Some(f64::NEG_INFINITY),
    };
    let wire = serde_json::to_string(&line).expect("serialize");
    assert!(
        wire.contains(r#""best_fitness":null"#),
        "non-finite best_fitness must serialize to null: {wire}"
    );
    let parsed: ProgressLine = serde_json::from_str(&wire).expect("deserialize");
    match parsed {
        ProgressLine::Gen { best_fitness, .. } => assert_eq!(best_fitness, None),
        other => panic!("expected Gen, got {other:?}"),
    }
}

/// A terminal `done` line that predates QE-406 (no `protocol_version`) still parses — defaulting to
/// `0`, distinct from every real [`PROTOCOL_VERSION`], so the server can detect and warn rather than
/// drop the terminal line.
#[test]
fn legacy_done_without_protocol_version_parses_to_zero() {
    let parsed: ProgressLine =
        serde_json::from_str(r#"{"t":"done","result":"result.json"}"#).expect("deserialize");
    match parsed {
        ProgressLine::Done {
            protocol_version, ..
        } => {
            assert_eq!(protocol_version, 0);
            assert_ne!(protocol_version, PROTOCOL_VERSION);
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

/// The emit helpers produce exactly the frozen terminal wire (what the CLI writes on stdout).
#[test]
fn emit_helpers_match_the_frozen_wire() {
    let mut buf = Vec::new();
    emit_progress(&mut buf, 5, "load", "loading").unwrap();
    emit_done(&mut buf, "result.json").unwrap();
    emit_train_done(&mut buf, "result.json", "vintage-abc123").unwrap();
    emit_error(&mut buf, "boom").unwrap();
    let out = String::from_utf8(buf).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            r#"{"t":"progress","pct":5,"stage":"load","msg":"loading"}"#,
            r#"{"t":"done","result":"result.json","protocol_version":1}"#,
            r#"{"t":"done","result":"result.json","protocol_version":1,"vintage":"vintage-abc123"}"#,
            r#"{"t":"error","msg":"boom"}"#,
        ]
    );
}
