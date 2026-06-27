# QE-202 — wss Market-tier streams + connection registry — design note

`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-004` · `Branch: qe-202/wss-market-streams`

## Goal (from backlog)

The Hedge-Planner data path consumes Market-tier streams (kline + markPrice@1s) via a tier-partitioned
connection registry.

- Subscribe kline (5m/30m/4h) + markPrice@1s; tier-partitioned websocket connection registry;
  reconnection/resubscribe with gap handling.

**Acceptance criteria.**
- [ ] Disconnect/reconnect resubscribes and reports any gap; Market and Realtime tiers are partitioned in
  the registry.

**Out of scope.** Realtime-tier streams themselves (QE-203 — QE-202 only establishes the *partition* for
them); live bar reconstruction (QE-205).

## Current-state evidence & placement

- QE-201 just landed the REST half of `qe-venue` with the seam discipline this ticket follows: a transport
  trait + offline fake, concrete client behind the default-off `http` feature, deterministic via the
  `Clock` seam. QE-202 adds the **wss** half.
- `qe-domain` gives `InstrumentId`, `Resolution` (`minutes()` → cadence), `Timestamp`. The Market tier
  subscribes kline 5m/30m/4h + markPrice@1s.
- Same firewall position as QE-201: `qe-venue` is runtime-side, depends only on `qe-domain` (+ `thiserror`,
  + optional `tungstenite` behind `http`); no `qe-wfo`/`qe-ensemble` edge.

## Design

### D1 — Stream model (`stream.rs`)

- `StreamTier { Market, Realtime }` — the partition key. The spec routes kline + markPrice to **Market**;
  Realtime (QE-203) is a *separate* partition established here so the registry is tier-partitioned from day
  one.
- `StreamChannel { Kline(Resolution), MarkPrice }`. `cadence_ms()` = `Resolution::minutes()·60_000` for
  kline, `1_000` for markPrice@1s — the expected inter-message spacing, used for gap detection.
- `Subscription { instrument, channel }` + `tier()` (Market for both channels here) + `stream_name()` (the
  venue stream id, e.g. `btcusdt@kline_5m`, `btcusdt@markPrice@1s`) — the resubscribe payload and the
  per-subscription gap-tracking key.
- `StreamMessage { subscription, event_time_ms, payload }` — one decoded update.
- `Gap { subscription, from_ms, to_ms }` — a detected discontinuity (`to − from` missed).

### D2 — Transport seam (`ws.rs`)

The single network seam, pull-based for deterministic tests (no threads, no real socket in core):

- `trait WsConnection { fn subscribe(&mut self, subs:&[Subscription]) -> Result<(),WsError>; fn poll(&mut
  self) -> WsPoll; }` where `WsPoll { Message(StreamMessage), Disconnected, Idle }`.
- `trait WsConnector { fn connect(&self, tier: StreamTier) -> Result<Box<dyn WsConnection>, WsError>; }` —
  the factory the registry calls to (re)establish a tier's socket.
- `WsError { Connect(String), Subscribe(String), Closed }`.
- The **concrete async websocket adapter is deferred to the runtime wiring** (it pulls a TLS websocket
  stack with its own licence surface; kept out of the core so the offline build/`deny` stay
  dependency-light, mirroring how the `http` REST transport is opt-in). QE-202's deliverable is the seam +
  registry + gap logic, proven against an offline `FakeConnector`/`FakeConnection` that scripts a message
  sequence, a `Disconnected`, and the post-reconnect resume.

### D3 — Tier-partitioned registry + reconnect/resubscribe/gap (`registry.rs`)

`ConnectionRegistry<C: WsConnector>` keeps **one entry per `StreamTier`** (`Market` and `Realtime` held in
separate slots — the partition). Each entry owns its connection + its `Vec<Subscription>` + a
`last_event_ms` map keyed by `stream_name`.

- `subscribe(tier, subs)` — connect the tier's socket if absent, record the subscriptions, send them. A
  Market subscription never lands in the Realtime slot and vice-versa (the AC's partitioning).
- `pump(tier) -> PumpOutcome` — poll the tier's connection once:
  - `Message(m)` → update `last_event_ms[m.stream]`, return the message (and any gap detected vs the prior
    `last_event_ms` for that stream — a mid-stream gap, e.g. a skipped bar, is reported too).
  - `Disconnected` → **reconnect** (via the connector) → **resubscribe** the recorded subscriptions → the
    next messages are compared against the pre-disconnect `last_event_ms`; the **gap** across the outage is
    reported when the first post-reconnect `event_time_ms` for a stream exceeds `last_event_ms + cadence`.
- Gap rule: for a subscription with a known `last_ms`, a message at `t` reports `Gap{from:last_ms, to:t}`
  iff `t − last_ms > channel.cadence_ms()` (a contiguous next message — exactly one cadence later — is no
  gap). This catches both an in-stream skip and an outage hole uniformly.

`tiers()` / `subscriptions(tier)` expose the partition for assertions.

## Module / API plan

New deps for `qe-venue`: optional `tungstenite` (system TLS) behind the existing `http` feature, default
off. New modules `stream`, `ws`, `registry`; re-exported from `lib.rs`. No change to QE-201 modules.

## Test plan (TDD)

1. **Tiers are partitioned (AC).** Subscribe a Market kline and (a placeholder) Realtime subscription;
   assert they occupy distinct registry slots with distinct connections, and Market's subscription set does
   not contain the Realtime one (and vice-versa).
2. **Disconnect → reconnect → resubscribe (AC).** `FakeConnection` yields 2 messages then `Disconnected`;
   the connector hands a fresh connection that resumes. Assert the registry reconnected and **re-sent the
   recorded subscriptions** on the new connection (the fake records `subscribe` calls).
3. **Gap reported across the outage (AC).** The resume message's `event_time_ms` skips ahead more than one
   cadence past the last pre-disconnect event → `pump` returns a `Gap{from,to}` covering the hole.
4. **No gap when contiguous.** A resume exactly one cadence later → no `Gap`.
5. **In-stream skip is also a gap.** A mid-stream message that jumps >1 cadence reports a `Gap` without a
   disconnect.
6. **Channel cadence / stream names.** `kline_5m` → 300_000ms, `markPrice@1s` → 1_000ms; `stream_name()`
   matches the venue form.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` (default + `http`),
`cargo test -p qe-venue`, `cargo test --workspace`, `cargo test -p qe-architecture --test firewall`,
`cargo deny check`.

## Risks

- **Determinism.** The transport is pull-based (`poll`) and the registry is single-threaded; tests drive it
  step-by-step with a scripted `FakeConnection` — no real socket, no sleeps, no races. Real async I/O is a
  thin `http`-feature adapter, not in the tested core.
- **Gap semantics.** Defined purely from `event_time_ms` + the channel cadence, so it is independent of
  wall-clock and identical in replay/live. The `> cadence` rule treats the normal next message as
  contiguous; only true holes report.
- **Registry holds Realtime as an empty partition** until QE-203 fills it — that is deliberate (the AC
  requires the partition to *exist*), not dead code.
- **Firewall.** `qe-venue` gains no `qe-wfo`/`qe-ensemble` edge; the QE-132 guard stays green.
```
