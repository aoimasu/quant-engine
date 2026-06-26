# QE-003 — Structured logging & tracing — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Goal.** Structured, low-overhead observability for both long offline runs and the
latency-sensitive runtime.

**Acceptance criteria.**
- A training run emits **spans for each stage** with the **correlation fields** populated.
- Logging the runtime hot path adds **no blocking I/O** on the order-emission path.

**Out of scope.** Metrics dashboards / cockpit (QE-304); alerting SLAs (QE-305).

## Current-state evidence

- Post-QE-002 workspace: `domain`, `config`, `signal`, `storage`, `ingest`, `wfo`, `ensemble`,
  `venue`, `runtime`, `cli`. No telemetry crate. `tracing` requested in QE-003 scope.

## Design decisions

### New crate `qe-telemetry` (`crates/telemetry`)

Cross-cutting infra; no internal-crate deps (depends only on QE-001 per the ticket). Naming:
`telemetry` to avoid confusion with the spec's "Observability" subgraph (the cockpit, QE-304).
Does not affect the decoupling invariant; QE-001 topology guard still holds.

### Subscriber

- Built on `tracing` + `tracing-subscriber` (`env-filter`, `json`) + `tracing-appender`
  (non-blocking writer).
- `TelemetryConfig { level: String, format: LogFormat, non_blocking: bool }`. `level` is an
  `EnvFilter` directive (e.g. `"info,qe_wfo=debug"`) giving **per-module levels**. `format` is
  `Json | Pretty`. Kept self-contained (no `qe-config` dep) to respect the QE-001-only
  dependency; a later wiring ticket connects config → telemetry.
- `init(&TelemetryConfig) -> Result<TelemetryGuard, TelemetryError>` sets the global default
  subscriber and returns a guard owning the `tracing-appender` `WorkerGuard` (caller keeps it
  alive). When `non_blocking`, log writes are handed to a background thread.

### Correlation fields

`Correlation { run_id, vintage_hash, instrument, window_id }` and
`stage_span(stage, &Correlation) -> tracing::Span` opening an `info`-level span named `stage`
with all four fields recorded. Training stages wrap their work in this span so every event under
it inherits the correlation context (AC #1). `instrument`/`window_id` use `"-"` when not
applicable (e.g. a whole-run stage).

### Hot-path guarantee (AC #2)

Two mechanisms, both documented and the first one tested:
1. **Disabled events cost ~nothing and perform no I/O.** Hot-path emissions use a dedicated
   target (e.g. `qe::hot_path`) at `trace`/`debug`; production filters disable them, so the
   event macro short-circuits before formatting/writing — *no* write, no allocation, no I/O.
   A test builds a subscriber at `info` (trace disabled) and asserts the capture writer receives
   zero bytes from a `trace!` on the hot-path target.
2. **Enabled logging is non-blocking.** When hot-path logging is turned on, `init(non_blocking)`
   routes writes through `tracing-appender`'s background worker, so the emitting thread never
   blocks on the writer — the order-emission path does no synchronous I/O.

### Testing approach

- A private `BufWriter(Arc<Mutex<Vec<u8>>>)` implementing `io::Write` + `MakeWriter` captures
  JSON output. Tests use `tracing::subscriber::with_default` with a locally-built subscriber
  (avoids the process-global `init`, so tests are isolated and parallel-safe).
- AC #1: enter a `stage_span`, emit an event, assert the JSON contains `stage`, `run_id`,
  `vintage_hash`, `instrument`, `window_id`.
- AC #2: trace-on-disabled-target produces empty output (no I/O).

## New workspace dependencies

`tracing`, `tracing-subscriber` (`env-filter`, `json`), `tracing-appender`, `thiserror` (already
present).

## Risks

- **Global subscriber is process-once:** `init` can only be called once; tests must use
  `with_default`. Documented; `init` returns a clear error if the global is already set.
- **JSON layer type differences:** `.json()` vs pretty produce different builder types; `init`
  boxes to `dyn Subscriber` to unify. Tests construct concrete subscribers to avoid boxing
  `with_default` friction.
- **Non-blocking writer drop:** if the `WorkerGuard` is dropped early, buffered logs are flushed
  on drop; callers must hold `TelemetryGuard` for the program lifetime (documented).
