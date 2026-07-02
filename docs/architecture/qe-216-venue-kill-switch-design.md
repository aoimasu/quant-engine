# QE-216 — Out-of-band kill-switch at venue adapter — design note

`Phase: P2` · `Area: ⑥ Edge gateway / risk` · `Depends on: QE-009, QE-217` · `Branch: qe-216/venue-kill-switch`

## Goal (from backlog)

A cockpit button dependent on the cockpit process is not a kill-switch; the halt must be out-of-band, at the
order-submission layer, deterministic.

- **Scope.** Implement the QE-009 kill contract **at the venue adapter**: flatten-and-halt, independent of
  cockpit and Hedge Planner; independently testable trigger.
- **Out of scope.** Alerting (QE-305).

**Acceptance criteria.**
- [ ] Triggering the kill **flattens positions and halts submission** even with the cockpit/planner down.

`Spec ref: Runtime — Edge gateway submits orders; reviewer: out-of-band kill.`

## Current-state evidence & placement

- QE-009 defines the contract (`crates/risk/src/gate.rs`, `crates/risk/src/kill.rs`): the **latching**,
  cloneable `KillHandle`/`KillSwitch` (trippable from anywhere), the `OrderGate` trait whose default
  `admit` applies `kill_precheck` **structurally** — a tripped switch always yields
  `Admission::FlattenAndHalt` — and the reusable conformance check `assert_honours_kill_switch`. QE-009's own
  docs say enforcement "is implemented by QE-215/216". QE-216 is that implementation for the **kill**.
- QE-217 (`crates/runtime/src/edge.rs`) provides the venue adapter: `VenueSimulator` (order submission),
  `plan_delta` (target → delta), and `VenueKeeper` (authoritative position).
- **Placement: new `crates/runtime/src/kill_gate.rs`.** It wraps the QE-217 venue adapter with the QE-009
  kill contract — needs both `qe_risk` (kill/gate) and `crate::edge`. `qe-runtime` already depends on
  `qe-risk`; no new dependency, QE-132 firewall unaffected.

## Design

### D1 — `VenueKillGate` — the kill contract at the submission layer

```rust
pub struct VenueKillGate { kill: KillHandle, sim: VenueSimulator, flattened: bool }
pub enum KillOutcome { Live, Flattened(Option<SimFill>), Halted }
pub struct KillHalt { pub reason: String }
```

- **`submit(intent, price, t) -> Result<SimFill, KillHalt>`** — the normal order path: **halts** (returns
  `Err(KillHalt)`, nothing sent) once the kill is tripped; otherwise submits via the simulator.
- **`enforce_kill(current_qty, fill_price, t) -> KillOutcome`** — the out-of-band halt action, driven purely
  by the `KillHandle` (no planner/cockpit target needed):
  - not tripped → `Live`.
  - tripped, position non-flat → **flatten**: the closing order is computed by `flatten_intent(current_qty)`
    (opposite side, full magnitude of the signed qty), submitted **directly to the simulator** (the kill's
    *own* action, so it bypasses the submission halt); latch `flattened` → `Flattened(Some(fill))`. The
    caller applies the returned fill to the keeper.
    - **Rationale (diverges from an earlier `plan_delta(Notional::ZERO, …)` sketch, deliberately):** the
      safety path must not depend on a mark to size the flatten. `plan_delta` sizes a *notional* target into
      contracts and so needs a price; flattening to zero is purely `-current_qty` in contracts. `flatten_intent`
      derives the closing order from the kept position **alone**, so the kill flattens even if no fresh mark
      is available. `fill_price` is only the sim's execution price for the resulting order, never used to size it.
  - tripped, position flat / **not yet known** → `Flattened(None)` and the gate stays **armed** (does *not*
    latch): a position learned on a later tick — e.g. after a QE-217 keeper reconnect that re-absorbs the
    authoritative snapshot — is still flattened. It latches to `Halted` only *after* a real position has been
    flattened, which also guards against a double-flatten from keeper fill latency.
- **`impl OrderGate`** — `kill_handle()` returns the held handle; `admit_within_limits` is `Admit` (sizing
  caps are QE-215; QE-216 is the kill). The QE-009 **default `admit`** therefore structurally returns
  `FlattenAndHalt` whenever the switch is tripped — the gate cannot submit while killed.

### D2 — why this satisfies "out-of-band" and "even with cockpit/planner down"

The `KillHandle` is a cloneable, latching, `Send + Sync` handle to shared atomic state. A watchdog, the
clock-skew guard (QE-008), or a manual control holds a **clone** and trips it; the gate observes the same
trip. Neither `submit`'s halt nor `enforce_kill`'s flatten needs the Hedge Planner to produce a target or the
cockpit to be alive — the flatten target is a hard-coded **flat** (`Notional::ZERO`), computed from the kept
position alone. That is exactly the out-of-band, deterministic halt the reviewer required.

## Test plan (deterministic)

1. `gate_honours_kill_switch_conformance` (**AC — contract**) — the QE-009 `assert_honours_kill_switch` on a
   fresh `VenueKillGate`: untripped is live, tripped makes `admit` return `FlattenAndHalt` and `ensure_live`
   a `Halt` disposition.
2. `kill_flattens_position_and_halts_submission` (**AC — behaviour**) — a keeper long 0.2; trip the kill
   **directly** (no planner); `enforce_kill` submits a `Sell 0.2` that flattens the keeper to 0; a second
   `enforce_kill` → `Halted`; `submit` now returns `Err(KillHalt)`; `admit` returns `FlattenAndHalt`.
3. `out_of_band_trip_via_cloned_handle_flattens` — trip a **clone** of the handle (as a watchdog would, with
   no planner/cockpit call); the gate flattens-and-halts — proving independence from the planner.
4. `flat_first_call_stays_armed_and_still_flattens_a_later_position` (**F1 regression**) — flat/not-yet-known
   keeper → `enforce_kill` = `Flattened(None)` (no order, **not** latched); a later non-flat `current_qty`
   is still flattened, and only then does the gate latch to `Halted` (no double-flatten on fill latency).
5. `submit_succeeds_until_killed_then_halts` — pre-trip `submit` fills; post-trip `submit` is `Err`.
6. `kill_latches` — after a trip the gate never returns to `Live`/admits again (QE-009 latch, re-exercised at
   the gate).

## Risks

- **Flatten fills at the mark in sim.** The real venue fills the flatten order at market; the sim uses the
  supplied mark — sufficient to prove flatten-and-halt. A real reduce-only/market flatten is a QE-218 wiring
  detail; documented.
- **`enforce_kill` is caller-driven (once per tick).** The gate flattens on the first post-trip call that
  sees a non-flat position and latches; a caller that never calls `enforce_kill` would still be **halted** on
  `submit`/`admit` (the halt is structural), but would not auto-flatten — the live loop calls `enforce_kill`
  each tick. Documented.
- **Trip during a keeper-reconnect window (F1).** The gate latches only *after* a non-flat position has
  actually been flattened, never on a transient flat/unknown `current_qty`. So a kill that fires while the
  QE-217 keeper has reset and not yet re-absorbed the snapshot cannot leave an open position unflattened: the
  gate stays armed until the real position is known, then flattens it. Latching on the real flatten still
  prevents a double-flatten from keeper fill latency. Covered by
  `flat_first_call_stays_armed_and_still_flattens_a_later_position`.
