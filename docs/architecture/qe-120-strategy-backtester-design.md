# QE-120 — Strategy backtester (the fitness engine) — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-109, QE-113, QE-118`
`Branch: qe-120/strategy-backtester`

## Goal (from backlog)

The fitness engine: evaluates genomes net-of-cost with noise-robust geometric fitness and a
minimum-trade-count floor.

- Evaluate a genome over features (QE-108) with frictions/funding (QE-109); compute net-of-cost
  geometric fitness (QE-113); reject elites below a minimum trade count.
- Noise-robust multi-window/bootstrap evaluation feeding archive replacement decisions.

**Acceptance criteria.**
- [ ] Fitness is net-of-cost; a `<N`-trade genome is rejected as noise.
- [ ] Replacement respects standard error (no replace-on-noise).

**Out of scope.** Elite robustness gates (QE-124).

## Current-state evidence

This ticket is the **integration** that turns a `Genome` into a `NoiseRobustFitness`, wiring three
already-merged pieces:
- **QE-110** `Genome::decide(features, PositionState) -> Decision {Hold, Enter(dir), Exit}` — the
  per-bar signal (flat ⇒ one-sided entry; in-position ⇒ exit on holding cap or opposite signal).
- **QE-109** `qe_wfo::friction`: `FeeSchedule::fee` (taker/maker), `SlippageModel::cost`,
  `FrictionConfig` (with a `cost_multiplier` for sensitivity), `Liquidity::Taker`. The cost *formulas* —
  reused per fill so the return series is genuinely net-of-cost.
- **QE-113** `qe_wfo::fitness`: `log_growth` (time-average net-of-cost geometric fitness, ruin-absorbing),
  `NoiseRobustFitness::from_windows` (mean ± SE over windows), `should_replace` (SE-aware, no
  replace-on-noise).

## Design

### D1 — Execution model (no look-ahead)

Bar-by-bar over `&[Bar]` (`Bar { features, price, funding_rate }`). The signal at bar `i` is computed
from `features_i` and the position held *as of* bar `i`; the resulting order **fills at bar `i+1`'s
price** (next-bar fill — never the same bar the signal was seen, so no look-ahead). A decision on the
final bar cannot fill (no next bar). An open position at the end is left marked-to-market in the equity
curve (its unrealised P&L is already in the last return).

Per bar `i`: (1) execute the order pending from `decision_{i-1}` at `price_i`; (2) accrue funding
(`−pos_qty · price_i · funding_rate`, QE-109 sign); (3) mark `equity_i = cash + pos_qty · price_i` and
record `r_i = equity_i / equity_{i-1} − 1` (skipping the bar-0 baseline); (4) `decision_i =
decide(features_i, {dir, bars_held})`; schedule the fill for `i+1`.

### D2 — Sizing & cash/mark accounting (net-of-cost)

Target notional = `(size_bps / 10000) · equity` (so `size_bps ≤ MAX_SIZE_BPS = 10000` ⇒ ≤ 1× leverage,
no leverage-only ruin); `qty = notional / price`. Pure **cash + mark** accounting in `Decimal` (no float
money): a fill moves `cash −= signed_qty · price` and `cash −= (fee + slippage)·cost_multiplier`; funding
moves cash; `equity = cash + pos_qty · mark`. Because fees/slippage/funding reduce cash, every `r_i` is
**net-of-cost** by construction — raising `cost_multiplier` strictly lowers fitness (AC1). Fills take
liquidity (`Taker`).

### D3 — Noise-robust geometric fitness (QE-113)

The net return series is split into `windows` contiguous sub-windows; `NoiseRobustFitness::from_windows`
gives the mean per-window `log_growth` and its standard error. That is the genome's fitness as a
**distribution**, ready for the SE-aware replacement rule. The scalar archive fitness (QE-118 stores
`f64`) is `fitness.mean`.

### D4 — Minimum-trade gate (AC1)

`trades` counts entry fills (flat → position). If `trades < min_trades` the genome is **rejected as
noise**: `accepted = false` and its `fitness.mean = −∞`, so it can never become or displace an elite
(`should_replace` rejects a non-finite challenger). A handful of lucky trades is not signal.

### D5 — Replacement respects standard error (AC2)

The backtester does not itself mutate the archive; it produces the `NoiseRobustFitness` that the
replacement decision consumes. `should_replace(incumbent, challenger, k_sigma)` (QE-113) only displaces
when the challenger's mean beats the incumbent's by more than `k_sigma` **combined standard errors** — an
improvement inside the noise band is rejected, so the archive never churns elites on a noisy single
draw. With `windows ≥ 2` the backtester's SE is a real estimate, so the guard bites.

## Module / API plan

New module `crates/wfo/src/backtest.rs`, re-exported:

- `Bar { features: FeatureVector, price: Decimal, funding_rate: Option<Decimal> }`.
- `BacktestConfig { friction: FrictionConfig, min_trades: usize, windows: usize }` (+ `Default`).
- `BacktestResult { returns: Vec<f64>, trades: usize, net_pnl: Decimal, accepted: bool, fitness: NoiseRobustFitness }` (+ `elite_fitness()` = `fitness.mean`).
- `backtest(genome, bars, cfg) -> BacktestResult`.
- `DEFAULT_MIN_TRADES`, `DEFAULT_WINDOWS`.
- Reuses `qe_wfo::{friction, fitness, genome}`; `qe_domain::{Side, Direction}`; `Decimal`. No new deps.

## Test plan (TDD)

1. **Net-of-cost (AC1).** The same genome/series at a higher `cost_multiplier` yields strictly lower
   fitness mean and net P&L (fees drag returns).
2. **Minimum-trade gate (AC1).** A genome that (almost) never fires → `accepted = false`, `fitness.mean
   = −∞`, and it never replaces a finite incumbent.
3. **Geometric fitness sign.** A profitable up-trend long strategy has positive fitness; a series with a
   ruinous bar drives fitness to `−∞`.
4. **Replacement respects SE (AC2).** The backtester yields `n = windows`, `std_error > 0`; a challenger
   inside the SE band does **not** replace, one well outside **does**, and a rejected (under-trade)
   challenger never replaces.
5. **No look-ahead / determinism.** A signal never fills on its own bar; the backtest is a pure function
   of `(genome, bars, cfg)` (same inputs → identical result).

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Sizing/accounting simplifications.** Unit starting capital, mark = bar price, taker fills, ≤ 1×
  leverage. Faithful for relative fitness (the search only needs a consistent net-of-cost ordering); a
  richer margin/liquidation model is a later refinement, documented.
- **Window split is contiguous, not bootstrap.** Contiguous sub-windows give a cheap, deterministic SE;
  block-bootstrap is a future option (QE-124) — `from_windows` is agnostic to how windows are formed.
- **Funding is per-bar when supplied.** The caller passes the historical rate on funding bars; the
  backtester does not synthesise funding schedules (that is the data layer, QE-105/109).
