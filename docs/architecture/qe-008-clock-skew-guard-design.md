# QE-008 — Clock-skew / time-sync guard — design / evidence

## Ticket

`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-004`

**Goal.** Funding timestamps, the 60s mark EMA, bar-close evaluation, and signed-request windows all
assume a trustworthy clock; skew on a leveraged venue causes wrong funding accrual, mis-timed
breakers, and rejected requests.

**Scope / requirements.**
- Monitor local-vs-reference (NTP / venue server time) skew.
- Hard halt (via QE-009 kill path) when skew exceeds a configured threshold.
- Surface skew as a health signal to the cockpit (QE-304).

**Out of scope.** Time-series alignment in fusion (QE-104).

**Acceptance criteria.**
- Simulated skew beyond threshold triggers a halt, not a silent continue.
- Skew is logged with the correlation fields and exposed as health state.

## Current-state evidence

- **QE-004 (`qe-error`)** gives the halt mechanism: `QeError::fatal(ctx)` has `ErrorClass::Fatal`, and
  `disposition(&err) == Disposition::Halt` ("routed to the kill/halt path; never a panic"). So a skew
  breach is expressed as a Fatal `QeError` — the runtime's existing disposition routing turns that
  into a halt. QE-009's concrete kill switch will consume that same `Halt` disposition; no new halt
  channel is invented here.
- **QE-003 (`qe-telemetry`)** gives `Correlation { run_id, vintage_hash, instrument, window_id }` for
  structured logging (AC #2's "logged with the correlation fields").
- **QE-007 (`qe-domain`)** gives `Timestamp` (epoch-ms, UTC) — the local & reference instants use it,
  so the guard speaks the shared time vocabulary rather than raw `i64`.
- No NTP/venue client exists yet (that wiring is QE-venue/runtime). QE-008 therefore delivers the
  **threshold/health/halt logic given (local, reference) samples** — which is exactly what makes
  "simulated skew" testable — and leaves *fetching* reference time to the integration tickets.

## Design decisions

New crate `qe-clock` (`crates/clock`):

### `skew.rs`
- `SkewGuard { max_abs_skew_ms }` — `new(max_abs_skew_ms) -> Result<_, ClockError>` rejects a
  non-positive threshold. `DEFAULT_MAX_SKEW_MS = 1000` (mark price @1s; well under a 5s recvWindow).
- `evaluate(local: Timestamp, reference: Timestamp) -> SkewReading` — **pure, total, no panic**:
  `skew_ms = local − reference` (saturating; magnitude via `unsigned_abs`, so even extreme inputs
  can't panic). `SkewReading { skew_ms, threshold_ms, health }` where
  `ClockHealth { InSync, Skewed }`. The reading is the **health signal** surfaced to the cockpit.
- `SkewReading::breach() -> Option<QeError>` → `Some(QeError::fatal(...))` when `Skewed` (so
  `disposition == Halt`); `None` when in sync. `SkewGuard::check(local, reference) -> Result<…>` is
  the ergonomic "Ok in sync / Err(Fatal) on breach" wrapper, built on `evaluate` + `breach`.
- `record_skew(&SkewReading, &Correlation)` emits a structured `tracing` event (target
  `qe::clock`) carrying **all four correlation fields + `skew_ms` + `health`** — `warn` on breach,
  `info` otherwise. This is AC #2's "logged with the correlation fields".

The split (always-`evaluate` → always-`record_skew` → optional `breach`/halt) guarantees a breach is
**never a silent continue**: the reading is logged and the health is `Skewed` regardless, and the
caller routes the Fatal error to the halt path.

### `lib.rs`
Crate docs + `ClockError` (thiserror; `InvalidThreshold`) + re-exports.

### Dependencies
`qe-domain` (Timestamp), `qe-error` (Fatal/Halt), `qe-telemetry` (Correlation), `tracing`, `serde`
(health/reading serialisation for the cockpit), `thiserror`. Dev: `serde_json`,
`tracing-subscriber` (capture-and-assert the log event). All foundational; `qe-clock` is a neutral
shared crate, so the QE-001 topology guard is unaffected.

## Test plan (proves both ACs)

- **AC #1 (breach → halt, not silent continue):**
  - `check_returns_fatal_halt_on_breach` — simulated skew beyond threshold → `Err`, and
    `qe_error::disposition(&err) == Disposition::Halt`.
  - `check_ok_within_threshold` — small skew → `Ok(InSync)`.
  - `evaluate_marks_health_skewed_on_breach` / `breach_is_some_on_skew` — health is `Skewed` and a
    Fatal error is produced (not swallowed). Boundary test at exactly `threshold` (in sync) and
    `threshold + 1` (breach), for positive **and** negative skew.
- **AC #2 (logged with correlation fields + health):** `record_skew_emits_correlation_and_health` —
  install a local JSON subscriber, call `record_skew` for a skewed reading, parse the captured line,
  assert `run_id`/`vintage_hash`/`instrument`/`window_id`/`skew_ms`/`health="skewed"` are all present.
- Construction: `new` rejects `0`/negative thresholds; `unsigned_abs` path tested for no-panic on
  large opposite-sign instants.

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **Halt is expressed, not yet enforced end-to-end:** the concrete kill switch is QE-009; QE-008
  produces the Fatal/`Halt` disposition the kill path will consume. Documented; the contract
  (`disposition == Halt`) is tested so QE-009 can rely on it.
- **No reference-time client yet:** by design — `evaluate` takes injected samples, keeping the guard
  deterministic and unit-testable; real NTP/venue time is wired in a later integration ticket.
- **Overflow on extreme instants:** avoided via `saturating_sub` + `unsigned_abs` (no `abs()` panic).
