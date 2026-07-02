# QE-213 — Position netting — design note

`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-212` · `Branch: qe-213/position-netting`

## Goal (from backlog)

Per-bar decisions net into a single aggregate target position.

- **Scope.** Net per-strategy (post-breaker) decisions into **one aggregate target per instrument**.
- **Out of scope.** Hedge Planner (QE-214) — it turns this aggregate target into absolute positions and tracks
  equity/buying power.

**Acceptance criteria.**
- [ ] Netting equals the sum of post-breaker per-strategy targets; **gated strategies contribute zero**.

`Spec ref: ④ live_netter "Position netting (per-bar evaluation)".`

## Current-state evidence & placement

- The evaluator (QE-207) produces one `PositionState { dir: Option<Direction>, bars_held }` per chromosome
  (`evaluator.rs`), aligned to `vintage.content.chromosomes` / `.weights` (`weights() -> &[f64]`). Each
  genome carries `RiskParams { size_bps: u16 }` — "target notional as **basis points of allowed capital**"
  (`genome.rs`), read on entry (never part of `Decision`).
- QE-212's `BreakerLayer::clamp` rewrites a gated strategy's decision to `Decision::Exit`; applying the shared
  `PositionState::advance` to `Exit` yields **flat** (`dir: None`). So a gated strategy's **post-breaker
  position is flat** — the hook the AC's "gated strategies contribute zero" hangs on.
- **Placement: new `crates/runtime/src/live_netter.rs`** (Area ④ `live_netter`), exported from `lib.rs`.
  `qe-runtime` already depends on `qe-signal` (`PositionState`, `RiskParams`) and `qe-domain` (`Direction`).
  No new dependency, no cross-crate edge → QE-132 firewall unaffected.

## Design

### D1 — a per-strategy leg and its signed target

A strategy's **target** is its post-breaker signed notional as a fraction of allowed capital:

```
magnitude_i = weight_i × size_bps_i / 10_000          (0 if the strategy is flat)
target_i    = +magnitude_i  (Long)  |  −magnitude_i  (Short)  |  0  (flat)
```

`size_bps / 10_000` converts basis-points-of-capital to a fraction; `weight_i` is the ensemble weight. All
arithmetic is `rust_decimal::Decimal` (no float money); the ensemble `weight` (`f64`) is converted **once** at
the boundary with `Decimal::from_f64_retain` (deterministic; a non-finite weight → `0`, documented).

```rust
pub struct NetLeg { pub direction: Option<Direction>, pub weight: Decimal, pub size_bps: u16 }
impl NetLeg {
    pub fn from_position(p: PositionState, weight: f64, size_bps: u16) -> Self;  // dir = p.dir
    pub fn signed_target(&self) -> Decimal;   // the target_i above; 0 when flat
}
```

A **gated** strategy reaches the netter as a **flat** leg (`direction: None`) because QE-212 clamped it to
`Exit` → `advance` → flat, so `signed_target() == 0` — it contributes zero by construction, not by a special
case.

### D2 — the netter

```rust
pub struct NetTarget { pub net: Decimal, pub long: Decimal, pub short: Decimal }  // net = long − short
impl NetTarget { pub fn gross(&self) -> Decimal; }                               // long + short

pub struct PositionNetter;
impl PositionNetter {
    pub fn net(legs: &[NetLeg]) -> NetTarget;                                    // Σ, split by side
    pub fn net_positions(positions: &[PositionState], weights: &[f64], sizes: &[u16]) -> NetTarget;
}
```

- `net` folds the legs: `long` sums long magnitudes, `short` sums short magnitudes (both ≥ 0), `net = long −
  short`. The AC's "sum of post-breaker per-strategy targets" is exactly `Σ signed_target(leg)` = `net`.
- `net_positions` is the ergonomic per-bar entry: it zips the (post-breaker) positions with the vintage's
  `weights` and per-genome `size_bps` (all aligned to the chromosomes) into legs and nets them. A gated
  strategy's position is already flat, so it contributes 0.
- **`long` / `short` split** is deliberate: it is the per-**direction** aggregate the QE-212 forward
  obligation (per-direction breakers) and QE-215 (gross/per-side caps) will consume — produced here where the
  per-strategy directions are known, for free.

**One instrument.** The dev universe is single-instrument (every chromosome trades the same instrument), so
netting yields one `NetTarget`. Multi-instrument is a trivial extension — group legs by instrument and net
each group — deferred until a multi-instrument vintage exists (no instrument id on `Decision`/`PositionState`
today).

## Test plan (deterministic)

1. `net_equals_sum_of_leg_targets` (**AC, half 1**) — for a mixed long/short/flat set of legs,
   `PositionNetter::net(legs).net == legs.iter().map(NetLeg::signed_target).sum()`, and `long`/`short`/`gross`
   are the expected per-side sums.
2. `flat_leg_contributes_zero` (**AC, half 2**) — a flat leg's `signed_target()` is `0`; netting a set with an
   extra flat leg equals netting without it.
3. `gated_strategy_via_breaker_contributes_zero` (**AC, end-to-end with QE-212**) — build raw decisions, gate
   strategy 0 through `BreakerLayer`, `clamp`, advance the prior positions by the clamped decisions to get
   post-breaker positions, then `net_positions`: the aggregate equals the sum over the **ungated** strategies
   only, and strategy 0's leg target is `0`.
4. `longs_and_shorts_offset` — equal-and-opposite legs net to `0` while `gross` is non-zero (netting, not
   gross-summing).
5. `weights_and_sizes_scale_the_target` — a leg's magnitude tracks `weight × size_bps / 10_000` exactly
   (Decimal), and doubling a weight doubles its contribution.

## Risks

- **f64 weight → Decimal conversion.** The only float touch-point; converted once via `from_f64_retain`
  (deterministic, lossless for the ensemble's finite fractions). Money stays Decimal throughout the fold.
- **Aggregate target is a fraction of capital, not absolute notional.** Turning it into an absolute position
  (× equity / buying power) is QE-214's job by design; QE-213 stops at the per-instrument aggregate target.
