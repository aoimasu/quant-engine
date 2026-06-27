# QE-109 — Execution-friction & funding model — design note

`Phase: P1` · `Area: ⑤ WFO (backtest realism)` · `Depends on: QE-105, QE-107`
`Branch: qe-109/execution-friction-funding`

## Goal (from backlog)

*(Reviewer-added; BLOCKS the backtester.)* For linear perps, fees and funding are first-order P&L. A
frictionless backtest biases the archive toward high-turnover fee-losers and net-negative-after-funding
trend strategies.

- Fee schedule (taker/maker by tier), **default Binance USDT-M VIP0: taker 0.05% / maker 0.02%**;
  funding accrued to held positions at venue stamps (8h) **from the actual historical funding series,
  not a constant**; spread-cross + **size-dependent** slippage; **next-bar-open** fill convention. All
  parameters configurable.
- A **cost-sensitivity sweep** utility (e.g. 1×/2× assumed costs) for reporting.

**Acceptance criteria.**
- [ ] Backtest P&L is net-of-cost and funding-adjusted; a turnover-1 strategy shows fee drag.
- [ ] A held-through-funding directional strategy shows the correct funding sign in P&L.
- [ ] Cost-sensitivity sweep is available to the validation report (QE-133).

**Out of scope.** Live execution mechanics (QE-217); the full strategy backtester / evaluation (QE-120).

## Current-state evidence

- `qe-wfo` is a scaffold depending on `qe-domain` + `qe-signal` + `qe-storage`. This is "⑤ WFO
  (backtest realism)", the right home for the cost/funding model the backtester (QE-120) will call.
- `qe-domain` gives `Side` (Buy/Sell → Long/Short) and exact `rust_decimal` money. P&L and funding
  cashflows are **signed**, so they are computed in `Decimal` (never float), consistent with QE-007.
- There is no backtester yet (QE-120). QE-109 therefore ships the **cost model + a minimal,
  self-contained P&L attribution** (position accounting over a fill/funding event stream) that proves
  the ACs and that QE-120 will drive; it does not build strategy logic.

## Decisions

### D1 — A pure, configurable cost model + a small event-driven P&L attributor

`FrictionConfig { fees, slippage, cost_multiplier }` holds every parameter (all defaulted to Binance
USDT-M VIP0). `simulate(events, &config) -> PnlBreakdown` walks a `Vec<Event>` (`Fill` or
`FundingStamp`), maintaining a signed average-cost `Position`, and returns a **decomposed** P&L:
`{ gross, fees, slippage, funding }` with `net = gross − fees − slippage + funding`. Decomposition is
what makes the ACs (fee drag, funding sign) directly assertable and feeds the QE-133 report.

### D2 — Funding from the actual series, signed correctly

A `FundingStamp { rate, mark_price }` carries the **historical** rate (not a constant) and the mark to
value the position. Cashflow **to the trader** = `−signed_qty · mark_price · rate` (longs pay shorts
when `rate > 0`). It is accrued to whatever position is held at the stamp — so a directional position
held across an 8h stamp shows the correct funding sign (AC #2).

### D3 — Cost multiplier scopes *assumed* costs only (the sweep knob)

`cost_multiplier` scales **fees + slippage** (the modelled/assumed frictions) but **not funding**
(funding is a realised market cashflow, not an assumption). `cost_sweep(events, base, multipliers)`
re-runs `simulate` at each multiplier → `Vec<(multiplier, PnlBreakdown)>` for the 1×/2× sensitivity
report (AC #3).

### D4 — Fees / slippage / fill convention

- **Fees:** `FeeSchedule { taker, maker }` as fractions; `fee = |notional| · rate(liquidity) ·
  multiplier`. Default VIP0 taker `0.0005` / maker `0.0002`. Tiered schedules are just a different
  `FeeSchedule`.
- **Slippage:** `SlippageModel { half_spread, impact }`; per-fill cost = `|notional| · (half_spread +
  impact · |qty|) · multiplier` — a spread-cross term plus a **size-dependent** impact term.
- **Fill convention:** the model is fed fills already stamped at the **next bar open** (the convention
  is the caller's; documented on `Fill`). QE-120 supplies next-bar-open prices.

## Module / API plan (`crates/wfo/src/friction.rs`, new)

- `Liquidity { Taker, Maker }`.
- `FeeSchedule { taker, maker }` (+ `Default` = VIP0), `rate(Liquidity)`, `fee(notional_abs, liq)`.
- `SlippageModel { half_spread, impact }` (+ `Default`), `cost(notional_abs, qty_abs)`.
- `FrictionConfig { fees, slippage, cost_multiplier }` (+ `Default`); `with_multiplier(m)`.
- `Fill { side: Side, qty, price, liquidity }`; `FundingStamp { rate, mark_price }`; `Event`.
- `Position` (signed qty + avg price) with `apply(side, qty, price) -> realized_gross` (average-cost,
  handles add / reduce / flip).
- `PnlBreakdown { gross, fees, slippage, funding }`, `net()`.
- `simulate(events: &[Event], cfg: &FrictionConfig) -> PnlBreakdown`.
- `cost_sweep(events, base: &FrictionConfig, multipliers: &[Decimal]) -> Vec<(Decimal, PnlBreakdown)>`.
- `lib.rs` wiring + re-exports.

## Test plan (TDD)

- **AC #1 (fee drag):** a turnover-1 round trip (buy 1 @100 taker, sell 1 @100 taker) at flat price →
  `gross == 0`, `fees > 0`, `net < 0` (== `−fees − slippage`).
- **AC #2 (funding sign):** long 1 @100 then `FundingStamp{ +rate, 100 }` → `funding < 0` (long pays);
  the symmetric short → `funding > 0`; a negative rate flips both. Zero position at a stamp → `0`.
- **AC #3 (sweep):** `cost_sweep` at `[1, 2]` doubles fees + slippage, leaves `gross` and `funding`
  unchanged; `net` reflects the larger assumed costs.
- **Position accounting:** add (avg price), partial reduce (realised gross on the closed qty), and a
  flip (realise all, reopen remainder) — hand-computed.
- **Config:** defaults equal VIP0; `with_multiplier` scopes fees+slippage only.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
`cargo test --workspace --locked`; `cargo deny check` (only `rust_decimal`/`thiserror`, both already
workspace deps — no new third-party crates); topology guard (`qe-wfo` already may depend on
domain/signal/storage; the QE-001 `runtime ⊥ wfo` edge is untouched — nothing new points *into*
runtime).

## Risks

- **Average-cost realisation** is the standard convention; documented. Funding mark uses the stamp's
  `mark_price` (the historical mark/close), supplied by the caller — not invented here.
- **Scope:** no strategy logic, signals, or walk-forward windowing (QE-110+/QE-120); QE-109 is the cost
  primitive + attribution + sweep the backtester and the QE-133 report consume.
