# QE-433 — Portfolio-level fractional (≤½) empirical-Kelly sizing pass

**Spec of record:** `docs/reviews/2026-07-16-maxdama-panel-review.md#qe-433` · Backlog: Review R2.a.
**Depends on:** QE-113 (`log_growth`), QE-126/128 (capacity weights), QE-215 (pretrade cap),
QE-219 (vintage→runtime load), QE-431 (`SlippageCalibration`, now on `main`).

## Problem (current-state evidence)

The pipeline does maxdama §6.3 **step 1** (mask → capacity-capped weights) but never **step 2**: no
growth-optimal `f*` is solved on the **combined** net PnL.

- `crates/wfo/src/fitness.rs::log_growth` — `mean_i ln(1+r_i)`, ruin-absorbing. The exact objective a
  Kelly solve maximises; reused, not re-derived.
- `crates/cli/src/jobs/train.rs:410-503` — the seal path. `weights = capacity_capped_weights(...)` then
  `in_sample_returns = combine(&chromosomes, &weights, train_bars, &train_cfg)` (line 421) is **exactly**
  the realised combined **net-of-cost** series after the mask + capacity weights are fixed. The train
  backtest prices cost via `BacktestConfig::default().friction`, which derives from
  `SlippageCalibration::default()` (QE-431), so `in_sample_returns` is already net of the **calibrated**
  cost model — the QE-431 dependency is satisfied by solving `f*` on this series.
- `crates/hedger/src/live_netter.rs` — `PositionNetter::net_positions` sums per-strategy legs into a
  `NetTarget`. `NetTarget.net` is a **fraction of allowed capital**, and downstream
  `HedgePlanner::plan` sets `notional = net.net × equity`, so **`net.net` is literally the net leverage**
  (`|notional|/equity = |net.net|`). The pretrade cap (`crates/hedger/src/pretrade.rs`, `MaxLeverage`)
  clamps `|notional| ≤ max_leverage × equity`, i.e. `|net.net| ≤ max_leverage`. This equivalence is what
  lets the advisory sizer scale in the same leverage units the cap is expressed in.
- `crates/vintage/src/lib.rs` — `VintageContent` rides `CalibrationProfile` (QE-116) and
  `SlippageCalibration` (QE-431) as content-addressed sidecars; `VINTAGE_FORMAT_VERSION = 4`.

Kelly is **non-additive under correlation**: summing standalone per-strategy Kellys over-allocates the
shared BTC-beta directional bet. A fractional (≤½) empirical Kelly on the realised **joint** path
estimates **no covariance** (correlation-robust by construction) and typically **cuts** size on fat left
tails (§6.4).

## Design

Three seams, respecting the search ⟂ portfolio ⟂ live firewall (QE-132):

1. **Solver — `crates/wfo/src/kelly.rs` (new).** Pure `f64`, reuses `log_growth`.
   - `empirical_kelly(returns) -> f64`: `argmax_{f≥0} mean ln(1 + f·r)` by golden-section search on
     `[0, hi]`, where `hi = (1/|min r|)·(1−ε)` stays strictly inside the ruin boundary (or a finite
     `MAX_LEVERAGE_SEARCH` when the series never loses). The objective is concave in `f` ⇒ unimodal ⇒
     golden-section converges deterministically. A non-positive-drift series optimises at `f→0`, guarded
     to return exactly `0` (Kelly of a non-edge is no bet).
   - `fractional_kelly(returns, kappa) -> f64 = kappa.clamp(0.3, 0.5) · empirical_kelly(returns)` — the
     `κ ∈ [0.3, 0.5]` fractional multiplier (§6.5 half-Kelly robustness). `DEFAULT_KELLY_FRACTION = 0.5`.
   - Deterministic (no RNG, fixed iteration count), so a fixed input yields byte-identical output.

2. **Artefact — `crates/risk/src/sizer.rs` (new).** `PortfolioSizer { multiplier: Decimal }`, the
   content-addressed sidecar (mirrors `SlippageCalibration`): `content_hash` over canonical JSON,
   coefficient quantised+normalised (`SIZER_SCALE = 12`) so a fit reproduces byte-identically. `multiplier`
   is the fractional-Kelly leverage factor `κ·f*` (≥ 0). `Default` is `1.0` — **neutral** (deploy the
   naive summed size), the pre-QE-433 behaviour for vintages sealed without a Kelly pass.
   `from_kelly(f64)` converts + quantises (non-finite ⇒ `0`, fail-safe).

3. **Vintage — `crates/vintage/src/lib.rs`.** Add `sizer: PortfolioSizer` to `VintageContent` (hashed
   content), **bump `VINTAGE_FORMAT_VERSION` 4 → 5** (new hashed field), doc the version row. Riding it in
   the hashed content ties the chosen size into the vintage's reproducible lineage, exactly like
   `slippage`. Per-vintage & reproducible by construction.

4. **Seal — `crates/cli/src/jobs/train.rs`.** `sizer =
   PortfolioSizer::from_kelly(fractional_kelly(&in_sample_returns, DEFAULT_KELLY_FRACTION))`; add to the
   sealed `VintageContent`. Solved on the calibrated-cost combined series (QE-431).

5. **Consume — `crates/hedger/src/live_netter.rs`.** `PositionNetter::net_positions_sized(positions,
   weights, sizes, sizer, leverage_cap: Option<Decimal>)`: net as before, scale the whole book
   (`net`/`long`/`short`) by `sizer.multiplier()`, then **clamp `|net| ≤ leverage_cap`** so the advisory
   size **never exceeds** the pretrade cap. The hard cap in `pretrade.rs` is unchanged — it remains the
   backstop. Panic-free (honours the QE-268 `deny(unwrap/expect/panic)` on this order-emission path):
   only `Decimal` arithmetic, the one division guarded by `mag > cap ≥ 0 ⇒ mag > 0`.

**Why the netter and not the planner/pretrade:** the ticket names `live_netter` as the consumer, and
`net.net` is already in leverage units, so the sizer applies and clamps in one place without touching the
hard-cap governor.

## Test plan (TDD)

- **wfo `kelly.rs`:**
  - `empirical_kelly_cuts_on_fat_left_tail` — a positive-mean series with a fat left tail yields a smaller
    `f*` than a thin-tail series of the same mean; `fractional_kelly` (×0.5) is smaller still. **(AC1 root)**
  - `positively_correlated_downweighted_vs_summed_standalone` — for positively-correlated A,B the
    portfolio Kelly on the average book is **strictly below** `kelly(A)+kelly(B)`, while for independent
    A',B' with the same marginals it is ≈ the sum (isolates correlation, not averaging). **(AC2)**
  - `no_edge_is_zero`, `respects_ruin_feasibility` (solved `f` keeps `log_growth` finite),
    `fractional_clamps_kappa`, `deterministic_bit_for_bit`.
- **risk `sizer.rs`:** `content_hash` stable/idempotent/field-sensitive; `Default` multiplier `1`;
  `from_kelly` quantises; negative clamps to `0`.
- **vintage:** a different `sizer` changes `content_hash`; round-trips; `VINTAGE_FORMAT_VERSION == 5`.
- **hedger `live_netter.rs`:**
  - `sizer_cuts_netted_leverage` — multiplier `< 1` (fat-left-tail Kelly) yields a sized `net` **below**
    the naive `net_positions` result. **(AC1)**
  - `sizer_never_exceeds_pretrade_cap` — a large multiplier is clamped so `|net| ≤ cap`, and the sized
    target run through `PreTradeGovernor` is `Send(≤ cap·equity)`. **(AC1)**
  - `neutral_sizer_is_identity` — `Default` multiplier reproduces `net_positions` (no clamp).

## Golden / content_hash impact

Adding a hashed vintage field moves **every** vintage `content_hash`. Affected committed fixtures:
`crates/cli/tests/fixtures/sample_vintage.json` and `crates/server/tests/fixtures/sample_vintage.json`
(byte-identical copies). Regenerated via the real path
(`cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact`), never hand-edited;
the server copy is refreshed from the regenerated cli fixture. `golden_result.json` carries **no**
`content_hash`, and the advisory sizer does not affect backtest reporting, so it is unaffected.
`VINTAGE_FORMAT_VERSION` bumped 4 → 5. Before→after hashes reported in the PR.

## Risks

- **f64 determinism of the solve.** `ln`/golden-section are deterministic per build; the multiplier is
  quantised+normalised to `Decimal` (serialize-idempotent), matching the `SlippageCalibration`/`hash_stable`
  precedent, so the sealed value round-trips byte-identically.
- **Over-de-risking.** A non-positive in-sample combined drift sizes to `0`. This is the honest Kelly
  output; the hard cap and breakers are unaffected. Documented, not clamped away.
- **Scope containment.** No runtime loop rewire (the existing `net_positions` is likewise consumed only at
  its call sites); the sized method + tests satisfy "consume in `live_netter`" without widening the diff.
