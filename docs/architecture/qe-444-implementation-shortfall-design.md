# QE-444 — Decision-to-fill implementation-shortfall (alpha-loss) term in friction

`Phase: Review R2 (P2 — panel #15)` · `Area: wfo friction / risk calibration` · `Depends on: QE-109, QE-431, QE-435, QE-440`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-444`](../reviews/2026-07-16-maxdama-panel-review.md)
· Spec ref: maxdama §7.3 "Alpha Loss" (profit absorbed between signal and fill).

## 1. The defect

The backtest (`crates/wfo/src/backtest.rs`) makes its decision at **bar close** (`Genome::decide` on bar `i`'s
features) and fills the whole delta at the **next bar's open** (bar `i+1`'s `price`), with only the **symmetric**
half-spread + participation-impact slippage (`SlippageModel::cost`, QE-431/QE-440) charged on the fill.

If the signal's own edge **leaks into the close→open gap** — i.e. some of the move the signal predicts happens
*before* the next-bar-open fill — the trade fills at a price already moved **against the trade**, and the
backtest never charges for it. Net returns are therefore **optimistically biased**: the strategy is scored as if
it captured edge it actually surrendered to the gap. This is maxdama §7.3 "Alpha Loss": profit absorbed between
signal and fill. It is the one §7.3 piece that bites at 1h cadence **without** any TWAP/VWAP execution machinery
(there is exactly one fill per decision, at next-bar-open), and it is the natural home for the QE-435
execution-parity concern on the money model.

## 2. Why the term is DIRECTIONAL, not symmetric

The existing `half_spread` is a **side-blind** cost: `notional · half_spread`, identical whether you buy or sell —
it models crossing a symmetric bid/ask spread. The alpha-loss is different in kind: it is a **directional drift**
whose *sign follows the trade side*, because the drift is the **signal's own edge** front-running the fill.

- A **buy** (opening/adding a long, or a bullish signal) is decided because price is expected to **rise**; part of
  that rise happens in the gap, so the fill lands at a price **above** the decision price → the buy pays the
  **up-drift**.
- A **sell** (opening/adding a short, or a bearish signal) is decided because price is expected to **fall**; part
  of that fall happens in the gap, so the fill lands **below** the decision price → the sell pays the
  **down-drift**.

So the per-unit price drift is an **odd function of side**: `+γ` for a buy, `−γ` for a sell
(`directional_drift(side)`), whereas `half_spread` is an **even** function of side (side-blind). That sign
asymmetry is exactly what distinguishes alpha-loss from the half-spread, and why it cannot simply be folded into
`half_spread`.

Note on magnitude: because the drift is **signal-aligned** in both directions (it is adverse to *whichever* way
the trade points), the adverse-cost *magnitude* is symmetric — `|drift| · notional = γ · notional` for both a buy
and a sell. Directionality lives in the **sign** of the drift (which side is hurt by which way price moved), not
in the magnitude. Both a long entry and a short entry have their net return **reduced** by the term. This is the
honest model: implementation shortfall is adverse in the trade's own direction.

Consequences for where the coefficient may live:

- The coefficient rides the **shared** content-addressed `SlippageCalibration` (QE-431) so friction reads it from
  the one source of truth (measurement-deferred; see §5).
- It is charged **only on the friction side** (`SlippageModel` / the wfo ledger), **not** in the symmetric
  `SlippageCalibration::cost_fraction` / `notional_cost` that `capacity.rs` also consumes. Capacity models
  *sizing headroom*, not per-fill decision-to-fill drift; folding alpha-loss into the shared symmetric cost would
  leak a directional execution term into capacity and break the QE-431 friction↔capacity coefficient parity.
  Keeping alpha-loss a **separate** directional method preserves that parity exactly.

## 3. Default-0, measurement-deferred stance (golden-safe)

Per the QE-435 finding, the **realised** close→open directional drift can only be **measured** from live/shadow
execution data — which **does not exist yet**. We therefore refuse to invent a directional cost number. The
coefficient defaults to **`0`** (`DEFAULT_ALPHA_LOSS = 0`): the term is fully present, wired into the ledger, and
tested, but **inert** until calibrated from real data. This is:

- **Honest** — no fabricated directional cost, consistent with QE-435 (no live drift data) and QE-431
  (calibration-driven, not magic constants).
- **Golden-safe** — see §4: at the default, the term contributes exactly `0` to every ledger, and the calibration
  serialises **byte-identically** to before, so **no golden and no `content_hash` moves**.

The alternative (a documented small non-zero default) was rejected: it would move goldens and, worse, bake a
**made-up** directional cost into every selection — precisely the fabrication QE-435/QE-431 warn against.

## 4. Golden / content_hash safety — the `skip_serializing_if` design

Adding a hashed field to `SlippageCalibration` would normally move its `content_hash` (and every downstream
golden/vintage) **even at value 0**, because the field would appear in the canonical JSON. We avoid that:

```rust
#[serde(default, skip_serializing_if = "Decimal::is_zero")]
pub alpha_loss: Decimal,   // default 0
```

- **`skip_serializing_if = "Decimal::is_zero"`** — at the default (`0`) the `alpha_loss` key is **omitted** from
  the serialized JSON, so a default calibration serialises to the **exact same bytes** as before the field
  existed. `content_hash` (SHA-256 over that JSON) is therefore **unmoved**, and no vintage/golden that embeds the
  calibration moves. **No `VINTAGE_FORMAT_VERSION` bump is required.**
- **`serde(default)`** — old vintages/JSON that predate the field deserialise cleanly (`alpha_loss = 0`) and
  re-serialise byte-identically (round-trip stable, the content-hash invariant).
- Only a **non-zero, calibrated** `alpha_loss` makes the key appear and the hash move — at which point the golden
  is regenerated **via real code** and the `content_hash` change tracked, exactly as QE-431 prescribes.

This achieves the ticket's preferred outcome: **default-0 leaves the hash unmoved**, with a clean field-only diff
only when a real coefficient is fitted.

## 5. Live/shadow measurement plan (the deferred calibration)

The measurement primitive ships now (test-covered), inert on the historical path:

- `realized_alpha_loss(side, decision_price, fill_price) -> Decimal` — the realised per-fill implementation
  shortfall as a **signed fraction of the decision price, charged in the trade direction**:
  - Buy: `(fill − decision) / decision` (positive = adverse: price drifted up before the buy).
  - Sell: `(decision − fill) / decision` (positive = adverse: price drifted down before the sell).
- `AlphaLossAccumulator` — accumulates realised shortfalls over live/shadow fills and yields their **mean**, the
  coefficient fed back into `SlippageCalibration::with_alpha_loss`.

**Plan.** On the live/shadow execution path (QE-435), for each fill record `(side, decision_price = bar-close
mark at the decision bar, fill_price = achieved next-bar-open fill)`. Feed each into the accumulator; the mean
signed shortfall — clamped at `0` if favourable (we do not credit a *negative* execution cost into the selection
fitness) — is the fitted `alpha_loss`, emitted on the **same content-addressed calibration vintage** as
`half_spread`/`impact_coeff`/β (QE-431). Friction then charges the fitted, real directional cost; the golden moves
once, via real code, tracked. Until that data exists, `alpha_loss` stays `0` and the term is inert.

## 6. Implementation surface (scoped)

- `crates/risk/src/slippage.rs` — `alpha_loss` field (+ `DEFAULT_ALPHA_LOSS`, `with_alpha_loss`,
  `alpha_loss_cost`, `directional_drift`) on `SlippageCalibration`; `realized_alpha_loss` +
  `AlphaLossAccumulator` measurement hook.
- `crates/wfo/src/friction.rs` — `alpha_loss` on `SlippageModel` (derived from the calibration via
  `from_calibration`); `alpha_loss_cost` / `directional_drift`; charged into the slippage bucket in `simulate`.
- `crates/wfo/src/backtest.rs` — charge the alpha-loss on each fill in `apply_fill` (entries and closes both fill
  at next-bar-open, so both incur decision-to-fill shortfall).
- `crates/wfo/tests/qe432_slow_reference_oracle.rs` — mirror the term in the independent reference `apply` and
  exercise a non-zero `alpha_loss` in the randomised friction, so the QE-432 oracle keeps proving parity.

**Invariants.** All money exact `rust_decimal` (`directional_drift` and the cost are pure Decimal, deterministic
across platforms). At `alpha_loss = 0`: byte-identical ledger, byte-identical calibration serialization, unmoved
`content_hash`. The QE-431 friction↔capacity parity and QE-435 money-model parity are untouched (the term is
separate from the symmetric `cost`). No new cross-crate dependency edge (firewall-safe): the coefficient lives in
`qe-risk`, which `qe-wfo` already depends on.

## 7. Acceptance-criteria mapping

- *Directional slippage term (not symmetric half-spread):* `directional_drift(Buy) = +γ`,
  `directional_drift(Sell) = −γ` (odd in side) vs side-blind `half_spread` — unit-tested.
- *A non-zero coefficient reduces net return for a directional entry:* backtest test — a long-only genome on an
  uptrend and a short-only genome on a downtrend both lose strictly more net P&L with `alpha_loss > 0`.
- *Coefficient 0 = byte-identical to current:* backtest byte-equality test + calibration serialization/hash test.
- *The term flows from the calibration:* `SlippageModel::from_calibration` copies `alpha_loss`; friction charges
  the derived value.
- *Deterministic exact Decimal:* pure-Decimal arithmetic throughout; the QE-432 oracle reproduces it.
</content>
</invoke>
