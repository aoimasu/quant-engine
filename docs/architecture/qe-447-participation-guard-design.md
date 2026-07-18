# QE-447 — Pre-trade %ADV participation guard: design & evidence note

`Phase: Review R2 (P3 — panel #18, unanimous)` · `Area: hedger` · `Depends on: QE-215, QE-219` ·
`Spec of record: docs/reviews/2026-07-16-maxdama-panel-review.md#qe-447` · `Backlog: docs/backlog.md → Review R2.b`

## 1. What the ticket asks

Add a **pre-trade %ADV participation guard** to `crates/hedger/src/pretrade.rs`: reject/flag any delta-close
order whose participation `order_qty / ADV` exceeds a configured threshold. There is **no %ADV concept in the
runtime today**; at current AUM participation is ~0, so this is **latent** — it only bites when AUM grows or
liquidity thins in a stress regime. A cheap safety rail that also sanity-checks capacity.
Spec ref: maxdama §7 intro ("participation = %ADV; 1% is already high").

## 2. Where ADV comes from (evidence)

- **QE-440** introduced the engine's only rolling-ADV concept, `adv_notional: f64` (rolling hourly ADV **in
  dollars** of the traded instrument), living in the **wfo / ensemble** cost path:
  - `crates/ensemble/src/capacity.rs:38-40` — `pub adv_notional: f64` on the capacity profile; participation
    `u = traded_notional / adv_notional`, with the explicit contract "**Non-finite / non-positive ⇒ no
    modellable capacity**" (fail-safe precedent).
  - `crates/ensemble/src/capacity.rs:96-99` — `slippage_cost(notional, adv_notional)`: participation is
    `notional / adv_notional` **only when `adv_notional > 0.0`**, else the spread-crossing term only (no
    divide-by-zero).
  - `crates/risk/src/slippage.rs:35-95` — the dimensionless participation `u = traded / ADV` used by the
    √-in-participation impact model.
- **grep confirms there is no ADV on the hedger / pre-trade / live-order side.** The `adv` hits in
  `crates/hedger/src/live_kline.rs` and `evaluator.rs` are the word "ad**v**ance", not ADV. There is **no live
  rolling-ADV feed reaching `pretrade`** today. This matches the ticket ("there is no %ADV concept in the
  runtime today", "check whether an ADV value is reachable at pretrade / needs to be passed in").

**Conclusion: ADV must be _passed in_ to the governor.** It is a rolling **market-liquidity** quantity (dollars
of hourly volume), not account capital, so it does **not** belong on `CapitalView` (equity/available-margin,
the QE-426 planner↔edge seam contract in `qe-runtime-core`). We add it as an **optional governor input**
(`Option<Notional>`, dollars — same unit as `adv_notional`), supplied via a `with_adv(..)` builder that leaves
the existing `PreTradeGovernor::new(limits, mmr)` signature (and all its call sites) untouched. When a live
rolling-ADV feed is added, it wires into `with_adv` from the same QE-440 ADV source; until then ADV is `None`.

## 3. Design — follow the existing limit pattern exactly

- **New `LimitKind::MaxParticipation`** in `crates/risk/src/limit.rs`, added to `default_outcome` and `as_str`.
  - **Default outcome = `Reject`** (like `MaxGrossExposure` / `MarginUtilisationCeiling`): a liquidity breach
    must **not** be silently resized into a smaller order — refuse it, keep trading, position unchanged.
    (Severity `Reject` outranks the `Clamp` caps and is outranked by `Halt`, via the existing reducer.)
- **New `RiskLimits.max_participation: Option<Fraction>`** — the cap as a fraction of ADV (e.g. `0.01` = 1% of
  hourly ADV). `Fraction` is `[0,1]`, exactly the %ADV domain, and reuses the validated serde boundary.
- **Guard in `PreTradeGovernor::check`**, placed with the other **Reject** caps. Participation numerator is the
  **order magnitude** `mag = |notional|` (the delta-close order quantity), denominator is ADV — matching the
  spec's `order_qty / ADV` and `capacity.rs`'s `traded_notional / adv_notional`.

## 4. Default-OFF (no golden moves)

- `max_participation` defaults to `None` via `#[derive(Default)]` on `RiskLimits` → the guard branch is never
  entered unless configured. At default, **no order is ever rejected by this guard** and behaviour is
  identical to pre-QE-447.
- **No golden / fixture serializes `RiskLimits`** — verified by grep over `crates/**/tests`, `**/fixtures`,
  and `--include=*.json` for `max_leverage` / `liquidation_distance_floor` / `RiskLimits` (no hits). Adding an
  `Option` field therefore moves **no `content_hash`**. The `risk_limits_round_trips` serde test round-trips a
  value built with `..RiskLimits::default()`, so the new `None` field round-trips cleanly.
- **No new firewall edge:** `qe-hedger` already depends on `qe-risk` and `qe-runtime-core`; the change adds no
  crate dependency, so `qe-architecture`'s firewall test is unaffected.

## 5. Fail-safe on 0 / None / unknown ADV (no panic — QE-268)

`pretrade.rs` carries `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` (QE-268, live order
path). The guard is panic-free by construction:

- **Cap configured, order flat (`mag == 0`):** participation is 0 → no breach (a flat/close-to-zero order is
  always safe). No division performed.
- **Cap configured, `mag > 0`, ADV present and `> 0`:** participation `= mag / adv` via
  **`Decimal::checked_div`** (never the panicking `/`); breach iff `participation > cap`.
- **Cap configured, `mag > 0`, ADV `None` / `≤ 0` / `checked_div` overflow:** **fail-closed → `Reject` breach**
  ("adv unknown/non-positive"). This matches the repo's fail-closed convention for a degenerate required input
  with a live position: `LiquidationDistanceFloor` and `MarginUtilisationCeiling` both **reject** when their
  input (equity / available-margin) is `≤ 0` with `mag > 0`, and `MaxLeverage` clamps to flat. We never divide
  by zero (ADV `> 0` is checked before the division) and never index/unwrap/expect/panic.

## 6. Money exactness

Participation numerator (`mag`), ADV (`Notional` → `Decimal`), and the threshold (`Fraction` → `Decimal`) are
all exact `rust_decimal::Decimal`; the comparison and `checked_div` are exact. No `f64` on the guard path.

## 7. Tests (TDD)

1. Order **over** the configured %ADV cap → `Reject`, with the participation value in the breach detail.
2. Order **within** the cap → no participation breach (sent).
3. **Default `None`** cap → guard inert, current behaviour unchanged (no participation breach even at huge size).
4. **Fail-safe:** cap configured but ADV `None` / `0` / negative with `mag > 0` → `Reject`, no panic; flat
   target with unknown ADV → no breach.
5. **Policy parity:** the `MaxParticipation` breach carries `LimitOutcome::Reject` and is outranked by `Halt`,
   outranks `Clamp` — same reducer as its siblings; a participation `Reject` + a `MaxNotional` clamp → `Reject`.
