# QE-203 — wss Realtime-tier streams — design note

`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-202` · `Branch: qe-203/wss-realtime-tier-streams`

## Goal (from backlog)

The Edge gateway's execution mechanics consume **Realtime-tier** streams (`bookTicker`, `depth20@100ms`)
plus `aggTrade`.

- Subscribe `bookTicker`, `depth20@100ms`, `aggTrade` via the registry; disjoint from the Hedge-Planner
  data path (shares no upstream path with QE-202 by construction).

**Acceptance criteria.**
- [ ] Edge-side and Planner-side streams share no upstream data path (verified).

**Out of scope.** Order submission (QE-217); the concrete async websocket adapter (deferred to runtime
wiring, per QE-202's note — this ticket stays on the deterministic pull-based seam).

## Current-state evidence & placement

- **QE-202 already built the partitioning machinery.** `crates/venue/src/stream.rs` defines `StreamTier`
  (`Market` / `Realtime`), `StreamChannel` (`Kline(Resolution)` / `MarkPrice`), and `Subscription` with
  `tier()`, `stream_name()`, and `cadence_ms()`. `crates/venue/src/registry.rs`'s `ConnectionRegistry`
  keeps **one connection per tier** in separate `HashMap<StreamTier, TierConn>` slots — the registry test
  `market_and_realtime_tiers_are_partitioned` already establishes a `Realtime` partition and asserts
  subscriptions never bleed across the boundary, noting *"its streams arrive in QE-203."*
- So QE-203 is **not** new plumbing. It is: (1) add the three Realtime channels to `StreamChannel`;
  (2) route them to `StreamTier::Realtime` in `Subscription::tier()`; (3) give them venue-correct
  `stream_name()` suffixes and honest gap cadences; (4) add a **disjointness test** proving the Edge-side
  (Realtime) and Planner-side (Market) subscriptions share no tier / no connection.
- **Placement: `crates/venue/src/stream.rs` + `registry.rs`** — the same files QE-202 landed. No new crate,
  no new dependency, no cross-crate edge, so the QE-132 firewall guard is unaffected.

## Design

### D1 — Three new Realtime channels on `StreamChannel`

Add three unit variants (keeps `StreamChannel: Copy`, since they carry no data):

| Variant | `stream_name()` (btcusdt) | Tier | Cadence |
|---|---|---|---|
| `BookTicker` | `btcusdt@bookTicker` | Realtime | event-driven → **no cadence** |
| `Depth20` | `btcusdt@depth20@100ms` | Realtime | **100 ms** |
| `AggTrade` | `btcusdt@aggTrade` | Realtime | event-driven → **no cadence** |

Convenience constructors mirror QE-202: `Subscription::book_ticker(inst)`, `depth20(inst)`,
`agg_trade(inst)`.

### D2 — `cadence_ms(self) -> Option<i64>` (the one honest signature change)

`bookTicker` and `aggTrade` are **event-driven**: consecutive events can be microseconds or seconds apart,
so a time-based "hole" is undefined for them. Modelling their cadence as a constant would make the
registry's gap detector fire (or never fire) arbitrarily. `depth20@100ms` genuinely has a 100 ms cadence.

The honest model: `cadence_ms` returns `Option<i64>` — `Some(ms)` for fixed-cadence channels
(`Kline`, `MarkPrice`, `Depth20`), `None` for event-driven ones (`BookTicker`, `AggTrade`). The registry
runs cadence-based gap detection **only** when the channel declares a cadence; for event-driven streams an
outage is still surfaced via `PumpOutcome.reconnected` (the registry already sets it), just not as a
time-delta `Gap` — which is the correct semantics.

This is contained to the venue crate: the only external callers are `registry.rs` (gap logic) and the two
`stream.rs` unit-test asserts (updated to `Some(...)`). No other crate references `cadence_ms`.

### D3 — Disjointness proof (the AC)

The AC is architectural: Edge-side and Planner-side streams "share no upstream data path." By construction
the registry holds each tier's socket in a **separate slot** and never routes a subscription across tiers.
The new test subscribes the Market tier (kline + markPrice — Planner/Hedge-Planner path) and the Realtime
tier (bookTicker + depth20 + aggTrade — Edge path) on **one** registry and asserts:
- the two tiers resolve to **distinct** `StreamTier`s via `Subscription::tier()`;
- each tier's recorded `subscriptions()` contains only its own channels (no bleed either direction);
- the registry opened **one connection per tier** (two independent sockets — no shared upstream).

## Test plan

All deterministic, no network (pull-based seam + `FakeConnector`), matching QE-202's style.

1. `realtime_channels_have_venue_correct_stream_names` — `bookTicker` / `depth20@100ms` / `aggTrade` suffix
   + full `stream_name()` forms.
2. `realtime_channels_are_realtime_tier` — `tier()` returns `Realtime` for all three; `Kline`/`MarkPrice`
   stay `Market` (guards against a mis-route regression).
3. `event_driven_channels_have_no_cadence` — `bookTicker`/`aggTrade` → `None`; `depth20` → `Some(100)`;
   `kline`/`markPrice` unchanged.
4. `edge_and_planner_streams_are_disjoint` (the AC) — one registry, Market + Realtime subscribed; distinct
   tiers, no subscription bleed, one connection per tier.
5. `depth20_reconnect_reports_gap` / event-driven reconnect reports **no** cadence gap but flags
   `reconnected` — proves the `Option` cadence path in the registry.

## Risks

- **API change to `cadence_ms`.** Mitigated: within-crate only; both callers updated in the same diff; the
  new signature is strictly more expressive (no silent behavioural change for `Kline`/`MarkPrice`).
- **Depth stream naming.** Binance USDT-M offers `depth5/10/20` at `@100ms`/`@250ms`/`@500ms`; the backlog
  fixes **`depth20@100ms`**, which is what `Depth20`'s `stream_name()` emits. Other depth flavours are out
  of scope for this ticket.
