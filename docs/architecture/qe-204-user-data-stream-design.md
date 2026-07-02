# QE-204 — User-data stream subscription — design note

`Phase: P2` · `Area: ②/⑥ private` · `Depends on: QE-201` · `Branch: qe-204/user-data-stream`

## Goal (from backlog)

Fills, position reports, and heartbeat are the authoritative ground-truth feed for the Position keeper.

- Subscribe the **subaccount-scoped** private user-data stream (fills + positions + heartbeat); reconnect
  with **listen-key renewal**; **simulator equivalent** for sim mode.

**Acceptance criteria.**
- [ ] Fills/positions/heartbeat are delivered **in order**; a dropped stream reconnects **without losing
  position truth** (re-snapshot on reconnect).

**Out of scope.** Position keeper logic (QE-217) — this ticket delivers the *feed*, not the position
state machine. The concrete signed-REST listen-key client and the concrete async wss adapter are deferred
to runtime wiring (mirrors QE-202's deferral of the real socket adapter); QE-204 lands the deterministic
seams + event model + orchestration + a usable sim implementation.

## Current-state evidence & placement

- The venue crate already establishes the pattern QE-204 follows: **logic written against seams, real
  network behind the `http` feature, deterministic fakes in tests.** QE-201 (`rest.rs`) uses the
  `RestTransport` seam; QE-202 (`ws.rs`/`registry.rs`) uses the pull-based `WsConnection`/`WsConnector`
  seams and a `ConnectionRegistry` orchestrator that reconnects + resubscribes.
- The user-data stream is **not** tier-partitioned market data — it is a single **private** connection
  keyed by a venue **listen key** with its own lifecycle (create → keep-alive → expire/renew). So it gets
  its own module rather than extending the market-tier `ConnectionRegistry`.
- Domain types are ready: `Side`/`Direction` (`side.rs`), `Price`/`Qty` (`money.rs`), `InstrumentId`.
- **Placement: new `crates/venue/src/userdata.rs`**, exported from `lib.rs`. No new dependency, no
  cross-crate edge → QE-132 firewall guard unaffected.

## Design

### D1 — Event model (domain-typed, faithful to the venue feed)

- `Fill` — an order/trade update: `instrument`, `side`, `price`, `qty`, `order_id`, `trade_id`,
  `event_time_ms`.
- `PositionReport` — one instrument's position from an account update: `instrument`,
  `direction: Option<Direction>` (`None` = flat, since `Qty` is non-negative), `qty`, `entry_price`,
  `event_time_ms`.
- `PositionSnapshot` — a full set of `PositionReport`s taken via REST at (re)connect. This is the
  **position-truth** carrier.
- `UserDataEvent` = `Fill | Position(PositionReport) | Heartbeat{event_time_ms} |
  ListenKeyExpired{event_time_ms} | Snapshot(PositionSnapshot)`.

### D2 — Seams (the network boundary)

- `ListenKeyProvider` — `create() -> ListenKey`, `keepalive(&ListenKey)`. (Real impl = signed POST/PUT
  `/fapi/v1/listenKey`, deferred to runtime wiring.)
- `UserDataConnector` — `connect(&ListenKey) -> Box<dyn UserDataConnection>`.
- `UserDataConnection` — pull-based `poll() -> UserDataPoll` (`Event | Disconnected | Idle`), exactly the
  QE-202 style so the tested core is single-threaded and deterministic.
- `PositionSnapshotSource` — `snapshot() -> PositionSnapshot` (real impl = REST `positionRisk`/account).

### D3 — `UserDataSession` orchestrator

Owns the current `ListenKey` + live `UserDataConnection`. Methods:

- `connect()` — `create()` a listen key, `connect()` the socket, take an initial `snapshot()`; returns the
  snapshot so the caller (Position keeper, later) establishes truth from bar zero.
- `pump() -> UserDataOutcome { event, reconnected }` — poll once:
  - `Event(e)` → deliver `e` **in order** (no reordering; one event per pump).
  - `Disconnected` **or** an in-band `ListenKeyExpired` event → **renew + reconnect + re-snapshot**:
    `create()` a *fresh* listen key (renewal), `connect()` a new socket, take a fresh `snapshot()`, and
    surface it as `UserDataEvent::Snapshot(..)` with `reconnected = true`. This is the AC's
    "reconnects without losing position truth" — the re-snapshot re-establishes the full position set the
    keeper must trust after any gap.
- `keepalive()` — `provider.keepalive(current_key)` to hold the key alive between renewals.

`ListenKeyExpired` is treated identically to a disconnect because an expired key means the socket's data
can no longer be trusted — the safe response is the same renew + re-snapshot path.

### D4 — Simulator for sim mode

A concrete `sim::ScriptedUserData` (NOT test-only) implements all three seams from an in-memory script:
a queue of connection scripts (each a `Vec<UserDataPoll>`), a sequence of snapshots to hand out per
connect, and monotonic listen keys (`sim-key-1`, `sim-key-2`, …). Runtime sim mode drives a
`UserDataSession` over `ScriptedUserData` to exercise the full fills/positions/heartbeat + reconnect loop
with **no real venue** — the sim-mode equivalent the ticket requires.

## Test plan (deterministic, no network)

1. `events_are_delivered_in_order` — a connection scripted with Fill → Position → Heartbeat yields exactly
   that order from successive `pump()`s (AC part 1).
2. `reconnect_renews_key_and_resnapshots_without_losing_position_truth` — connect (snapshot A), Fill,
   Disconnected → pump renews the listen key (new key created), reconnects, and delivers a fresh
   `Snapshot(B)` with `reconnected = true`; the post-reconnect position set is the re-snapshotted truth
   (AC part 2).
3. `listen_key_expiry_triggers_renew_and_resnapshot` — a `ListenKeyExpired` event drives the same renew +
   re-snapshot path as a disconnect.
4. `keepalive_holds_the_current_key` — `keepalive()` calls the provider for the *current* key; count
   verified.
5. `idle_poll_yields_no_event` — an `Idle` poll returns `{ event: None, reconnected: false }`.
6. `sim_scripted_user_data_drives_a_full_session` — the sim implementation runs connect → in-order events →
   reconnect end-to-end, proving the sim-mode equivalent is usable.

## Risks

- **No real network path in this ticket.** Intentional and consistent with QE-202; documented. The signed
  listen-key REST client + async wss adapter are runtime-wiring follow-ups; the seams pin their contracts.
- **Position representation.** `Qty` is non-negative, so direction is carried separately
  (`Option<Direction>`, `None` = flat) rather than as a signed quantity — avoids a signed-money type the
  domain deliberately doesn't have.
