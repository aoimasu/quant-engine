# QE-431 — Calibrate slippage `half_spread` + `impact` from venue data, shared by friction & capacity

`Phase: Review R2 (P1 — net-of-cost truth)` · `Area: wfo friction / ensemble capacity / vintage lineage`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-431`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [`docs/backlog.md`](../backlog.md) → Review R2.a.

## 1. Problem / current-state evidence

`SlippageModel { half_spread, impact }` (`crates/wfo/src/friction.rs:60`) and the impact coefficient in
`crates/ensemble/src/capacity.rs` are **hardcoded guesses living in two places and two unit systems**:

| Site | half-spread | size-impact | unit |
|---|---|---|---|
| `friction.rs` `SlippageModel::default` | `1e-4` (1bp) | `impact = 1e-4` | **per contract** (`impact · qty`) |
| `capacity.rs` `CapacityModel::default` | `1e-4` (1bp) | `impact_coeff = 2e-9` | **per $ of notional** (`impact_coeff · qty_notional`) |

Evidence they can silently drift: the two size-impact literals are only mutually consistent at one mark
price — `1e-4 (per contract) = 2e-9 (per $) × 50 000 ($/contract)`. Nothing enforces that relation; either
side can be edited independently. Both numbers are **selection-critical**: they price every trade in the
net-of-cost geometric fitness the train search selects on (`friction` via
`BacktestConfig::default().friction`, `capacity` via `CapacityModel::with_defaults()` in
`cli/src/jobs/train.rs`), and the Deflated Sharpe cannot remove a systematic per-trade cost bias (PSR is
absolute vs a noise ceiling, not vs a cost error).

The vintage artefact (`crates/vintage/src/lib.rs`) already carries a per-vintage `CalibrationProfile`
(`qe_risk`, QE-116) inside its content-hashed `VintageContent`, tagged by a resolvable `Lineage` (QE-006)
— the exact lineage slot a shared cost calibration should ride in.

## 2. Design decisions

### D1 — One content-addressed source of truth in `qe-risk`
Add `SlippageCalibration { half_spread, impact_per_notional, reference_mark }` (all `Decimal`, exact money)
to **`qe-risk`** (`crates/risk/src/slippage.rs`), the shared upstream crate that already hosts
`CalibrationProfile`. `impact_per_notional` is the **canonical** size-impact unit (per $ of notional, i.e.
capacity's unit — asset-portable); `reference_mark` is the mark price that pins friction's per-contract
coefficient. `content_hash()` = lowercase-hex SHA-256 over canonical JSON (same pattern as `Lineage::id` /
`Vintage::content_hash`). Decimals are `round_dp(_).normalize()`-quantized so the value is
serialize-idempotent (excess-precision division results would otherwise break byte-reproducibility of the
hash — the same hazard `qe_risk::quantize_calibration` guards, QE-416).

### D2 — Both sides derive, never author, their coefficients
- `friction.rs`: `SlippageModel::from_calibration(&cal)` sets `half_spread = cal.half_spread`,
  `impact = cal.impact_per_notional × cal.reference_mark` (per-$ → per-contract). `SlippageModel::default()`
  becomes `from_calibration(&SlippageCalibration::default())`. **The per-contract formula and `cost(notional,
  qty)` signature are unchanged**, and the default numbers are byte-identical to today — so friction's
  net-of-cost behaviour (and its goldens) do not move; only the source of the constants moves.
- `capacity.rs`: `CapacityModel::from_calibration(&cal)` sets `half_spread`/`impact_coeff` from `cal`
  (`Decimal → f64` via `ToPrimitive::to_f64`, capacity is an f64 model). `CapacityModel::default()` becomes
  `from_calibration(&SlippageCalibration::default())`. The standalone `DEFAULT_HALF_SPREAD` /
  `DEFAULT_IMPACT_COEFF` **literals are removed** — the numbers now live only in `SlippageCalibration`.

This honours the search⟂portfolio firewall (QE-001/QE-132): `qe-wfo` and `qe-ensemble` each gain an edge to
the shared upstream `qe-risk` (which reaches only `qe-domain`/`qe-error` — no forbidden crate), **not** a
`qe-wfo → qe-ensemble` edge. Each side keeps its own unit conversion (the sanctioned duplicated-CONFIG /
sealed-DATA pattern), and a **coefficient-parity test** proves they can never drift.

### D3 — The estimator (maxdama §7.7)
`fit_slippage_calibration(trades, quotes, bins)` in `qe-risk`:
- **half_spread** = median of `(ask − bid) / (2 · mid)` over the quote samples (`mid = (ask+bid)/2`).
- **impact_per_notional** = binned zero-intercept least-squares slope of *signed* fractional impact vs
  notional. For each trade `signed_impact = dir · (price − pre_mid)/pre_mid`, `notional = qty · price`,
  `dir = +1` (Buy/aggressor-lifts) / `−1` (Sell). Trades are sorted by notional, split into `bins`
  equal-count buckets, and the slope through the per-bucket means is `Σ(x̄·ȳ)/Σ(x̄²)`. The perp trade feed
  **carries aggressor side**, so `dir` is taken directly — **the Lee-Ready classifier is skipped**.
- **reference_mark** = median trade price (a representative mark from the same snapshot).
All arithmetic is exact `Decimal` (no float non-determinism); results are quantized+normalized. Re-running
on the same pinned input snapshot yields **byte-identical** coefficients (proved by test).

### D4 — Ride the vintage lineage
Add `slippage: SlippageCalibration` to `VintageContent` right after `calibration`, and bump
`VINTAGE_FORMAT_VERSION` `3 → 4`. The field enters the content hash, so every vintage id changes — expected
and called out by the AC. Goldens are regenerated **via real code** (the `#[ignore]`d
`regenerate_fixtures` test) and the before→after hash is reported; the server fixture is byte-identical to
the CLI one and copied from it.

**Out of scope (per ticket):** the concave square-root-in-participation impact *shape* change (QE-440);
wiring the live `VenueSimulator` to consume the fitted calibration (the train/live default still seals the
canonical `SlippageCalibration::default()` seed — the estimator exists and is proven reproducible, but its
fitted output is not yet threaded into the live selection default); rolling-ADV ingest (QE-440).

## 3. Test plan (prove-it, one per AC)

1. **Parity (AC1)** — cross-crate test (`qe-cli`, which links both `qe-wfo` + `qe-ensemble`): for identical
   `(side, qty, mark, spread)` at the calibration's `reference_mark`, `SlippageModel::from_calibration` and
   `CapacityModel::from_calibration` charge the identical slippage. Non-vacuous: a deliberately mismatched
   calibration makes them disagree.
2. **Content-addressed + reproducible (AC2)** — `qe-risk`: `fit_slippage_calibration` on a pinned fixture
   snapshot reproduces byte-identical coefficients and content hash across runs; `content_hash` is
   serialize-idempotent (serialize→parse→serialize stable).
3. **No magic literal on the selection path (AC3)** — `qe-wfo` + `qe-ensemble`: `SlippageModel::default()`
   ≡ `from_calibration(default)` and `CapacityModel::default()` ≡ `from_calibration(default)`, so the only
   place slippage/impact numbers are authored is `SlippageCalibration::default()`.
4. **Goldens via real code (AC4)** — `regenerate_fixtures` rewrites `sample_vintage.json` +
   `golden_result.json`; `backtest_over_fixture_store_matches_golden` stays green; determinism/firewall
   suites green. Vintage `content_hash` before→after recorded in the PR.

## 4. Risk / rollback
- **Blast radius**: a new required `VintageContent` field touches 14 construction sites (mostly test
  fixtures) + 2 golden JSON fixtures + the format-version bump. Mechanical; each non-selection site seals
  `SlippageCalibration::default()`.
- **Behaviour risk**: friction/capacity *default* numbers are unchanged by construction, so the only
  intended output move is the vintage hash (format-version + new field). The estimator is new code not yet
  on the live default path, so it cannot regress selection.
- **Rollback**: revert the branch; the format-version bump means old vintages are rejected on load (fail
  closed), so there is no silent-mismatch window.
