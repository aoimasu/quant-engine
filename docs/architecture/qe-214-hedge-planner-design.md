# QE-214 — Hedge Planner (target-position) — design note

`Phase: P2` · `Area: ⑤ Hedge Planning` · `Depends on: QE-213` · `Branch: qe-214/hedge-planner`

## Goal (from backlog)

Emits **absolute** target positions; **stateless** with respect to current position; tracks equity and buying
power.

- **Scope.** Emit absolute target positions from netted targets; maintain an independent equity +
  available-margin view (capital allocation) sourced from the **position keeper**; surface to cockpit.
  Stateless wrt current position (the architectural benefit of target-based hedging).
- **Out of scope.** Venue delta translation (QE-217) — turning `target − current` into an order delta.

**Acceptance criteria.**
- [ ] The planner emits identical targets regardless of current venue position (**statelessness test**).
- [ ] Equity/margin view matches keeper truth.

`Spec ref: ⑤ hedger; Runtime — "stateless with respect to current position".`

## Current-state evidence & placement

- QE-213 produces `qe_runtime::live_netter::NetTarget { net, long, short }` — the aggregate target as a
  **fraction of allowed capital** (signed; `net` positive = net long). The backlog names QE-214 as the stage
  that "turns this aggregate target into **absolute positions** and tracks equity/buying power" — so QE-214
  scales the fraction by capital.
- Money: `qe_domain::Notional` — a **signed** money amount (`new`/`get`/`ZERO`/`checked_*`), the right type
  for a signed absolute position (sign = direction, `0` = flat) and for equity/margin.
- The **position keeper** (equity, available margin, current venue position) is QE-217's component; it does
  not exist yet. QE-214 depends only on an *interface* to it — a `PositionKeeper` **seam** (the same
  deterministic-seam discipline as the transport/clock seams) — with a fake keeper in tests.
- **Placement: new `crates/runtime/src/hedger.rs`** (Area ⑤), exported from `lib.rs`. `qe-runtime` already
  depends on `qe-domain`; it consumes the in-crate `NetTarget`. No new dependency, no cross-crate edge →
  QE-132 firewall unaffected.

## Design

### D1 — the `PositionKeeper` seam and the capital view

```rust
pub struct CapitalView { pub equity: Notional, pub available_margin: Notional }

pub trait PositionKeeper {
    fn capital(&self) -> CapitalView;     // equity + available margin — keeper truth
    fn venue_position(&self) -> Notional; // current signed venue position — keeper truth
}
```

The keeper is the single source of truth for capital **and** the current venue position. The planner reads
`capital()` for the equity/margin view; `venue_position()` exists for QE-217's delta translation and is
**deliberately not read by the planner** — that omission *is* the statelessness property.

### D2 — `HedgePlanner` — absolute, stateless target

```rust
pub struct TargetPosition { pub notional: Notional }   // signed absolute target; sign = direction
impl TargetPosition { pub fn direction(&self) -> Option<Direction>; }  // +→Long, −→Short, 0→None

pub struct HedgePlanner<K> { keeper: K }
impl<K: PositionKeeper> HedgePlanner<K> {
    pub fn new(keeper: K) -> Self;
    pub fn capital_view(&self) -> CapitalView;             // == keeper.capital()  (AC #2)
    pub fn plan(&self, net: NetTarget) -> TargetPosition;  // net.net × equity     (AC #1)
}
```

- **`plan`** emits the **absolute** target: `notional = net.net × equity` (equity = allowed capital, read
  **fresh** from the keeper each call, so the target tracks equity). It computes an absolute target position,
  **not** a delta from the current position — so it never calls `venue_position()`. This is the architectural
  benefit the spec calls out: a target-based hedger is stateless; the *delta* `target − current` is computed
  downstream by QE-217, which is the only place that needs the current position.
- **`capital_view`** forwards the keeper's `CapitalView`, so the planner's independent equity/margin view
  matches keeper truth by construction and tracks the keeper as capital moves.
- **Statelessness is structural, and tested behaviourally:** because `plan` takes only `NetTarget` and reads
  only `capital().equity`, two keepers with identical equity but different `venue_position` (or one keeper
  whose venue position mutates between calls) yield **identical** targets.

**Buying power.** `available_margin` is surfaced in the view (for the cockpit and QE-215 pre-trade caps) but
does not clamp the target here — sizing caps are QE-215. QE-214's target is `fraction × equity`; enforcement
is the next stage. (The "surface to cockpit" is the `capital_view()` accessor; no cockpit exists yet.)

## Test plan (deterministic)

1. `plan_is_stateless_wrt_current_venue_position` (**AC #1**) — fix equity, plan a `NetTarget`; change the
   keeper's `venue_position` (flat → large long → large short) and re-plan: the `TargetPosition` is identical
   every time, even though `venue_position()` reports the changed position.
2. `capital_view_matches_keeper_truth` (**AC #2**) — `planner.capital_view()` equals the keeper's `capital()`
   for equity and available margin, and tracks a subsequent keeper change.
3. `plan_scales_net_fraction_by_equity` — `notional == net.net × equity` on a worked example (`net 0.009`,
   equity `10_000` → `90`); doubling equity doubles the target (fresh read).
4. `plan_sign_encodes_direction` — a net-short `NetTarget` yields a negative `notional` (`direction() ==
   Short`); a zero net yields `Notional::ZERO` (`direction() == None`).

## Risks

- **Keeper does not exist yet (seam).** QE-214 ships against the `PositionKeeper` trait with a fake; the real
  keeper is QE-217. This is the established seam discipline — the interface is the contract, and the
  statelessness/`capital_view` ACs are provable against the fake.
- **Allowed capital = equity.** The target scales by equity; leverage/buying-power *caps* are QE-215, not a
  QE-214 scaling factor. Documented, so a later change to the allowed-capital definition is localised.
