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

## QE-008 — Clock-skew / time-sync guard — PR #8 — [Ready-for-review]

- **Branch:** `qe-008/clock-skew-guard`
- **PR:** https://github.com/aoimasu/quant-engine/pull/8
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-008-clock-skew-guard-design.md`
- **Changed surface:** new crate `crates/clock` (`src/{lib,skew}.rs`, `tests/logging.rs`,
  `Cargo.toml`); root `Cargo.toml` (+`qe-clock` path dep). Also bundles the QE-007 archive
  (`docs/mds/reviewed/qe-007.md`) — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [ ] Simulated skew beyond threshold triggers a halt, not a silent continue.
- [ ] Skew is logged with the correlation fields and exposed as health state.

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
