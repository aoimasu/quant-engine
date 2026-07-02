# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. A PR entry is added here when it
reaches review, the dedicated review agent writes `[Reviewed]`/`[Approved]` + comments inline, and on
merge the approved block is archived to `docs/mds/reviewed/<ticket>.md` and this file is **cleared back
to empty**. No running "Completed" list is kept here — the traceable history lives solely in
`docs/mds/reviewed/`.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

---

## QE-220 — Bootstrap/restart parity test — [Ready-for-review]

- **PR:** #71 — https://github.com/aoimasu/quant-engine/pull/71
- **Ticket:** QE-220 (`Phase: P2` · `Area: ③ risk` · `Depends on: QE-210, QE-211`)
- **Branch:** `qe-220/restart-parity`
- **Latest commit:** `57a416cae17e6f61e934d2f06abfef788fc61250`
- **Evidence / design:** `docs/architecture/qe-220-restart-parity-design.md`
- **Changed files:** `crates/runtime/tests/restart_parity.rs` (new integration test), design note. (Also
  archives QE-219 → `docs/mds/reviewed/qe-219.md` + clears the prior `work.md` entry.)

### Goal
*(Reviewer-added.)* If reconstructed peak/state diverges from continuous state, every drawdown breaker is
mis-anchored — a capital-risk event. Assert bootstrap-reconstructed state equals continuously-running state on
the breaker-relevant fields (committed peak, dormancy latches, positions).

### Acceptance criteria (from backlog)
- [x] Reconstructed vs continuous state match bit-for-bit on breaker-relevant fields —
  `reconstructed_state_matches_continuous_bit_for_bit`.

### Implementation summary
- New integration test `crates/runtime/tests/restart_parity.rs` (no production surface). **Reconstructed** =
  `ReconstructedState::from_replay(final_positions, &outputs, &equity_paths)` (production restart path).
  **Continuous** = an **independent** reference (plain running max for the peak, plain entered-flag for
  dormancy, session's final positions — deliberately not calling `CommittedPeak`/`from_replay`), so agreement
  is a real parity check, not a tautology.
- Scenario: a `cycling_genome` (trades → active, non-flat) + an all-off genome (dormant) over a real
  `EvaluatorSession` (40 bars, past indicator warmup); rise-then-fall equity paths so the true peak is an early
  value the tail is below (guards the trailing-window regression the module warns about).
- **Scrutinise:** (1) is the "independent reference" genuinely independent (plain max / entered-flag), or does
  it smuggle in the production logic it should check against? (2) is 40 bars a robust warmup margin, or brittle
  if the catalogue's max lookback changes? (3) positions share one source (the session) — is asserting that
  field meaningful, and is the non-flat assertion enough to make it non-vacuous? (4) synthetic equity paths
  (length 3, decoupled from bar count) — faithful to "continuous state", or should equity derive from the
  replay? (5) integration test vs a reusable production parity primitive — right call for this AC?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 563 passed / 1 ignored / 57 suites (+4 restart_parity tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok
