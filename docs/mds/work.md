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

**F1 — [Blocker] `enforce_kill` latch-once disarms the flatten on a transiently-flat first call.**
`crates/runtime/src/kill_gate.rs:128` sets `self.flattened = true` unconditionally, before it is known
whether `flatten_intent` produced a closing order:

```rust
self.flattened = true;
let fill = flatten_intent(current_qty).map(|intent| self.sim.submit(intent, fill_price, event_time_ms));
```

If `current_qty == 0` at the first post-trip call — e.g. a watchdog trips during a reconnect window where the
QE-217 `VenueKeeper` has reset and not yet re-absorbed the authoritative position snapshot (the keeper "never
infers") — no closing order is sent, yet the gate latches. On a later tick, once the real position is known,
`enforce_kill` returns `Halted` and never flattens it. Because `submit` is already halted, any position the
keeper learns about after the trip necessarily existed at trip time, so this is a genuine
open-position-never-flattened hole on the safety path. The design-note risk (design §Risks) only addresses a
caller that *never* calls `enforce_kill`; it does not cover this. **Fix direction:** latch only after a
non-flat position has actually been flattened (do not set `flattened` when `flatten_intent` is `None`), while
still guarding against a double-flatten caused by keeper fill latency. Add a regression test: establish a
position, trip, call `enforce_kill(current_qty = 0, …)` once, then `enforce_kill(current_qty = 0.2, …)` and
assert the flatten still occurs.

**F2 — [Nit] Implementation diverged from the design note.** Design §Design (design note line 47) specifies
`plan_delta(Notional::ZERO, current_qty, mark)`; the code instead hand-rolls `flatten_intent(current_qty)`.
The rationale (the safety path shouldn't need a mark to compute size) is reasonable — arguably better — but
update the design note so code and design don't drift.

**F3 — [Nit] Duplicated kill-reason fallback.** `submit` (`kill_gate.rs:103-106`) re-implements the
`reason().unwrap_or_else(|| "kill switch tripped".to_owned())` fallback already encoded in the QE-009
`OrderGate::kill_precheck`. Centralize so `submit`'s `KillHalt.reason` and `admit`'s `FlattenAndHalt` reason
stay consistent from one source.

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
