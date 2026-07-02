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

## QE-216 — out-of-band kill-switch at venue adapter — [Ready-for-review]

- **PR:** #68 — https://github.com/aoimasu/quant-engine/pull/68
- **Ticket:** QE-216 (`Phase: P2` · `Area: ⑥ Edge gateway / risk` · `Depends on: QE-009, QE-217`)
- **Branch:** `qe-216/venue-kill-switch`
- **Latest commit:** `571f0adb3a7f07dc8cda805b7ad98675b11712d4`
- **Evidence / design:** `docs/architecture/qe-216-venue-kill-switch-design.md`
- **Changed files:** `crates/runtime/src/kill_gate.rs` (new), `crates/runtime/src/lib.rs`, design note.
  (Also archives QE-217 → `docs/mds/reviewed/qe-217.md`.)

### Goal
Implement the QE-009 kill contract at the venue adapter: flatten-and-halt, independent of cockpit and Hedge
Planner; independently testable trigger.

### Acceptance criteria (from backlog)
- [x] Triggering the kill flattens positions and halts submission even with the cockpit/planner down —
  `kill_flattens_position_and_halts_submission`, `out_of_band_trip_via_cloned_handle_flattens`,
  `gate_honours_kill_switch_conformance`.

### Implementation summary
- `VenueKillGate` wraps the QE-217 `VenueSimulator` + a latching `KillHandle`; impls QE-009 `OrderGate`
  (`admit_within_limits=Admit` → default `admit` structurally `FlattenAndHalt` when tripped; passes
  `assert_honours_kill_switch`). `submit()` → `Err(KillHalt)` once tripped; `enforce_kill()` flattens the kept
  position once (closing order from the position alone — no mark/planner needed), then latches `Halted`.
- **Scrutinise:** (1) `enforce_kill` submits the flatten directly to the sim (bypassing the submission halt) —
  is that the correct "the kill flattens" semantics? (2) flatten computed from `current_qty` alone (mark only
  the fill price) — right for a safety path? (3) out-of-band proof via a cloned handle (no planner) — genuine?
  (4) `admit_within_limits=Admit` (sizing = QE-215) — right boundary? (5) `enforce_kill` is caller-driven per
  tick; a caller that never calls it is still halted on submit but won't auto-flatten — acceptable?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 56 suites ok, 0 failed (qe-runtime: 76 tests)
- `cargo-deny check` — advisories/bans/licenses/sources ok

### Feedback

_Re-review of commit `571f0adb` (2026-07-02)._

**F1 — [Blocker] RESOLVED.** `enforce_kill` (`kill_gate.rs:130-143`) now latches `self.flattened = true`
**only inside the `Some(intent)` branch**, after the closing order is actually submitted. A flat/not-yet-known
`current_qty` yields `Flattened(None)` and leaves `flattened == false`, so the gate stays armed and a position
learned on a later tick is still flattened. Verified against the two named failure modes: (a) **no
double-flatten from keeper fill latency** — the latch is set synchronously in the same call as the submit, so
a later tick still reporting the pre-flatten qty short-circuits at the `if self.flattened { return Halted }`
guard; exactly one flatten order can ever be sent; (b) **no spurious never-latch** — the only case that never
latches is a permanently-flat position, which is correct (nothing to flatten; `submit`/`admit` remain
structurally halted). Regression test `flat_first_call_stays_armed_and_still_flattens_a_later_position`
matches the requested case. Accepted.

**F3 — [Nit] RESOLVED.** `OrderGate::kill_reason()` (`gate.rs:69-73`) is now the single source; both
`kill_precheck`'s `FlattenAndHalt` and `submit`'s `KillHalt.reason` call it. `OrderGate` is in scope in
`kill_gate.rs`, so `self.kill_reason()` resolves via the trait default. Accepted.

**F2 — [Nit] PARTIALLY RESOLVED; one residual drift remains (see F4).** D1, the test plan, and Risks are
correctly updated to `flatten_intent`. Accepted for those.

**F4 — [Nit] Residual design drift in D2 — the exact issue F2 was meant to close.** The "Fixes applied"
note claims "Code and design no longer drift," but `qe-216-venue-kill-switch-design.md:69` (§D2) still reads:
"the flatten target is a hard-coded **flat** (`Notional::ZERO`), computed from the kept position alone." That
`Notional::ZERO` parenthetical directly contradicts D1's own deliberate divergence from
`plan_delta(Notional::ZERO, …)` and the implementation, which uses `flatten_intent(current_qty)` and touches
no `Notional` at all. **Fix:** drop the stale `(Notional::ZERO)` reference in D2 (e.g. "computed from the kept
position alone via `flatten_intent`"), so the note is internally consistent with D1. Low severity — the code
is correct; this is a doc-only inconsistency, but it is precisely the drift F2 flagged, so it must be closed
before approval.

### Fixes applied (commit `571f0adb`)

**F1 — resolved.** `enforce_kill` now latches `flattened` **only after** `flatten_intent(current_qty)`
actually produced and submitted a closing order (`kill_gate.rs` `match flatten_intent { Some => submit +
latch, None => Flattened(None), stays armed }`). A flat/not-yet-known `current_qty` at the first post-trip
call no longer disarms the flatten: the gate stays armed until a real position is known and flattened. The
`Some`-branch latch still prevents a double-flatten from keeper fill latency. Regression test
`flat_first_call_stays_armed_and_still_flattens_a_later_position` (exactly the reviewer's requested case:
position → trip → `enforce_kill(0)` → `enforce_kill(0.2)` still flattens, then latches `Halted`). Design-note
Risks updated to cover the reconnect-window scenario.

**F2 — resolved.** Design note D1 + test-plan #4 + Risks updated to match the implementation: the flatten is
computed by `flatten_intent(current_qty)` (mark-free, sized from the kept position alone), with an explicit
rationale for diverging from the earlier `plan_delta(Notional::ZERO, …)` sketch (the safety path must not
depend on a mark to size the flatten). Code and design no longer drift.

**F3 — resolved.** Added `OrderGate::kill_reason()` default method (`crates/risk/src/gate.rs`) as the single
source of the halt-reason string; `kill_precheck`'s `FlattenAndHalt` and `submit`'s `KillHalt.reason` both
call it, so they cannot diverge.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 546 passed / 1 ignored /
56 suites · `cargo deny check` advisories/bans/licenses/sources ok.

### F4 fix (commit pending)

**F4 — resolved.** Agreed — genuine residual drift. Design note §D2 (line 69) no longer references
`Notional::ZERO`; it now reads "the flatten is always to **flat**, computed from the kept position alone via
`flatten_intent(current_qty)` (see D1: no `Notional` target and no mark are needed to size it)", so D2 is
internally consistent with D1 and the code. Doc-only change; full green gate re-run and clean (fmt · clippy ·
`cargo test` 546 passed/1 ignored/56 suites · deny ok).
