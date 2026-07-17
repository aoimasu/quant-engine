# QE-440 — Concave √-in-participation impact model + rolling ADV

`Phase: Review R2 (P2 — panel #11)` · `Area: wfo / ensemble` · `Depends on: QE-128, QE-109, QE-431`

Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-440`](../reviews/2026-07-16-maxdama-panel-review.md).
Backlog: [`docs/backlog.md`](../backlog.md) → Review R2.b (rank #11).

---

## 1. The defect (old model)

Both cost sides charge a size term that is **linear in the traded amount** — i.e. the *total* impact
cost is **quadratic** (convex), the opposite of maxdama §7.7's single-curve collapse (impact is
**concave**, ≈ square-root in participation):

| Side | File | Cost (fraction of notional) | Size-term unit | Total impact ∝ |
|---|---|---|---|---|
| Friction | `crates/wfo/src/friction.rs` | `half_spread + impact·qty` | per **contract** (`1e-4`) — price-scale dependent | `qty²` |
| Capacity | `crates/ensemble/src/capacity.rs` | `half_spread + impact_coeff·notional` | per **dollar** (`2e-9`) — asset-portable | `notional²` |

Consequences the panel flagged:

- **Convex (quadratic-total) overstates large-order cost** (conservative on friction) but
  **under-states capacity**, and cannot be reconciled with any *measured* impact (which is concave).
- The two sides live in **two unit systems** (per-contract vs per-$). QE-431 hoisted the coefficients
  into one `SlippageCalibration`, but the friction side still converts to a **price-scale-dependent
  per-contract** coefficient (`friction_impact_per_contract = impact_per_notional · reference_mark`).
  The QE-431 reviewer explicitly flagged this as the seam to fix here: **make the coefficient
  participation-keyed** so it is dimensionless and asset-portable — no `reference_mark` conversion.
- There is **no %ADV concept anywhere** in the engine (grep-confirmed; also QE-447's premise), so the
  coefficient could never be dimensionless.
- The CLI already advertises a `"square-root-impact"` contract tag (`slippage_model`) while the engine
  runs linear — this closes that gap.

## 2. The new model — impact concave in participation

Define dimensionless **participation** `u = traded / ADV` (order size as a fraction of a rolling ADV),
and charge

```
cost(notional) = notional · ( half_spread + impact_coeff · u^β )      u = traded / ADV,  0 ≤ β < 1
```

- `impact_coeff` — the impact **fraction of notional at u = 1** (100 % of ADV). **Dimensionless,
  asset-portable, and shared verbatim by both sides** — no per-contract vs per-$ split, no
  `reference_mark`. This is the reviewer's reconciliation.
- `β` (`impact_exponent`) — the concavity exponent, `β ∈ [0.2, 0.5]`, **default `0.5`** (the
  square-root law, maxdama §7.7). Both `impact_coeff` and `β` are **calibrated inputs** on the shared
  `SlippageCalibration`, alongside `half_spread`.

**Concavity (the shape fix).** The per-unit impact **fraction** `impact_coeff · u^β` is concave in the
traded amount (`β < 1`): doubling the order at fixed ADV multiplies the impact fraction by `2^β < 2`
(sub-linear), versus the old linear term's exact `×2`. The *total* impact cost is `∝ traded^(1+β)`
(exponent in `(1, 2)`), strictly **less convex** than the old `traded²`.

**Reduces sensibly.** `u → 0` (or ADV missing / non-positive) ⇒ impact term `→ 0` ⇒
`cost = notional · half_spread` (spread-cross only). `u = 1` ⇒ `cost = notional·(half_spread +
impact_coeff)`.

### Friction ↔ capacity reconciliation (unit portability)

Participation is a **pure ratio**, computed in each side's native unit but numerically identical:

- Friction (Decimal, contracts): `u = qty / ADV_qty`.
- Capacity (f64, dollars): `u = notional / ADV_notional`.

For identical `(qty, mark, ADV)` with `notional = qty·mark` and `ADV_notional = ADV_qty·mark`:

```
notional / ADV_notional = (qty·mark) / (ADV_qty·mark) = qty / ADV_qty
```

so both sides evaluate the **same** `impact_coeff · u^β` with the **same** shared coefficient and β —
the coefficient-parity test (QE-431 AC1) still holds, now on the participation-keyed coefficient. The
firewall is respected: the reconciliation is **shared DATA/CONFIG via `qe-risk`
`SlippageCalibration`**, never a `qe-wfo → qe-ensemble` code edge (each side keeps its own unit
conversion — the sanctioned duplicated-CONFIG pattern).

### Capacity closed form (new)

Per-period traded notional `= turnover·W`, so `u = turnover·W / ADV$`. Net per-period edge:

```
net(W) = gross_edge − turnover·half_spread − turnover · impact_coeff · (turnover·W / ADV$)^β
```

Setting `net(W*) = edge_retention·gross_edge` and solving (`usable_edge = gross_edge·(1−edge_retention)
− turnover·half_spread`):

```
W* = (ADV$ / turnover) · [ usable_edge / (turnover · impact_coeff) ]^(1/β)
```

Guards (unchanged spirit): `usable_edge ≤ 0` ⇒ `0` (uneconomic at any size);
`turnover·impact_coeff = 0` or `ADV$` non-finite/≤0 ⇒ `+∞` (no modellable size cap). Capacity now
**scales linearly with ADV** and falls **super-linearly** with turnover (`1/turnover · turnover^(−1/β)`).

## 3. Rolling hourly ADV (the new input)

- **Source.** The OHLCV bars already carry per-bar (hourly) `volume` (`qe_domain::Bar::volume`, a
  `Qty`). It was **dropped** at the `to_decision_bars` bridge; QE-440 threads it onto the wfo
  `backtest::Bar` (`volume: Decimal`, in contracts) and onto `friction::Fill`.
- **Rolling ADV.** In the backtest loop, `ADV_i = mean(volume over the trailing `min(i+1, ADV_WINDOW)`
  bars ending at bar i)`, `ADV_WINDOW = 24` (≈ one day of hourly bars). Trailing + inclusive-of-current
  ⇒ **no look-ahead** (the fill at bar i uses bar i's price and contemporaneous volume, never a future
  bar). Exact `Decimal` running sum/count ⇒ byte-reproducible.
- **Participation** at a fill = `order_qty / ADV_i`. If `ADV_i ≤ 0` the impact term is 0 (spread only).
- **Capacity ADV$.** train's `strategy_capacities` computes one representative `ADV$ = mean(volume·close)`
  over the train bars (single-instrument v1) and threads it to `capacity`.

## 4. Determinism of the fractional power `u^β`

Money is exact `rust_decimal`; the fill cost feeds the Decimal cash ledger, so `u^β` **must not** leak
machine-dependent float into the sealed/hashed path.

- **Friction (Decimal, sealed path).** Uses `rust_decimal`'s `MathematicalOps::powd` (enable the
  `maths` feature). It is implemented in **pure Decimal integer arithmetic** (no hardware `f64`), so it
  is **byte-identical across platforms** (arm64 dev ↔ x86_64 CI). Verified: `0.01^0.5 = 0.1` exact,
  `1^β = 1`, `0^β = 0`, and repeated calls are byte-equal. A determinism unit test pins an exact
  expected `Decimal` literal for a representative `u^β`.
- **Capacity (f64, hashed weights).** The closed form uses `powf(1/β)`. `f64::powf` is **not**
  guaranteed correctly-rounded cross-platform, but the sealed ensemble `weights` are already rounded to
  `HASH_STABLE_SCALE = 1e12` (12 dp) by train's `hash_stable` before the vintage hash, which neutralises
  the sub-ULP (`~1e-15`) cross-platform drift. Documented at the call site + covered by a value test.
- **Reported metrics** (`golden_result.json`) are `round10`-quantised (10 dp) as before, so the golden
  stays byte-identical across targets.

## 5. Calibration schema change → `VINTAGE_FORMAT_VERSION` bump

`SlippageCalibration` is content-addressed **and** rides the sealed vintage (a hashed field). QE-440
changes its hashed shape:

| Field | Before (QE-431) | After (QE-440) |
|---|---|---|
| `half_spread` | Decimal (1e-4) | Decimal (1e-4) — unchanged |
| `impact_per_notional` | Decimal (2e-9, per-$) | **removed** |
| `reference_mark` | Decimal (50000) | **removed** |
| `impact_coeff` | — | Decimal — participation impact fraction at u=1 (default `0.01`) |
| `impact_exponent` | — | Decimal — β (default `0.5`) |

New hashed fields ⇒ **`VINTAGE_FORMAT_VERSION` 5 → 6**. The default seed values (`impact_coeff = 0.01`,
`β = 0.5`) are a **pre-fit placeholder** (as QE-431's seed was) — an economically-grounded √-law
coefficient (~1 % impact at 100 % of one hour's ADV); live power-law fitting is follow-up. The fit
`fit_slippage_calibration` is generalised: it regresses signed impact on `u^β` (with `u = notional/ADV$`,
β at the default prior) via the same binned zero-intercept LS, keeping β as a documented prior (robustly
fitting an exponent needs far more data than we bin here).

## 6. Expected golden movement + direction sanity-check

Golden movement is **expected** — the cost *shape* changes, so backtest metrics change. Regenerated via
the **real** `regenerate_fixtures` path (never hand-edited). Reported below in the PR:

- vintage `content_hash` before → after (the calibration seed + format-version bump both move it),
- the `golden_result.json` field diff (only cost-shape-impacted metrics: equity/drawdown/CAGR/Sharpe/…
  + the new calibration fields — no unrelated movement),
- **Direction check:** with coefficient and ADV held fixed, the new impact fraction is **concave**
  (`2^β < 2` on a size-doubling) whereas the old was **linear** (`×2`); so large orders see a
  *diminishing* marginal impact rate vs the old convex-total model — the panel's expected direction. On
  the fixture (tiny participation) net equity moves modestly lower (√ dominates a near-zero linear term
  there), consistent with the shape.

## 7. Blast radius

- `crates/risk/src/slippage.rs` — calibration fields, `impact_fraction`, `notional_cost(notional, adv)`,
  generalised fit, `powd`.
- `crates/wfo/src/friction.rs` — `SlippageModel{half_spread, impact_coeff, impact_exponent}`,
  `cost(notional, qty, adv)`, `Fill.adv`.
- `crates/wfo/src/backtest.rs` — `Bar.volume`, rolling ADV, `apply_fill` threads ADV.
- `crates/ensemble/src/capacity.rs` — participation `CapacityModel`, new `capacity` closed form,
  `StrategyProfile.adv_notional`.
- `crates/cli/src/jobs/{features,backtest,train}.rs` — thread volume/ADV.
- `crates/vintage/src/lib.rs` — `VINTAGE_FORMAT_VERSION` 5 → 6.
- Parity/oracle tests: `slippage_parity.rs`, `money_model_parity.rs`,
  `qe432_slow_reference_oracle.rs`; golden `backtest_job.rs`.
- Workspace `Cargo.toml` — `rust_decimal` `maths` feature.

Composes with QE-431 (shared calibration), QE-438 (deployed-weight scoring reads the same
`strategy_capacities`), QE-428 (reporting impact matches selection).
