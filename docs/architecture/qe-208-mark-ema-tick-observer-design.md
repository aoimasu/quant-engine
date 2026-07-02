# QE-208 — Mark EMA loop + tick observer — design note

`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-202` · `Branch: qe-208/mark-ema-tick-observer`

## Goal (from backlog)

Slow-DD probing rides a smoothed mark (EMA τ½=60s); the tick observer feeds the breaker layer.

- EMA loop (τ½=60s) on markPrice@1s; tick observer on smoothed mark for the slow-DD probe (spec baseline).
  A raw-mark fast-tier tick is a documented alternative per QE-116 (build only if that spike adopts it).

**Acceptance criteria.**
- [ ] EMA half-life is correct; both smoothed and raw mark ticks are available to breakers.

**Out of scope.** Breaker logic (QE-212) — this ticket produces the tick stream the breaker will consume,
not the breaker. The concrete markPrice@1s JSON decode / wss wiring is runtime plumbing (this module
operates on already-decoded marks, exactly as QE-205 operates on already-decoded base `Bar`s).

## Current-state evidence & placement

- **QE-116 already implemented the EMA primitive**: `qe_risk::MarkEma` (`crates/risk/src/breaker.rs`) —
  `with_half_life(half_life_secs, tick_secs)` (`alpha = 1 − 2^(−tick/half_life)`), `update(price) ->
  Decimal`, `value()`. It is `Decimal`-based (no float money) and its half-life property is documented
  (QE-116/D1). QE-208 **reuses** it — it does not re-implement smoothing.
- **QE-116/D1 decision on the raw-mark fast tier (A3):** the smoothed stream is the **baseline**; the
  unsmoothed raw-mark fast tier is a *documented alternative, not adopted*. So QE-208 builds the smoothed
  EMA loop and **exposes the raw mark alongside it** (the AC's "both smoothed and raw ticks available to
  breakers") without constructing a separate fast-tier breaker path — that stays QE-212's call if/when the
  alternative is adopted.
- **Placement: new `crates/runtime/src/live_mark.rs`**, exported from `lib.rs`. `qe-runtime` already
  depends on `qe-risk` (`MarkEma`) and `rust_decimal`; the live pipeline (Area ④) is runtime territory. No
  new dependency, no new cross-crate edge → QE-132 firewall guard unaffected.

## Design

### D1 — `MarkTick` — the observation carried to breakers

```
pub struct MarkTick { pub event_time_ms: i64, pub raw: Decimal, pub smoothed: Decimal }
```

Carries **both** the raw markPrice@1s sample and the EMA-smoothed value for the same tick. The smoothed
value drives the slow/med-DD probe (spec baseline); the raw value is available so the fast tier (or the A3
alternative) can watch un-averaged price without a second pipeline. This is exactly the AC — both are on
every tick.

### D2 — `MarkTickObserver` — the seam to the breaker layer

```
pub trait MarkTickObserver { fn on_tick(&mut self, tick: &MarkTick); }
```

The breaker layer (QE-212) implements this to receive the tick stream. A blanket impl for
`FnMut(&MarkTick)` lets callers pass a closure; that keeps QE-208 decoupled from the (not-yet-built)
breaker.

### D3 — `MarkEmaLoop` — the loop

Wraps a `MarkEma`. Per markPrice@1s sample:
- `observe(event_time_ms, raw) -> MarkTick` — push `raw` into the EMA, read back the smoothed value, and
  return a `MarkTick { event_time_ms, raw, smoothed }`. The **first** sample seeds the EMA, so its smoothed
  == raw (MarkEma's documented seeding).
- `drive(marks, observer)` — feed an ordered sequence of `(event_time_ms, raw)` marks, forwarding each
  produced `MarkTick` to a `MarkTickObserver` (the breaker feed), returning the ticks. Preserves arrival
  order.

Constructed with `MarkEmaLoop::with_half_life(half_life_secs, tick_secs)` — the spec baseline is
`with_half_life(60.0, 1.0)` (τ½=60s on 1s ticks), exposed as `MarkEmaLoop::spec_baseline()`.

## Test plan (deterministic, `Decimal`)

1. `ema_half_life_is_correct` — with τ½=60s/1s ticks, seed the loop at price 0 then feed a step to 100 for
   60 ticks; the smoothed value moves ~halfway (≈50) — the half-life property (AC part 1), asserted within
   a small tolerance.
2. `first_tick_seeds_ema_raw_equals_smoothed` — the first `MarkTick`'s `smoothed == raw` (seeding).
3. `both_raw_and_smoothed_reach_the_observer` — drive a short mark sequence through a collecting observer;
   every received tick carries the correct `raw` (== input) and the EMA `smoothed`, and smoothed lags raw
   on a moving series (AC part 2 — both available to breakers).
4. `drive_preserves_order_and_event_times` — the observer sees ticks in input order with matching
   `event_time_ms`.
5. `closure_observer_blanket_impl_works` — a `FnMut(&MarkTick)` closure is usable as an observer.

## Risks

- **No wss decode in this ticket.** Intentional and consistent with QE-205 (operate on decoded inputs); the
  markPrice@1s JSON decode + wss drive is runtime plumbing. The loop's contract is pinned by tests.
- **Float in `alpha` only.** `MarkEma` computes the smoothing coefficient in `f64` then works in `Decimal`;
  prices/marks never touch float. This is QE-116's existing, reviewed choice — QE-208 inherits it.
