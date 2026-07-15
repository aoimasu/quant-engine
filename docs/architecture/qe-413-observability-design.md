# QE-413 — Observability: env-driven telemetry, per-request tracing, CLI telemetry init

Evidence note / design record. Source of truth: `### QE-413` in
`docs/reviews/2026-07-15-team-improvement-review.md`.

## Problem (current state, file:line)

1. **No env override for telemetry.** `qe-server` installs
   `TelemetryConfig::default()` at `crates/server/src/main.rs:16`; the config
   struct (`crates/telemetry/src/lib.rs:51`, `Default` at `:66`) is fixed at
   `level="info"`, `format=Json`, `non_blocking=true`. An operator cannot change
   log level/format without recompiling.
2. **No per-request tracing on the server.** The axum router
   (`crates/server/src/lib.rs:304-320`) nests `/api` and serves static files but
   carries **no** `TraceLayer`/request-id middleware — zero structured
   per-request logging (method/path/status/latency).
3. **CLI never initialises telemetry.** `crates/cli/src/main.rs:39` (`main`) calls
   `run()` directly and never calls `qe_telemetry::init`; `qe-cli` does not even
   depend on `qe-telemetry`. The job pipeline's `tracing` spans (e.g.
   `qe_telemetry::stage_span`, clock-skew correlation) are dropped on the floor.

## Design

### Part 1 — `TelemetryConfig::from_env` + writer selection (`crates/telemetry/src/lib.rs`)

- Add `OutputStream { Stdout, Stderr }` and a `writer: OutputStream` field to
  `TelemetryConfig` (default `Stdout`, preserving existing server behaviour).
- `init()` selects stdout vs stderr from `cfg.writer` for both the blocking and
  non-blocking writer paths (previously hardcoded `std::io::stdout`).
- Add `TelemetryConfig::from_env() -> Self`:
  - **level**: first non-empty of `RUST_LOG`, then `QE_LOG`, else `"info"`
    (standard `RUST_LOG` wins; `QE_LOG` is the project-specific fallback).
  - **format**: `QE_LOG_FORMAT` — `"pretty"` ⇒ `Pretty`, anything else ⇒ `Json`
    (default `Json`). Case-insensitive; empty = unset.
  - **non_blocking**: keeps the default `true`.
  - **writer**: `Stdout` (callers override; the CLI forces `Stderr`).
- Empty env values are treated as unset.

### Part 2 — Per-request tracing on the server (`crates/server/src/lib.rs`)

- Enable tower-http features `trace` + `request-id`; promote `tower` from
  dev-dep to a normal dep (already in the lockfile) for `ServiceBuilder` ordering.
- Wrap **only the `/api` nested router** (static-file serving stays quiet) with,
  outermost→innermost:
  1. `SetRequestIdLayer::x_request_id(MakeRequestUuid)` — stamps `x-request-id`.
  2. `TraceLayer::new_for_http()` with a custom `make_span_with` +
     `on_response` emitting **one span per request** carrying `method`, `path`,
     `request_id`, and (recorded on response) `status` + `latency_ms`.
  3. `PropagateRequestIdLayer::x_request_id()` — echoes the id on the response.
- **Health kept quiet**: `make_span_with` branches on `path.ends_with("/health")`
  → `debug_span!` (and the response event is emitted at `debug`), so a
  production `info` filter suppresses `/api/health` per-request spam while every
  other request logs at `info`. Robust to axum nesting (matches the trailing
  `/health` whether the nested router sees `/health` or `/api/health`).

### Part 3 — CLI telemetry init (`crates/cli/src/main.rs`)

- Add `qe-telemetry` to `qe-cli` `[dependencies]` (firewall-legal: `qe-cli` is a
  composition root, not a firewall `upstream`; `qe-telemetry` is not `forbidden`).
- In `main()` (once, before `run()`), init telemetry with
  `TelemetryConfig { writer: OutputStream::Stderr, ..from_env() }`, holding the
  guard for the process lifetime. Init failure is **non-fatal**: warn to stderr
  and continue (the CLI must still run without telemetry).

## STDOUT-corruption safety (critical)

The server reads the CLI child's **stdout** as the `ProgressLine` JSON run
protocol. The CLI therefore forces telemetry to **stderr** (`OutputStream::Stderr`).
`init` no longer hardcodes stdout; the CLI path writes only to stderr, so tracing
output cannot interleave with the `--json` `ProgressLine` records on stdout. The
`--json` emitters (`emit_progress`/`emit_done`/`emit_error`, `println!`) remain
the sole writers of stdout. Verified by a manual `qe train --json` run asserting
every stdout line parses as JSON while `RUST_LOG=debug` produces stderr spans.

## Double-init safety

`tracing_subscriber::set_global_default` panics only on a second **successful**
install; `init` uses it and returns `TelemetryError::Init` rather than panicking
on the already-set case. CLI init happens exactly once in `main` (not per job).
CLI integration tests call job functions directly (`run_backtest`/`run_train`/
`run_ingest`) and none spawn the `qe` binary to read its stdout
(`dependency_topology.rs` only runs `cargo`), so no test path double-inits. CLI
init is additionally non-fatal, so even an unexpected double-init degrades to a
stderr warning, never a panic.

## Test plan

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked` — incl. `qe-server` router `oneshot` tests
  (TraceLayer is a no-op without an installed subscriber) and `qe-cli` job tests.
- `cargo test -p qe-architecture --test firewall --locked` — the new
  `qe-cli → qe-telemetry` edge stays firewall-legal.
- New unit test: `TelemetryConfig::from_env` level/format resolution (sequenced,
  env-guarded).
- Manual: `qe train --json` → every stdout line is valid JSON with telemetry on.

## Risks

- **Cargo.lock**: enabling tower-http `trace`/`request-id` and promoting `tower`
  to a normal dep may re-record lockfile edges (deps already present: `tracing`,
  `uuid`, `tower`). `deny` runs in CI only (not installed locally) — watch it.
- **Nesting path matching**: mitigated by matching the trailing `/health`.
