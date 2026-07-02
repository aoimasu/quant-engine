# QE-217 — Venue adapter / Position keeper / order lifecycle + simulator — design note

`Phase: P2` · `Area: ⑥ Edge gateway` · `Depends on: QE-203, QE-204, QE-007` · `Branch: qe-217/venue-adapter`

## Goal (from backlog)

Translate absolute targets into venue-native deltas against kept position; the keeper is fed by the
authoritative user-data stream; a simulator enables paper/sim mode.

- **Scope.** Translate targets → venue-native order **deltas**; track **order lifecycle**; a **position
  keeper** absorbs fills/position reports as ground truth (**never infers** position). **Simulator** mode
  (in-memory ledger for sim; live cash is venue-side).
- **Out of scope.** gRPC wiring (QE-218).

**Acceptance criteria.**
- [ ] Targets become **correct venue deltas vs kept position**; keeper state **tracks venue reports**; **sim
  mode runs the full loop with no real orders**.

`Spec ref: ⑥ router "Venue adapter · Position keeper"; Runtime — position reports authoritative.`

## Current-state evidence & placement

- QE-214 emits `TargetPosition { notional }` (absolute, signed) and defines the `PositionKeeper` **trait**
  (`capital`/`venue_position`/`equity`) the planner reads — QE-217 provides the **real** keeper.
- QE-204 (`crates/venue/src/userdata.rs`) is the authoritative private feed: `UserDataEvent::{Fill,
  Position, Snapshot, Heartbeat, ListenKeyExpired}` with `Fill { side, price, qty, order_id, … }` and
  `PositionReport { direction, qty, entry_price, … }`. The keeper consumes these.
- Domain money/side types: `Notional` (signed), `Price`, `Qty`, `Side`/`Direction` (total conversions),
  `InstrumentId`.
- **Placement: new `crates/runtime/src/edge.rs`.** It *must* live in `qe-runtime`: it **impls** the
  `qe_runtime::hedger::PositionKeeper` trait **and** consumes `qe_venue::userdata` types, and `qe-venue`
  cannot depend on `qe-runtime` (the edge already runs the other way). `qe-runtime` already depends on
  `qe-venue` + `qe-domain` → no new dependency, QE-132 firewall unaffected.

## Design

### D1 — order lifecycle

```rust
pub enum OrderState { New, Submitted, PartiallyFilled, Filled, Rejected, Cancelled }
pub struct Order { pub id: u64, pub side: Side, pub qty: Qty, pub filled: Qty, pub state: OrderState }
```

`submit()` (`New→Submitted`), `on_fill(q)` (accumulates `filled`; `Filled` when `filled ≥ qty`, else
`PartiallyFilled`), `reject()`, `cancel()`.

### D2 — delta translation (target → venue-native order)

```rust
pub struct OrderIntent { pub side: Side, pub qty: Qty }
pub fn plan_delta(target: Notional, current_qty: Decimal, mark: Price) -> Option<OrderIntent>;
```

`target_qty = target / mark` (signed); `delta = target_qty − current_qty`. `None` if `delta == 0` (already at
target) or `mark == 0` (cannot translate pre-mark). Otherwise `side = Buy` if `delta > 0` else `Sell`, `qty =
|delta|`. This is the **stateless→stateful bridge**: QE-214 emits the absolute target; QE-217 is the *only*
place the current kept position enters, exactly as the statelessness split intended. (Lot-size/precision
rounding is a venue-specific refinement, deferred.)

### D3 — position keeper (`VenueKeeper`) — venue truth, never inferred

Tracks a **signed quantity** for one instrument, updated **only** from venue events — never from its own
submitted orders:

- `apply(&UserDataEvent)`:
  - `Fill` (venue-confirmed): `signed_qty ±= qty` by side (for `self.instrument` only).
  - `Position` / `Snapshot` (authoritative reconciliation): **set** `signed_qty` from the report's
    `direction`/`qty` — overriding any fill-derived value (the venue report is ground truth).
  - `Heartbeat`/`ListenKeyExpired`: no position change.
- `observe_mark(Price)` and `observe_balance(equity, available_margin)` — mark and account balances are venue
  truth fed in (QE-204 carries no balance; the simulator/live account stream supplies it).
- `impl qe_runtime::hedger::PositionKeeper`: `capital()` = the observed `{equity, available_margin}`;
  `venue_position()` = `signed_qty × mark`. It does **not** override the default `equity()`, so it satisfies
  the QE-214 forward obligation (`equity()` stays consistent with `capital().equity`) by construction.

### D4 — simulator (`VenueSimulator`) — in-memory, no real orders

`submit(intent, fill_price, event_time_ms) -> SimFill { order: Order, event: UserDataEvent::Fill }`: assigns
an order id, drives the order `New→Submitted→Filled` with an **immediate full fill** at `fill_price`, updates
its own sim position, and returns the `Fill` **event** the keeper then absorbs. `position_report(t)` yields
the sim's authoritative `PositionReport` (for reconciliation). The in-memory ledger is reserved for sim; live
cash is venue-side. (Immediate-full-fill is the sim fill model; partial/latency fills are a later refinement —
the `OrderState`/`on_fill` machinery already supports partials.)

## Test plan (deterministic)

1. `target_becomes_correct_delta_vs_kept_position` (**AC #1**) — flat keeper, mark 50 000, target +10 000 →
   `Buy 0.2`; after applying the sim fill, `signed_qty == 0.2`, `venue_position ≈ 10 000`; then target 5 000 →
   `Sell 0.1`; at-target → `None` (no order); a short target → `Sell`.
2. `keeper_tracks_venue_reports_authoritatively` (**AC #2**) — a `Fill` moves the position; a subsequent
   `PositionReport` **overrides** it to the venue's number (proving "never infers": the report wins over the
   fill-derived value); a `Snapshot` (re-)sets it; other-instrument events are ignored.
3. `sim_runs_full_loop_with_no_real_orders` (**AC #3**) — drive target → `plan_delta` → `sim.submit` →
   `keeper.apply(fill)` for two successive targets; the kept position converges to each target using only the
   in-memory simulator (no real venue call). Order ends `Filled`.
4. `hedge_planner_over_venue_keeper_end_to_end` — a `HedgePlanner` built over the `VenueKeeper` plans an
   absolute target from a `NetTarget`, which `plan_delta` turns into a delta the sim fills — the QE-213→217
   stack, proving the keeper satisfies the QE-214 seam.
5. `order_lifecycle_transitions` — `New→Submitted`, partial then full `on_fill` (`PartiallyFilled→Filled`),
   `reject`/`cancel`.

## Risks

- **Balance feed.** QE-204 carries no account-balance event, so equity/margin are fed via `observe_balance`
  (sim ledger now; a live account stream later). Documented; the keeper never *infers* balances.
- **Sim fill model is immediate-full-fill.** Sufficient for the loop AC; partial/slippage/latency fills are a
  refinement the `OrderState` machinery already accommodates. Documented.
- **No lot-size/precision rounding** in `plan_delta` yet — exact `Decimal` deltas; venue precision is a later
  refinement (QE-218 wiring / venue metadata). Documented.
