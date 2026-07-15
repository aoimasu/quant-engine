# QE-418 — Pre-trade gross cap checked against true gross exposure, not net notional

`Phase: P2` · `Area: trading / pre-trade risk` · `Depends on: QE-213, QE-215` · `Effort: S`
Authoritative spec: `docs/reviews/2026-07-15-team-improvement-review.md` § `### QE-418`.

## Problem (current state, file:line)

The netter already computes and exposes true per-side exposure, but the governor never sees it:

- **Netter** — `crates/runtime/src/live_netter.rs:87-103`: `NetTarget { net, long, short }` carries the net
  signed target *and* both side totals; `NetTarget::gross()` (`:99-102`) returns `long + short`. The per-side
  split is real (`net(...)` at `:111-126` accumulates `long`/`short` separately).
- **Hedger** — `crates/runtime/src/hedger.rs:91-96`: `plan(net) -> TargetPosition` scales **only** `net.net ×
  equity` into `TargetPosition { notional }` (`:49-54`). **Gross is dropped here** — the information-loss point.
- **Governor** — `crates/runtime/src/pretrade.rs:82-142`: `check` computes `mag = notional.abs()` (`:83-84`) and
  checks **both** `MaxGrossExposure` (`:125-133`) and `MaxNetExposure` (`:134-142`) against that single `mag`.

For a single instrument `gross == |net|`, so the gross check is correct **today**. But the config is explicitly
count-agnostic; once the universe grows or any hedged/offsetting book exists, true gross exposure exceeds net and
the gross cap silently passes oversized books. The gross-vs-net distinction is the entire reason both caps exist.

`PreTradeGovernor::check` has **no production caller yet** (grep: only its own unit tests construct it). So the
"boundary" is the `TargetPosition` struct that flows netter → hedger → pretrade; there is no pipeline call site to
disrupt.

## The minimal signature change

Carry gross through the one struct that flows the boundary — `TargetPosition` — rather than adding a parameter to
a `check` that has no production caller:

1. `TargetPosition` gains an unsigned `gross: Notional` field (long + short, absolute notional; `≥ |notional|`).
   Add `TargetPosition::single(notional)` — the single-instrument constructor where `gross == |notional|` by
   construction — for every existing call site (transport/shadow/pretrade test helpers) that does not model a
   hedged book.
2. `HedgePlanner::plan` scales gross too: `gross = net.gross() × equity`.
3. `PreTradeGovernor::check` reads `let gross = target.gross.get().abs();` and checks `MaxGrossExposure` against
   `gross`; `MaxNetExposure` continues to check `mag = |net|`. All other caps (`MaxNotional`, `MaxLeverage`,
   `LiquidationDistanceFloor`, `MarginUtilisationCeiling`) stay on `mag` — **out of scope** for QE-418.

## Single-instrument parity

`TargetPosition::single(n)` sets `gross = |n|`, so `gross == mag` and the `MaxGrossExposure` branch behaves
exactly as before for every single-instrument path. All existing pretrade tests use the `single` helper and stay
green with no semantic change. The only place `gross != mag` is the new multi-instrument AC test.

## Affected tests

- `crates/runtime/src/pretrade.rs` — `target()` helper → `TargetPosition::single(...)`. New AC test:
  `long = short = X` (net 0, gross 2X) breaches `MaxGross < 2X` while passing the net cap. New parity test:
  single-instrument `gross == |net|` for long and short.
- `crates/runtime/src/hedger.rs` — `plan` now sets `gross`; add an assertion that `plan` scales `net.gross()`.
- `crates/runtime/src/transport.rs`, `crates/runtime/src/shadow.rs` — test `TargetPosition { notional }` literals
  → `TargetPosition::single(...)` (these exercise transport/shadow mechanics, not gross; parity-preserving).

## Risks

- **Blast radius of a new required field**: every `TargetPosition` literal must set `gross`. Mitigated by the
  `single` constructor; all non-hedged call sites use it. Compiler enforces completeness.
- **Negative/degenerate equity**: governor uses `gross.get().abs()`, mirroring `mag = notional.abs()`, so the
  degenerate-capital cases behave identically to net.
- **Out of scope (noted, not fixed)**: leverage/liquidation/margin caps still use net magnitude; whether they
  should use gross is a separate risk decision, not QE-418. Multi-instrument netting itself is out of scope.

## Panic-freedom

Order-emission path: no new `unwrap`/`expect`/`panic`. Uses `Decimal` arithmetic and `.abs()` only
(`#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` remains satisfied).
