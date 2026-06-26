# QE-004 — Error model & result conventions — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Goal.** Consistent error handling that distinguishes recoverable (retry/skip) from fatal
(halt) conditions — load-bearing for the runtime's halt/kill semantics.

**Acceptance criteria.**
- A `Fatal` error on the runtime path is routed to a halt, not a panic.
- Clippy gate rejects `unwrap`/`expect`/`panic` in designated hot-path modules.

**Out of scope.** Specific retry policies (defined per-ticket where used).

## Current-state evidence

- Post-QE-003 workspace. `qe-config` and `qe-telemetry` each currently define their own local
  error enums (`ConfigError`, `TelemetryError`) — fine; QE-004 provides the *shared* taxonomy and
  conventions other crates adopt, without forcing those local errors to disappear.
- Lints today: workspace `unsafe_code = "deny"`, `clippy::all = warn`. No panic/unwrap restriction.

## Design decisions

### New crate `qe-error` (`crates/error`)

Cross-cutting, no internal deps (QE-001 only). Provides the shared error **taxonomy** and the
`Result` alias other crates use.

### Taxonomy

`ErrorClass` — the recoverability dimension that drives control flow:
- `Transient` — retryable (timeout, rate-limit, transient I/O).
- `Data` — skip/quarantine the offending datum, continue (bad row, parse error).
- `Fatal` — unrecoverable; the runtime must halt (not panic).

`QeError`:
- carries `class: ErrorClass`, a `context: String` (human message), and an optional
  `source: Option<Box<dyn std::error::Error + Send + Sync>>`.
- constructors `transient(msg)`, `data(msg)`, `fatal(msg)`, plus `with_source(err)`.
- `#[non_exhaustive]`-friendly via the class enum (new classes unlikely; keep three).
- `is_fatal()`, `is_retryable()` helpers; `class()` accessor.
- implements `std::error::Error` + `Display`; built with `thiserror`.

`pub type Result<T, E = QeError> = std::result::Result<T, E>;`

### Halt routing (AC #1)

A small control-flow type the runtime uses at the top of its loop:
`enum Disposition { Continue, Retry, Halt }` and
`fn disposition(err: &QeError) -> Disposition` mapping `Transient→Retry`, `Data→Continue`,
`Fatal→Halt`. The runtime's order-emission loop calls this and on `Halt` triggers the
kill/halt path (QE-009 contract) — **never panics**. QE-004 ships the mapping + a unit test that
a `Fatal` error yields `Disposition::Halt` (and `transient`/`data` do not), which is the testable
core of "routed to a halt, not a panic". Actual wiring into the runtime loop is a runtime ticket;
QE-004 owns the decision function and its guarantee.

### Hot-path lint gate (AC #2) — as shipped

Per-module enforcement that `unwrap`/`expect`/`panic` are rejected in designated hot-path code:
- **Convention:** hot-path modules carry the inner attribute block
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`. Documented in the crate
  docs as the standard to copy; the `qe-error::hot_path` module is a clean demonstrator.
- **Automated proof (chosen over `trybuild`):** `clippy::*` are clippy-only lints, so `trybuild`
  (which drives `rustc`) can't prove them. Instead, an **excluded fixture crate**
  (`crates/error/tests/fixtures/hotpath_violation`, `exclude`d from the workspace) deliberately
  contains an `unwrap()` inside a `#![deny(clippy::unwrap_used)]` module. The integration test
  `tests/hot_path_lint.rs` runs `cargo clippy` against that fixture (fresh `CARGO_TARGET_DIR` to
  avoid stale-cache passes) and asserts a non-zero exit with the `unwrap_used` lint. This is the
  honest, self-contained `cargo test`-time proof that the gate rejects the violation.
- The real enforcement in normal code is the QE-005 CI clippy gate (`-D warnings`) compiling
  hot-path modules under their deny block.

## New workspace dependencies

`thiserror` (present). No `trybuild` — the lint proof shells out to `cargo clippy` on the
excluded fixture crate instead (clippy-only lints can't be proven via rustc/trybuild).

## Test plan (TDD)

1. `disposition(fatal(..)) == Halt`; `Transient == Retry`; `Data == Continue`. (AC #1 core)
2. `is_fatal`/`is_retryable` correctness; `with_source` preserves the source chain.
3. Hot-path lint: a compile-fail/clippy test proving `unwrap()` in a hot-path module is rejected.
   (AC #2)
4. Gates: fmt/clippy/build/test green; QE-001 topology guard green.

## Risks

- **Clippy-only lints aren't build errors:** `unwrap_used`/`expect_used`/`panic` are clippy
  restriction lints, so the *gate* is `cargo clippy -D ...`, enforced in CI (QE-005). Resolved:
  the `tests/hot_path_lint.rs` integration test shells out to `cargo clippy` on the excluded
  fixture crate and asserts it fails with `unwrap_used` — a self-contained `cargo test`-time proof.
- **Clippy subprocess in a test:** the lint test spawns `cargo clippy` (needs the clippy
  component, present via `rust-toolchain.toml`) with a fresh `CARGO_TARGET_DIR`. Slightly slower
  and environment-dependent, but the honest way to prove a clippy gate; if clippy were absent the
  test would fail loudly rather than silently pass.
- **Scope creep:** keep `QeError` minimal (3 classes + source). Don't model retry policy.
