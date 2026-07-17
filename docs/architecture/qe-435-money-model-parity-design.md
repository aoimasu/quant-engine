# QE-435 — Train-backtest ↔ live execution/money-model parity: evidence & design

`Phase: Review R2 (P2 — panel #6, unanimous)` · `Area: wfo / hedger / edge` · `Depends on: QE-120, QE-219, QE-431, QE-432`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-435`](../reviews/2026-07-16-maxdama-panel-review.md)
· Spec ref: maxdama §5.5 ("backtesting on recorded data should produce the same results as the live run").

## 1. Why (the panel's concern)

Archive selection rides on the **net-of-friction `log_growth`** produced by the `qe-wfo` backtest. So the
*money* model — how a `(side, qty, mark, spread)` fill turns into cash — not just the *decision*, must match
the live path. If the two diverge, a genome tuned to the wfo linear-slippage ledger can be **selected and
sized on fills it never gets live** (the same "optimise-X / deploy-Y" defect class as the
equal-weight-scored / capacity-weight-deployed gap, QE-438).

Today only `Genome::decide` / `PositionState::advance` are shared across train and live (via `qe-signal`).
Everything about *money* — sizing, fills, cost — is implemented on each side independently. This ticket
closes the parity gap by **proving** (not assuming) that the optimised object equals the deployed one, per
the debate resolution ("fix by proving the optimised object equals the deployed one, not by betting the
divergence is small").

## 2. The two money models, side by side (current-state evidence)

### 2a. Training money model — `qe-wfo` (`friction.rs` + `backtest.rs`)

The wfo backtest (`backtest_with_trades`, `crates/wfo/src/backtest.rs`) is the **only** place the selection
fitness's money is computed. Per fill it calls `apply_fill` (`backtest.rs:291`):

```
notional_abs = |qty · price|
fee          = fees.fee(notional_abs, Taker) · cost_multiplier          # = notional·taker_rate·mult
slip         = slippage.cost(notional_abs, qty.abs()) · cost_multiplier  # see below
cash        -= signed_qty · price          # signed notional moves cash
cash        -= fee + slip                  # costs reduce cash → returns are net-of-cost
pos_qty     += signed_qty
```

Sizing (`backtest.rs:181`): an `Enter` from flat sizes `notional = size_frac · equity_prev`,
`qty = notional / price`, `side = Buy` (Long) / `Sell` (Short). A `Close` trades the whole `|pos_qty|`.

Slippage (`friction.rs:97`, `SlippageModel::cost`):

```
slippage_cost(notional_abs, qty_abs) = notional_abs · (half_spread + impact · qty_abs)
```

where (QE-431, `friction.rs:86` `SlippageModel::from_calibration`) `half_spread = cal.half_spread` and
`impact = cal.friction_impact_per_contract() = impact_per_notional · reference_mark` — i.e. the wfo default
friction model is **derived from the one shared `SlippageCalibration`**, not authored. Fees default to
Binance USDT-M VIP0 (taker `0.05%`, maker `0.02%`). Funding is `−signed_qty · mark · rate`.

### 2b. Live money model — `qe-edge` (`plan_delta` + `VenueSimulator`) + `qe-hedger` (`EvaluatorSession`)

- **`plan_delta`** (`crates/edge/src/edge.rs:131`) is the sizing/fill bridge. Given an **absolute** target
  notional, the signed kept position, and the mark, it emits an `OrderIntent { side, qty }`:

  ```
  target_qty = target.notional / mark
  delta      = target_qty − current_qty
  side       = Sell if delta < 0 else Buy
  qty        = |delta|
  ```

- **`VenueSimulator::submit`** (`crates/edge/src/edge.rs:297`) fills the intent **immediately at exactly
  `fill_price`**, moves its signed position by `±qty`, and emits the `Fill` user-data event. It applies **no
  fee, no slippage, no cost of any kind** — the fill price is the passed price verbatim.

- **`EvaluatorSession`** (`crates/hedger/src/evaluator.rs`) drives `Genome::decide` / `advance` live and emits
  **decisions only** — it computes no PnL and deducts no cost. It *carries* the vintage's
  `SlippageCalibration` (`evaluator.rs:230`) but never prices a fill with it.

- A workspace-wide grep (`fee|slippage|impact|taker|maker|cost|pnl|ledger`) across `crates/edge/src` and
  `crates/hedger/src` finds **no cost/fee/slippage arithmetic on any fill** — only carried
  `SlippageCalibration::default()` values threaded into vintages/sessions.

### 2c. Alignment / divergence table

| Aspect | wfo `friction`/`backtest` | live `plan_delta`/`VenueSimulator` | Agree? |
|---|---|---|---|
| Order side for a move to target | `Buy` long / `Sell` short (or close) | `Buy` if `delta>0` else `Sell` | **Yes** (identical sign rule) |
| Order qty for a move | `notional/price` (enter) / `|pos|` (close) | `|target/mark − current|` | **Yes** — both are `|Δposition|` in contracts; `notional/price` ≡ `target/mark` |
| Traded notional | `qty · price` | `qty · mark` | **Yes** (same `qty·mark`) |
| Resulting signed position | `pos_qty += signed_qty` | `signed_qty += ±qty` | **Yes** (identical delta) |
| **Fee on the fill** | `notional · taker_rate` | **none** | **No** — sim models zero |
| **Slippage on the fill** | `notional · (half_spread + impact·qty)` | **none** | **No** — sim models zero |
| Cost source of truth | shared `SlippageCalibration` (QE-431) | carried but **not applied** | n/a |

## 3. Finding: do the two money models agree for identical `(side, qty, mark, spread)`?

**They agree exactly on the *fill* (side, qty, traded notional, resulting signed position). They do not
"agree" on *cost* — because the live `VenueSimulator` models no cost at all.** The live sim/`plan_delta`
path is a **fill + position-keeping plumbing model**, not a money/PnL model: it deliberately fills at the
raw mark. Cost accounting on the selection path exists in exactly **one** place — the wfo `friction` model —
and that model is (QE-431) derived from the single content-addressed `SlippageCalibration`.

So the honest statement of the parity result:

1. **Fill-geometry parity holds exactly.** For identical `(side, qty, mark)` the wfo `friction::Position`
   and the live `VenueSimulator` reach the **same signed position** and book the **same traded notional**.
   The fills are *not* two divergent implementations of sizing — they are the same arithmetic.
2. **The cost model is single-sourced, not duplicated-and-divergent.** The wfo friction *slippage* cost for
   a fill marked at the calibration's `reference_mark` reduces **exactly** to the shared
   `SlippageCalibration::notional_cost(notional)` — the same single source of truth `capacity` uses
   (QE-431 `slippage_parity`). There is no *second, divergent* live cost number to disagree with.
3. **The one real gap** the panel named is that the live `VenueSimulator` prices fills at **zero cost**. That
   is correct *for a paper/plumbing simulator* (a real venue charges its own fees; the sim is a test double
   for the order loop), but it means a naïve "`VenueSimulator` fill cost == wfo friction cost" assertion
   would compare `wfo_cost` against `0`. This is documented and asserted explicitly (see §4, test 3) rather
   than papered over.

This is **not** a silent divergence to reconcile by editing coefficients: both sides that *do* price money
(wfo friction, ensemble capacity) already read the one QE-431 calibration; the sim simply prices nothing.
Reconciling by "adding a cost model to `VenueSimulator`" is explicitly a larger, higher-risk refactor on the
QE-268 panic-free order-emission path and is **out of scope / lower-risk-rejected** here — the ticket
prefers the parity-test approach, which pins the invariant and surfaces the finding without moving goldens.

### 3a. A precise nuance worth recording (per-contract vs per-notional impact)

wfo friction's size term is `impact_per_contract · qty` with `impact_per_contract = impact_per_notional ·
reference_mark` **fixed at construction**; the canonical calibration term is `impact_per_notional ·
notional = impact_per_notional · qty · price`. These coincide **iff `price == reference_mark`**. Away from
the reference mark the wfo per-contract form and the per-notional canonical form differ — this is a
pre-existing **QE-431 modelling choice** (the two legacy literals "were only mutually consistent at this
mark", `slippage.rs:38`), not a QE-435 regression, and QE-431's own `slippage_parity` test likewise asserts
agreement **at `reference_mark`**. The QE-435 parity test therefore asserts the cost identity at the shared
`reference_mark`, matching the established QE-431 common ground, and records this nuance so a future
per-notional/√-participation change (QE-440) knows where the two forms are pinned equal.

## 4. Design — the parity test (preferred, lower-risk, golden-safe)

Approach chosen: **oracle/parity test, no refactor** (spec's preferred, lower-risk option; mirrors the
QE-432 slow-reference-oracle style — a test, not a production change, so it moves **no** goldens / no
`content_hash`).

**Home:** `crates/runtime/tests/money_model_parity.rs`. Rationale — the assertion must link both `qe-wfo`
(`SlippageModel`, `friction`) and `qe-edge` (`plan_delta`, `VenueSimulator`) plus `qe-risk`
(`SlippageCalibration`). `qe-runtime` already links `qe-edge`/`qe-hedger`/`qe-risk` as production deps and is
the live composition facade; adding `qe-wfo` as a **dev-dependency** is firewall-clean because (a) the
`qe-architecture` firewall parser **excludes every dev-dependency form** (`architecture/src/lib.rs`
`classify_section`), and (b) `qe-runtime` is not a guarded *upstream* in `firewall_rules()` — the forbidden
direction is `wfo → runtime`, whereas `runtime → wfo` (live reading a training output) is the **allowed**
downstream direction. The `firewall` test (`crates/architecture/tests/firewall.rs`) stays green.

**Assertions (TDD — written to fail first against a wrong mirror):**

1. **Fill-geometry parity.** For a matrix of `(current signed qty, target notional, mark)`, assert
   `plan_delta` and a wfo-side sizing mirror (`notional/price`, signed) produce the **same** `(side, qty)`;
   then drive the wfo `friction::Position::apply` and the live `VenueSimulator::submit` with that
   `(side, qty, mark)` and assert **identical resulting signed positions** and **identical traded notional**.
2. **Cost single-source parity.** For each fill, assert the wfo `SlippageModel::from_calibration(cal).cost`
   equals the shared `SlippageCalibration::notional_cost` at `mark == cal.reference_mark` (the QE-431 common
   ground), across the default calibration and an off-default (ETH-scale) calibration.
3. **The finding, asserted (non-vacuous).** Assert the `VenueSimulator` fill carries **zero** modeled cost
   (its fill price is the input mark verbatim; cash moves only by the signed notional), and that the wfo
   friction cost it omits equals exactly `taker_fee + shared_calibration_slippage`. This pins the exact,
   quantified gap so a regression that silently started charging (or a wfo change that stopped matching the
   calibration) is caught.
4. **Non-vacuity guard.** A mismatched calibration (3× impact on one side) makes the cost identity fail —
   proving assertion 2 is a real constraint, not a tautology (mirrors QE-432's mutation guard / QE-431's
   `parity_is_non_vacuous`).

## 5. Test plan / green gate

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings` **and** `--all-features --locked -- -D warnings`
- `cargo test --workspace --locked` (includes the new `money_model_parity` test)
- `cargo test -p qe-architecture --test firewall` (must stay green — dev-dep is firewall-excluded)
- `cargo deny check`

## 6. Risks

- **Golden movement:** none expected — this is a test-only addition (new dev-dep + new integration test), no
  production code path changes, so no vintage/`content_hash`/determinism artefact can move. If any golden
  moves, that is itself a bug to investigate (it must not).
- **Firewall:** adding `qe-wfo` as a **dev**-dependency to `qe-runtime` is excluded from the firewall graph;
  verified against `architecture/src/lib.rs` parser + `firewall.rs`. A production dep would breach it and is
  not used.
- **Panic-freedom (QE-268):** the new code is test-only (`#![allow(clippy::unwrap_used)]`, the sanctioned
  integration-test pattern); it touches no live order-emission path, so the deny lints there are unaffected.
</content>
</invoke>
