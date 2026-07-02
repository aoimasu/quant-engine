# QE-218 — gRPC transport (Hedge Planner ↔ Edge gateway) — design note

`Phase: P2` · `Area: ⑤↔⑥` · `Depends on: QE-214, QE-217` · `Branch: qe-218/grpc-transport`

## Goal (from backlog)

Decisions flow planner→adapter over gRPC; fills/positions/heartbeat flow back.

- **Scope.** gRPC service: planner emits **target revisions**; adapter returns **fills + position reports +
  heartbeat/venue-health**. **Backpressure** and **reconnection** handled.
- **Out of scope.** Journal append (QE-301) — but the append path **must never gate** this dispatch.

**Acceptance criteria.**
- [ ] A target revision reaches the adapter and fills/positions return; the append path (QE-301) **never
  gates** this dispatch.

`Spec ref: Runtime — "flow into the Hedge Planner → Venue adapter chain over gRPC".`

## Current-state evidence & placement

- QE-214 (`crates/runtime/src/hedger.rs`): `HedgePlanner::plan(net) -> TargetPosition` — an **absolute,
  signed** target notional. This absoluteness is the load-bearing property for QE-218's backpressure and
  reconnection (below).
- QE-217 (`crates/runtime/src/edge.rs`): the venue adapter — `plan_delta(target, current_qty, mark)` (the
  only place the kept position enters), `VenueKeeper` (authoritative position, `apply(&UserDataEvent)`,
  `mark()`, `signed_qty()`), `VenueSimulator` (`submit → SimFill`, `position_report(t)`).
- QE-216 (`crates/runtime/src/kill_gate.rs`): `VenueKillGate` — the kill-gated submission port
  (`submit → Result<SimFill, KillHalt>`). QE-218 dispatches **through** it, so a tripped kill halts
  submission on the transport path too.
- QE-204 (`crates/venue/src/userdata.rs`): `UserDataEvent::{Fill, Position(PositionReport), …}` — the
  return-channel payloads.
- **Transport convention (QE-201/202/204).** Every venue seam is a **deterministic, pull-based** trait with
  an offline fake; the real network client is deferred behind the default-off `http` feature so the core
  build / `cargo test` / `cargo deny` stay offline and dependency-light. QE-218 follows this exactly: the
  **real tonic/gRPC wire is deferred to the runtime binary wiring**; QE-218's deliverable is the transport
  **semantics** (message model, backpressure, reconnection, append-decoupling) proven against an in-process,
  single-threaded model — no `tokio`/`tonic`/`prost` in the tested core, no new workspace dependency.
- **Placement: new `crates/runtime/src/transport.rs`.** The planner (`hedger`) and the adapter (`edge` +
  `kill_gate`) both live in `qe-runtime`; their boundary belongs there too. No new crate dependency → the
  QE-132 firewall is unaffected.

## Design

### D1 — message model

```rust
pub struct TargetRevision { pub seq: u64, pub target: TargetPosition, pub event_time_ms: i64 }

pub enum VenueHealth { Ok, Degraded(String), Down(String) }

pub enum AdapterReport {
    Fill(SimFill),
    Position(PositionReport),
    Heartbeat { ack_seq: Option<u64>, health: VenueHealth, event_time_ms: i64 },
}

pub enum TransportError { Disconnected }
```

- `TargetRevision.seq` is the planner's **monotonic revision number** (a later `seq` supersedes an earlier
  one). The mark is **not** carried — it is venue truth held by the `VenueKeeper`; the revision carries only
  the absolute target + the event time to stamp fills/reports.
- `AdapterReport` is the return stream: fills (the venue-confirmed `SimFill`), authoritative position reports,
  and a heartbeat carrying `venue-health` + the `ack_seq` of the last applied revision.

### D2 — the append seam (QE-301) — decoupled by construction

```rust
pub trait AppendSink { fn append(&mut self, rev: &TargetRevision, reports: &[AdapterReport]) -> Result<(), AppendError>; }
pub struct NullAppendSink;   // default: journalling not wired yet (QE-301)
```

The dispatcher **computes the reports from the edge and returns them**, then *offers* the same `&[reports]`
to the `AppendSink`. The sink's `Result` is captured into an `append_failures` counter and **cannot alter the
returned reports** — so the dispatch→fill roundtrip is structurally independent of the journal. That is the
AC's "append never gates dispatch": a failing/blocked sink changes nothing about what the planner receives.
(In the live binary the sink is a best-effort async journal; here it is a sync seam whose failure is proven
non-gating.)

### D3 — `PlannerAdapterLink<A: AppendSink>` — the in-process bidi stream

One struct models both ends of the gRPC bidi stream (planner client ↔ adapter server), single-threaded and
pull-based for deterministic tests. It owns the adapter state: a `VenueKeeper` + a `VenueKillGate` (the
kill-gated `VenueSimulator`), plus the transport state:

```rust
pending: Option<TargetRevision>,   // the coalescing send queue (backpressure)
last_applied: Option<TargetRevision>,  // retained for reconnection resume
connected: bool,
append: A, append_failures: u64, dropped_superseded: u64,
```

- **`submit_target(rev) -> Result<(), TransportError>`** (planner→transport). `Err(Disconnected)` while
  disconnected. Otherwise the revision enters `pending`; if a revision was already pending, the **older one
  is dropped** (`dropped_superseded += 1`) — see D4.
- **`pump() -> Vec<AdapterReport>`** (adapter server tick). Takes the single `pending` revision (if any),
  applies it to the edge, records it as `last_applied`, offers the reports to the append sink, and returns
  the reports. Applying a revision:
  1. `plan_delta(rev.target.notional, keeper.signed_qty(), keeper.mark())` → an optional delta order.
  2. If `Some(intent)`: `gate.submit(intent, keeper.mark(), rev.event_time_ms)` — `Ok(fill)` →
     `keeper.apply(&fill.event)` + push `AdapterReport::Fill`; `Err(KillHalt)` → **no fill** (submission
     halted). A zero delta submits nothing.
  3. Always push an authoritative `Position` report (`simulator().position_report(t)`) and a `Heartbeat`
     (`ack_seq = Some(rev.seq)`, `health`).
  - **Health is derived from the kill state directly** — `Down(kill_reason())` iff `gate.kill().is_tripped()`,
    else `Ok` — **not** from whether a delta happened to be submitted this tick. A tripped kill therefore
    reports `Down` even for an at-target (zero-delta) revision, so the out-of-band halt stays visible to the
    planner in steady state (it re-sends the same absolute target it already reached). `kill_reason()` reuses
    the QE-009 `OrderGate` fallback so the heartbeat reason matches `KillHalt.reason`.
- **`disconnect()` / `reconnect() -> Vec<AdapterReport>`** — see D5.
- Accessors: `keeper()`, `kill()`, `latest_target()`, `orders_submitted()`, `append_failures()`,
  `dropped_superseded()`, `is_connected()`. **Mutation is narrow:** `observe_mark(Price)` /
  `observe_balance(equity, avail)` forward mark/balance (venue truth) to the keeper — there is **no**
  `keeper_mut()`, so no caller can move the kept **position** without a venue fill/report and desync the
  keeper from the simulator (the invariant D5 relies on).

### D4 — backpressure = coalesce-to-latest (correct *because* targets are absolute)

The send queue holds **at most one** revision: a newer revision supersedes an unsent older one. Because a
`TargetPosition` is **absolute** (QE-214), a superseded revision carries no information the latest one lacks —
dropping it is **lossless**, not a compromise. This is the natural, bounded backpressure for an idempotent
absolute-target stream: the adapter always converges to the newest target, and `dropped_superseded` makes the
coalescing observable. (A delta stream could not do this; absoluteness is what buys it.)

### D5 — reconnection = re-snapshot + re-send latest (idempotent, no double-fill)

- While `connected == false`, `submit_target` returns `Err(Disconnected)` and `pump` is inert.
- `reconnect()` sets `connected`, takes an **authoritative `Position` snapshot** (venue truth from the
  simulator), **reconciles it into the keeper** (`keeper.apply(Position(report))` — QE-217 D3: a position
  report authoritatively sets the kept position), and returns it. Reconciling is what makes the snapshot the
  planner receives and the position `plan_delta` re-plans against **one single truth**, not two
  coincidentally-equal values — so if the sim and keeper ever diverged, the reconnect corrects the keeper to
  venue truth before resume (exactly the QE-204/217 user-data reconnect semantics).
- The planner then **re-sends its latest absolute target** (`latest_target()`), which the caller feeds back
  through `submit_target`. On the next `pump`, `plan_delta` compares that target against the **already-updated
  kept position** → the delta is `0` → **no duplicate order**. Absoluteness + keeper-as-truth make resume
  exactly idempotent: a reconnect cannot double the position. This is the reconnection AC.

## Test plan (deterministic, TDD)

1. `target_revision_reaches_adapter_and_fills_return` (**AC**) — flat keeper, mark 50 000; submit target
   +10 000, `pump` → reports contain `Fill(Buy 0.2)` + `Position(long 0.2)` + `Heartbeat{ack_seq:0, Ok}`;
   keeper `signed_qty == 0.2`.
2. `append_never_gates_dispatch` (**AC**) — same dispatch with a `FailingAppendSink` returns the **identical**
   `Fill`+`Position` reports as with `NullAppendSink`, and `append_failures == 1`. Proves the journal path is
   non-gating.
3. `backpressure_coalesces_to_latest_absolute_target` — submit three revisions (targets 10 000 → 20 000 →
   5 000) with **no pump between**; `dropped_superseded == 2`; a single `pump` converges the keeper to
   5 000 → `Sell/Buy` to exactly the latest target, `orders_submitted == 1`.
4. `reconnect_resends_latest_target_without_double_filling` — establish a position; `disconnect()` (→
   `submit_target` errors); `reconnect()` returns a `Position` snapshot; re-send `latest_target()`; `pump`
   → **no new order** (`orders_submitted` unchanged), position unchanged. Idempotent resume.
5. `kill_tripped_dispatch_halts_submission_on_the_wire` — trip `link.kill()`; submit a target; `pump`
   → **no `Fill`**, `Heartbeat` health `Down`, `orders_submitted` unchanged (QE-216 honoured through the
   transport).
6. `at_target_revision_reports_down_while_killed` (**F1 regression**) — reach a target, trip the kill, re-send
   the **same** at-target revision (zero delta → no submit): the heartbeat is still `Down`, not `Ok`, so the
   out-of-band halt stays visible in steady state.
7. `idle_pump_is_silent_and_at_target_revision_acks_without_a_fill` — a `pump` with nothing pending returns
   empty; an at-target revision (kill live) acks with `Ok` health and no fill.

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Determinism.** The link is single-threaded and pull-based (`submit_target` enqueues, `pump` applies); no
  threads, sockets, sleeps, or clocks in the tested core. The real tonic/gRPC bidi stream is a thin adapter in
  the runtime binary (deferred, like QE-202's real websocket), not in the tested surface.
- **Backpressure drops superseded targets — deliberate and lossless.** Justified *only* by the absoluteness of
  `TargetPosition`; the design note flags that a delta stream could not coalesce this way. `dropped_superseded`
  is observable, never silent.
- **Reconnection idempotence rests on keeper-as-truth.** Re-sending the latest absolute target is safe only
  because `plan_delta` reads the authoritative kept position (never inferred). `reconnect` **reconciles** the
  keeper to the venue snapshot it returns (single truth, not coincidental agreement), and mutation of the
  keeper is narrowed to `observe_mark`/`observe_balance` (no `keeper_mut`), so nothing can move the kept
  position off the venue truth. Covered by test 4.
- **Venue health tracks submission state, not delta activity.** Heartbeat health is derived from the kill
  directly, so a tripped kill reads `Down` even when there is no delta to submit — the halt cannot hide behind
  a steady-state at-target loop. Covered by test 6.
- **Append decoupling is structural, not timing-based.** We prove non-gating by construction (the return value
  is produced before and independent of `append`), not by a race — so it holds under any real async journal.
- **Firewall.** No new crate edge; `qe-runtime` already depends on `qe-venue`/`qe-risk`/`qe-domain`. QE-132
  guard stays green.
