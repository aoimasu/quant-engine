# QE-128 — Capacity analysis gating ensemble weights — design note

`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-126, QE-109`
`Branch: qe-128/capacity-gating`

## Goal (from backlog)

*(Reviewer-added.)* Weights are fiction at size if per-strategy capacity is ignored — a high-turnover
scalper may have edge at $10k and none at $1M.

- Estimate per-strategy capacity (impact model × turnover × target AUM) and cap weights to respect
  capacity at the configured AUM.

**Acceptance criteria.**
- [ ] A high-turnover strategy's ensemble weight is capped by its modelled capacity at target AUM.

**Out of scope.** Live impact measurement.

## Current-state evidence & the firewall constraint

- **QE-109's impact model** is `qe_wfo::friction::SlippageModel`, whose cost form is
  `notional · (half_spread + impact · qty)` — a spread-cross term plus a **size-dependent** impact term.
  That size term is exactly what bounds capacity.
- **But the firewall (QE-001/QE-132) forbids `qe-ensemble → qe-wfo`** (`ensemble`'s `Cargo.toml` says so
  explicitly). So QE-128 cannot import `SlippageModel`. It instead carries its **own parameterisation of
  the same impact form** — a `CapacityModel { half_spread, impact_coeff }` mirroring QE-109's
  `(half_spread, impact)` — so the *model* is QE-109's, the *coefficients* are config the portfolio side
  owns. This is the only firewall-clean way to "depend on QE-109" from the ensemble; the coefficients are
  what a shared config / calibration would supply in production.
- **QE-126** produces the ensemble (an `EnsembleMask`); its members are equal-weighted by default
  (`combined_returns`). QE-128 turns those equal weights into **capacity-capped** weights.

## Design

### D1 — Per-strategy capacity

A strategy with per-period **turnover** `τ` (fraction of AUM traded each period) deployed at AUM `W`
trades `τ·W` notional per period. Borrowing QE-109's impact form, its per-period cost drag (as a fraction
of AUM) is `τ · (half_spread + impact_coeff · τ·W)` = `τ·half_spread + impact_coeff·τ²·W`. Its net
per-period edge is therefore

```
net(W) = gross_edge − τ·half_spread − impact_coeff·τ²·W
```

— linearly **decreasing** in `W`. **Capacity** `W*` is the AUM at which the net edge falls to a retained
floor `edge_retention · gross_edge`:

```
W* = (gross_edge·(1 − edge_retention) − τ·half_spread) / (impact_coeff · τ²)
```

Because the size term scales with `τ²`, capacity falls **quadratically** in turnover — a high-turnover
scalper's capacity is orders of magnitude below a low-turnover strategy's at the same edge, which is the
whole point. Two guards: if the spread-cross alone already erodes the usable edge (numerator ≤ 0) capacity
is `0` (uneconomic at any size); if there is no size impact (`impact_coeff·τ² = 0`) capacity is `+∞` (no
size limit).

### D2 — Capacity-capped weights (AC)

Each strategy's allocated capital at target AUM `W_t` is `weight_i · W_t`; respecting capacity means
`weight_i ≤ capacity_i / W_t`. `cap_weights(weights, capacities, target_aum)` does a standard
**water-filling**: distribute the unit weight budget proportionally to the input weights, fix any strategy
whose share would exceed its cap at the cap, redistribute the freed budget to the uncapped strategies, and
repeat until stable. If the caps cannot absorb the whole budget the remainder stays uninvested (weights
sum to `< 1`) — a faithful "the ensemble is capacity-constrained at this AUM" signal. A high-turnover
strategy whose `capacity_i / W_t` is below its nominal weight is capped down; the slack flows to
strategies with spare capacity.

## Module / API plan

New module `crates/ensemble/src/capacity.rs`, re-exported:

- `StrategyProfile { gross_edge, turnover }`, `CapacityModel { half_spread, impact_coeff, edge_retention }`
  (+`Default`/`with_defaults`), consts `DEFAULT_HALF_SPREAD`, `DEFAULT_IMPACT_COEFF`, `DEFAULT_EDGE_RETENTION`.
- `capacity(profile, model) -> f64` (dollars; `0` if uneconomic, `+∞` if no size impact).
- `cap_weights(weights, capacities, target_aum) -> Vec<f64>` (water-filling).
- No new deps; **no `qe-wfo` import** (firewall preserved).

## Test plan (TDD)

1. **High-turnover weight capped at target AUM (AC).** Two strategies with equal nominal weight — a
   high-turnover (low-capacity) scalper and a low-turnover (high-capacity) strategy. At a target AUM above
   the scalper's capacity, its capped weight equals `capacity / target_aum` (strictly below its nominal
   weight), and the freed weight flows to the high-capacity strategy.
2. **Capacity falls with turnover.** Same edge, higher turnover ⇒ strictly lower capacity (quadratic).
3. **No capping below capacity.** At a target AUM below every strategy's capacity, weights are unchanged.
4. **Guards.** Uneconomic strategy (spread alone eats the edge) ⇒ capacity `0` ⇒ weight `0`; zero-impact ⇒
   `+∞` capacity ⇒ never capped.
5. **Water-filling conserves / redistributes.** Capped slack flows to uncapped strategies; output sums to
   `min(1, Σ caps)`.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-ensemble`,
`cargo test --workspace`.

## Risks

- **Impact-form duplication vs `qe-wfo`.** The firewall makes a shared type impossible without moving
  `SlippageModel` to a lower crate (a bigger QE-109/120 refactor, out of scope here). The duplicated form
  is tiny and documented; a future shared `qe-microstructure` crate could host both. The *coefficients*
  are config, so the two sides stay consistent through configuration, not code coupling.
- **Linear impact model.** Real impact is often concave (square-root law); the linear-in-size term is the
  QE-109 model and is config-ready. Swapping the functional form is localised to `capacity`.
- **Edge/turnover are inputs.** QE-128 estimates capacity from a per-strategy `(gross_edge, turnover)`
  profile; deriving those from the strategy's own backtest is the caller's job (the profile is the clean
  seam). Live impact measurement is explicitly out of scope.
