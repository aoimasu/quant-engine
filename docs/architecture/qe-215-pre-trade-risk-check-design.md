# QE-215 — Pre-trade risk check — design note

`Phase: P2` · `Area: risk (netting→hedger boundary)` · `Depends on: QE-009, QE-214, QE-130` · `Branch: qe-215/pre-trade-risk`

## Goal (from backlog)

Leveraged perps need hard pre-trade caps and a liquidation-distance floor; "tail-aware" optimisation does not
bound live worst-case loss.

- **Scope.** Enforce the QE-009 limits **before targets leave the planner**: max notional, max leverage,
  gross/net caps, **liquidation-distance floor**, margin-utilisation ceiling. Clamp or halt **per contract**.
- **Out of scope.** Out-of-band kill (QE-216). Per-vintage drawdown cap → `Halt` is the QE-212 breaker / kill
  path, not a per-order pre-trade check, so it is not enforced here.

**Acceptance criteria.**
- [ ] A target implying an unsafe liquidation distance or breaching a cap is **clamped/halted, not sent**.

`Spec ref: Robustness — circuit breakers; reviewer: pre-trade margin/leverage governor.`

## Current-state evidence & placement

- QE-009 already defines the **contract** (`crates/risk/src/limit.rs`): `RiskLimits` (every cap
  `Option<…>`), `LimitKind`, `LimitOutcome { Clamp, Reject, Halt }`, `Leverage`, `Fraction`, and
  `LimitBreach::with_default_outcome` — the per-kind policy is `MaxNotional`/`MaxLeverage → Clamp`;
  `MaxGross`/`MaxNet`/`LiquidationDistanceFloor`/`MarginUtilisationCeiling → Reject`; `DrawdownCap → Halt`.
  QE-215 is the **enforcement** of that contract (QE-009 is explicit that enforcement is QE-215/216).
- QE-214 emits `TargetPosition { notional: Notional }` (signed) and the `CapitalView { equity,
  available_margin }` the governor needs. Both live in `qe_runtime::hedger`.
- **Placement: new `crates/runtime/src/pretrade.rs`** (the netting→hedger boundary sits in `qe-runtime`).
  It *must* live here, not in `qe-risk`: it consumes both `qe_risk::RiskLimits` and the `qe-runtime`
  `TargetPosition`/`CapitalView`, and `qe-risk` cannot depend on `qe-runtime` (that edge already runs the
  other way). `qe-runtime` already depends on `qe-risk` + `qe-domain` → no new dependency, QE-132 firewall
  unaffected.

## Design

### D1 — `PreTradeGovernor`

```rust
pub struct PreTradeGovernor { limits: RiskLimits, maintenance_margin_rate: Fraction }
```

`maintenance_margin_rate` (mmr) is the venue maintenance-margin rate the liquidation-distance and
margin-utilisation models need; it is a venue constant, supplied at construction.

### D2 — the check and its verdict

```rust
pub enum PreTradeVerdict { Send(Notional), Reject, Halt }   // severity Halt > Reject > Send/clamp
pub struct PreTradeDecision { pub verdict: PreTradeVerdict, pub breaches: Vec<LimitBreach> }

impl PreTradeGovernor {
    pub fn new(limits: RiskLimits, maintenance_margin_rate: Fraction) -> Self;
    pub fn check(&self, target: TargetPosition, capital: CapitalView) -> PreTradeDecision;
}
```

`check` computes the metrics from `mag = |target.notional|`, `equity`, `available_margin`, `mmr`, tests each
**configured** cap (a `None` cap is skipped), records a `LimitBreach` (with the kind's default outcome) per
violation, and reduces to a verdict by **outcome severity**:

- **`Clamp`** caps (`MaxNotional`, `MaxLeverage`) shrink the sendable magnitude to the cap; the running clamp
  is the **min** over all clamp caps. If only clamp breaches fire → `Send(sign × clamped_mag)`.
- **`Reject`** caps (`MaxGross`, `MaxNet`, `LiquidationDistanceFloor`, `MarginUtilisationCeiling`) mean the
  target is unsafe in a way that must not be silently resized → verdict `Reject` (send **no** new target;
  the position is left as-is — "refuse this order but keep trading"). Reject outranks Clamp.
- **`Halt`** (contract-general; no pre-trade kind defaults to it here) outranks all → flatten-and-halt.
- No breach → `Send(target.notional)` unchanged.

### D3 — the metrics (documented, `Decimal`-exact)

For `mag = |notional|`, `E = equity`, `A = available_margin`, `m = mmr`:

| Cap | Metric | Breaches when |
|---|---|---|
| MaxNotional | `mag` | `mag > cap` → clamp to `cap` |
| MaxLeverage | `mag / E` | `mag > max_leverage × E` → clamp to `max_leverage × E` (E ≤ 0 with a position ⇒ clamp to 0) |
| MaxGrossExposure | `mag` | `mag > cap` |
| MaxNetExposure | `\|net\| = mag` | `mag > cap` |
| **LiquidationDistanceFloor** | `E / mag − m` (adverse price fraction to liquidation) | `E/mag − m < floor` (only when `mag > 0`) |
| MarginUtilisationCeiling | `(mag × m) / A` (share of available margin the position's maintenance requirement consumes) | `util > ceiling` (`A ≤ 0` with a position ⇒ breach) |

The liquidation-distance model is the maintenance-margin-adjusted margin ratio `E/mag − m`: at `mmr = 0` it is
`1/leverage` (the adverse move that exhausts equity); a positive `mmr` makes it conservative. It is a
deliberately venue-agnostic bound — an exact per-venue liquidation price (tiered maintenance, funding) is a
later refinement; the floor here is a sound leverage/solvency governor. A flat target (`mag = 0`) trivially
passes every cap.

## Test plan (deterministic)

1. `within_all_caps_sends_target_unchanged` — a target inside every cap → `Send(notional)` verbatim, no
   breaches.
2. `oversized_notional_is_clamped` (**AC — clamp**) — `mag > max_notional` → `Send(sign × max_notional)`, one
   `MaxNotional`/`Clamp` breach; sign preserved for a short.
3. `excess_leverage_is_clamped` — `mag > max_leverage × equity` → clamped to `max_leverage × equity`.
4. `unsafe_liquidation_distance_is_rejected` (**AC — headline**) — a target whose `E/mag − mmr < floor` →
   `Reject`, with a `LiquidationDistanceFloor` breach; nothing is sent.
5. `gross_and_net_and_margin_breaches_reject` — each Reject cap independently yields `Reject`.
6. `reject_outranks_clamp` — a target that is both oversized (clamp) *and* breaches the liq floor (reject) →
   `Reject` (not a clamped send), proving severity ordering.
7. `verdict_severity_prefers_halt` — the severity reducer returns `Halt` when a `Halt`-outcome breach is
   present (exercises the contract-general Halt path).
8. `flat_target_passes` — `mag = 0` → `Send(0)`, no breaches, even with tight caps.

## Risks

- **Liquidation model is a documented approximation.** `E/mag − mmr` is venue-agnostic; exact tiered
  maintenance / funding is deferred (QE-217 venue specifics + QE-130 stress evidence). The floor remains a
  conservative solvency gate; documented so a later exact model is localised.
- **Single-instrument gross/net.** With one netted target, gross = net = `mag`; a multi-instrument portfolio
  view sums across instruments (same cap types) — deferred with the multi-instrument vintage.
- **Drawdown cap not enforced here** (out of scope) — it is the QE-212 breaker + QE-216 kill path; documented.
