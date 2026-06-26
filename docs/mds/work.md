# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.

---

## QE-008 — Clock-skew / time-sync guard — PR #8 — [Approved]

- **Branch:** `qe-008/clock-skew-guard`
- **PR:** https://github.com/aoimasu/quant-engine/pull/8
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-008-clock-skew-guard-design.md`
- **Changed surface:** new crate `crates/clock` (`src/{lib,skew}.rs`, `tests/logging.rs`,
  `Cargo.toml`); root `Cargo.toml` (+`qe-clock` path dep). Also bundles the QE-007 archive
  (`docs/mds/reviewed/qe-007.md`) — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Simulated skew beyond threshold triggers a halt, not a silent continue.
      _(`check` returns `Err(QeError::fatal)` whose `disposition == Halt` — verified; boundary is
      strict (equal=in-sync) both signs; `evaluate` is panic-free. Layer-appropriate: QE-009 consumes
      the Halt disposition.)_
- [x] Skew is logged with the correlation fields and exposed as health state.
      _(`record_skew` logs all four correlation fields + `skew_ms` + `health` (WARN on breach), proven
      by a non-trivial JSON-capture test; `ClockHealth` is also exposed as typed serializable state on
      `SkewReading`, not just logged.)_

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-clock` 7 unit + 1 integration tests pass; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok

Key AC-proving tests:
- **AC #1 (breach → halt, not silent continue)** — `check_returns_fatal_halt_on_breach`
  (simulated skew → `Err` whose `qe_error::disposition == Disposition::Halt`); `check_ok_within_threshold`;
  `breach_beyond_threshold_both_signs` + `in_sync_at_and_below_threshold_both_signs` (boundary at
  `threshold` vs `threshold+1`, both signs); `evaluate_does_not_panic_on_extreme_opposite_instants`.
- **AC #2 (logged with correlation fields + health)** — `tests/logging.rs::record_skew_emits_correlation_and_health`
  captures the JSON log line and asserts `run_id`/`vintage_hash`/`instrument`/`window_id`/`skew_ms`/
  `health="skewed"` and WARN level on breach.

### Design notes for the reviewer
- The hard halt is expressed as QE-004's `QeError::fatal` (→ `Disposition::Halt`), the same channel
  QE-009's concrete kill switch will consume — no new halt path invented. `SkewGuard` is pure: it
  judges injected `(local, reference)` `Timestamp` samples, so "simulated skew" is trivially testable;
  fetching reference time (NTP/venue) is a later integration ticket.
- `ClockHealth { InSync, Skewed }` is the health signal surfaced to the cockpit (QE-304).
- `qe-clock` deps (qe-domain/qe-error/qe-telemetry) are all foundational → QE-001 topology guard green.

### Review notes

**Verdict: [Approved]** — both acceptance criteria genuinely met and the adversarial focus areas all
check out. Clean, pure, well-tested guard with honest layering. A few non-blocking advisories below.

**Independent re-verification (branch `qe-008/clock-skew-guard`):**
- `cargo fmt --all --check` clean · `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean · `cargo test --workspace --locked` **94 passed, 1 ignored** (qe-clock 8: 7 unit + 1
  integration) · `cargo deny check` ok · QE-001 `dependency_topology` guard green (foundational deps
  only).

**Focus area 1 — AC #1 (breach → halt):**
- **Halt is a real halt, at the right layer.** `check` returns `Err(QeError::fatal(…))` and the test
  asserts `disposition(&err) == Disposition::Halt`. Expressing the halt as QE-004's Fatal→Halt
  disposition (which QE-009's kill switch will consume) is a *legitimate* satisfaction of "triggers a
  halt," not under-delivery: QE-008 is the detector and QE-009 (a later ticket) is the enforcer, and
  the contract (`disposition == Halt`) is tested so QE-009 can rely on it. An `Err` can't be silently
  continued past without explicitly discarding it.
- **Boundary is correct.** Breach is **strictly** `|skew| > threshold`; equal = in sync — tested at
  `threshold` (in sync) and `threshold+1` (breach) for **both** signs.
- **`evaluate` is genuinely panic-free.** `saturating_sub` + `unsigned_abs` avoid the overflow/`abs()`
  panics. `evaluate_does_not_panic_on_extreme_opposite_instants` (`i64::MIN` vs `i64::MAX`) *really*
  exercises the path: it passes under `cargo test`'s **debug** profile where integer overflow panics,
  so a naive `-`/`abs()` would have aborted the test — the test is meaningful, not decorative. No
  sign/threshold edge silently continues; saturation always errs toward `Skewed` (fail-safe).

**Focus area 2 — AC #2 (logged + health exposed):**
- The JSON-capture test is **non-trivial**: it parses the real emitted JSON and asserts concrete
  values (`run_id=run-42`, `vintage_hash=vh-abc`, `instrument=BTCUSDT`, `window_id=w7`, `skew_ms=5000`,
  `health="skewed"`) **and** `level == "WARN"` — it fails if any field is dropped or the level is
  wrong. Capturing via a local JSON subscriber + `with_default` (no process-global) is the sound,
  isolated pattern (same as QE-003).
- Health is **exposed as state, not just logged**: `SkewReading.health: ClockHealth` is a public,
  `Serialize`/`Deserialize` field returned from `evaluate`/`check`, so the cockpit reads it as a typed
  value.

**Focus area 3 — soundness / API split:** `evaluate` (always, pure) vs `record_skew` (always logs) vs
`check`/`breach` (halt decision) is a clean separation. `Default` threshold `1000ms` is well-reasoned
(mark price @1s, under a 5s recvWindow). `SkewGuard`'s threshold invariant can't be bypassed — the
field is private, `new` rejects `≤ 0`, and `SkewGuard` is **not** `Deserialize` (so no QE-007-style
serde bypass).

**Non-blocking advisory notes (no action required):**
1. **`check` halts but does not log.** The full "log *and* halt" path requires
   `evaluate → record_skew → breach`; the convenience `check` performs only the halt decision and
   discards the `SkewReading` on breach (skew detail survives in the error message). When QE-009 wires
   the kill path, a `check`-only call site would halt **without** logging the breach — worth either
   having `check` also emit the event, or documenting prominently that `record_skew` must accompany it.
2. The design's "the split *guarantees* a breach is never a silent continue" is slightly overstated —
   the guard *provides* the non-silent path (`Err(Fatal)`/`breach()`), but can't force a caller that
   only calls `evaluate` and ignores the result. Wording.
3. The logging test covers only the breach (WARN) path; the in-sync (INFO) branch of `record_skew` is
   symmetric but untested. Optional to add.
