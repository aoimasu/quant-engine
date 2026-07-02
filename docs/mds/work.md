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

## QE-218 — gRPC transport (Hedge Planner ↔ Edge gateway) — [Ready-for-review]

- **PR:** #69 — https://github.com/aoimasu/quant-engine/pull/69
- **Ticket:** QE-218 (`Phase: P2` · `Area: ⑤↔⑥` · `Depends on: QE-214, QE-217`)
- **Branch:** `qe-218/grpc-transport`
- **Latest commit:** `3ff14ff7b5959e9c746b2973f1f8cc86c983c3b2`
- **Evidence / design:** `docs/architecture/qe-218-grpc-transport-design.md`
- **Changed files:** `crates/runtime/src/transport.rs` (new), `crates/runtime/src/lib.rs` (module +
  re-exports), design note. (Also archives QE-216 → `docs/mds/reviewed/qe-216.md` + clears the prior
  `work.md` entry.)

### Goal
Decisions flow planner→adapter over gRPC; fills/positions/heartbeat flow back. Backpressure and reconnection
handled; the QE-301 journal-append path must **never gate** the dispatch.

### Acceptance criteria (from backlog)
- [x] A target revision reaches the adapter and fills/positions return; the append path (QE-301) never gates
  this dispatch — `target_revision_reaches_adapter_and_fills_return`, `append_never_gates_dispatch`.

### Implementation summary
- New `crates/runtime/src/transport.rs`: `PlannerAdapterLink<A: AppendSink>` — a **deterministic,
  single-threaded, pull-based** model of the planner↔adapter gRPC bidi stream. `TargetRevision` (absolute,
  monotonic `seq`) in; `AdapterReport::{Fill, Position, Heartbeat{ack_seq, health}}` (with `VenueHealth`) out.
- `pump()` → `plan_delta` vs the authoritative kept position → submit **through the QE-216 `VenueKillGate`**
  → absorb the fill into `VenueKeeper` → return fills + authoritative position + heartbeat.
- **Backpressure = coalesce-to-latest** (`submit_target` keeps only the newest revision; `dropped_superseded`
  observable) — lossless *because* `TargetPosition` is absolute.
- **Reconnection = re-snapshot + re-send latest** (`disconnect`/`reconnect`) — re-sending the latest absolute
  target is idempotent (`plan_delta` → 0 delta → no double-fill).
- **Append never gates dispatch:** the `AppendSink` (QE-301 seam) is *offered* the already-produced reports;
  its `Result` is counted (`append_failures`) but **cannot alter** the dispatch. Real tonic/gRPC wire deferred
  to the runtime binary (QE-201/202 offline-core convention); no new workspace dep; firewall unaffected.
- **Scrutinise:** (1) coalescing backpressure **drops** superseded absolute targets — is "lossless because
  absolute" fully sound (e.g. does any consumer need intermediate revisions)? (2) reconnection re-snapshots
  from the **sim** `position_report` — right source of truth vs the keeper? (3) `append_never_gates_dispatch`
  proven structurally (return value produced before `append`) — is that a genuine proof of the AC, or does a
  real async journal need more? (4) position report sourced from `gate.simulator()` while `plan_delta` reads
  the `keeper` — are sim and keeper guaranteed in sync on this path? (5) `keeper_mut()` exposed for the
  mark/account streams — acceptable encapsulation?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 552 passed / 1 ignored / 56 suites (+6 transport tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok

### Feedback

_First review pass, commit `2e2986c8` (2026-07-02). What is correct: AC #1 holds — a target revision reaches
the adapter and `Fill`+`Position` return (`target_revision_reaches_adapter_and_fills_return`), and the QE-301
append path is **structurally** non-gating (the reports are computed and returned before `append` is offered
them, and `append`'s `Result` only bumps a counter — `append_never_gates_dispatch` proves the identical
reports under a `FailingAppendSink`). Backpressure coalesce-to-latest is sound: dropping a superseded
**absolute** target is genuinely lossless for position convergence. Reconnect idempotence holds *in this
model* (zero delta on re-send). No `tokio`/`tonic`/`threads`, no new workspace dependency, firewall
unaffected. Three items below._

**F1 — [Blocker] Heartbeat health misreports `Ok` while the kill is tripped when the delta is zero.**
`apply_revision` (`transport.rs:255-266`) initialises `health = VenueHealth::Ok` and only ever sets
`VenueHealth::Down` **inside** the `if let Some(intent) = plan_delta(...)` block, on a failed `gate.submit`.
So when the position is already **at target** (zero delta) and the kill is tripped, no submit is attempted
and the heartbeat is emitted with `health: Ok` — telling the planner the venue is healthy while submission is
in fact halted. This directly contradicts `VenueHealth::Down`'s own doc ("Submission is halted (reason) —
e.g. the QE-216 kill switch is tripped") and defeats the purpose of the health back-channel: in steady state
(planner re-sending the same absolute target it already reached) a tripped kill would be reported `Ok` on
**every** heartbeat, so the out-of-band halt is invisible to the planner over the wire. The existing
`kill_tripped_dispatch_halts_submission_on_the_wire` test only exercises the non-zero-delta case (flat → target
10 000), which is why it passes. **Fix direction:** derive `health` from the gate/kill state directly (e.g.
`if self.gate.kill().is_tripped() { Down(reason) }`) independent of whether a delta happened to be submitted
this tick, so health reflects venue submission state, not the side effect of a delta. Add a regression test:
reach a target, trip the kill, re-send the **same** (at-target) revision → `pump` heartbeat must be
`Down`, not `Ok`.

**F2 — [Nit] Reconnect re-snapshots from the sim but idempotence rests on the keeper; the two are only
coincidentally in sync.** `reconnect` (`transport.rs:241-247`) returns `Position` from
`gate.simulator().position_report(...)`, while resume idempotence depends on `plan_delta` reading
`keeper.signed_qty()` (`apply_revision:253`). These agree only because every sim fill is applied to the keeper
(`apply_revision:260`) and nothing else moves the keeper's position — an invariant nothing structurally
enforces (see F3). `reconnect` also does **not** reconcile the keeper to the snapshot it returns, so if sim and
keeper ever diverge, the snapshot handed to the planner and the truth `plan_delta` plans against would differ
and the "zero delta ⇒ no double-fill" argument breaks. Recommend sourcing the reconnect snapshot and the
planning position from a single truth (or asserting `sim_position == keeper.signed_qty()` on the pump/reconnect
path and documenting the invariant). Not wrong in the tested model, but the coupling is load-bearing and
implicit.

**F3 — [Nit] `keeper_mut()` leaks full mutable access to the authoritative keeper.** `keeper_mut`
(`transport.rs:154-156`) hands out `&mut VenueKeeper` guarded only by a doc-comment ("only mark + balances").
Nothing prevents a caller applying a `Fill`/`Position` event that moves the kept position without a
corresponding sim order, which is exactly the desync F2 warns about. Prefer narrow accessors for the intended
mark/balance updates (e.g. delegate `observe_mark`/balance mutation) over a blanket `&mut` handle, so the
sim-as-execution / keeper-as-truth invariant can't be violated from outside the transport.

_Answers to the Scrutinise list: (1) coalescing lossless — **yes** for position convergence; note only that
intermediate revisions are also never seen by the QE-301 append sink (only pumped revisions are journalled),
so a full audit trail of planner decisions would need the dropped ones recorded elsewhere — out of scope for
this AC but worth a design note. (2) reconnect snapshot source — see F2. (3) append non-gating structural
proof — **genuine**; holds under any async journal because the return value precedes and is independent of
`append`. (4) sim vs keeper sync — **coincidental, not guaranteed**; see F2/F3. (5) `keeper_mut` encapsulation
— see F3._

### Fixes applied (commit `3ff14ff7`)

**F1 — resolved.** `apply_revision` now derives heartbeat health from the kill directly —
`if self.gate.kill().is_tripped() { VenueHealth::Down(self.gate.kill_reason()) } else { VenueHealth::Ok }` —
independent of whether a delta was submitted this tick. So an at-target (zero-delta) revision while the kill is
tripped reports `Down`, keeping the out-of-band halt visible in steady state. `kill_reason()` reuses the QE-009
`OrderGate` fallback so the heartbeat reason matches `KillHalt.reason`. Regression test
`at_target_revision_reports_down_while_killed` (reach target → trip kill → re-send same at-target revision →
heartbeat `Down`, no fill, no new order).

**F2 — resolved.** `reconnect()` now **reconciles** the venue snapshot into the keeper
(`keeper.apply(&UserDataEvent::Position(report))`, QE-217 D3) before returning it, so the snapshot the planner
receives and the position `plan_delta` re-plans against are one single truth — a divergence would be corrected
to venue truth on reconnect, not silently split. Test 4 still green (no double-fill on resume).

**F3 — resolved.** Removed `keeper_mut()`; the transport now exposes only the narrow `observe_mark(Price)` and
`observe_balance(equity, avail)` (venue-truth mark/balance feeds). No caller can apply a `Fill`/`Position` that
moves the kept position without a corresponding sim order, so the sim-as-execution / keeper-as-truth invariant
F2 relies on cannot be violated from outside the transport.

**Scrutinise #1 (audit trail of dropped revisions) — noted in the design note Risks** (backpressure section):
coalesced/superseded revisions are not journalled; a full planner-decision audit trail would record them
separately. Out of scope for this AC (position convergence is lossless), flagged for QE-301.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 553 passed / 1 ignored /
56 suites (+1 F1 regression) · `cargo test -p qe-architecture --test firewall` 1 passed · `cargo deny check`
ok.
