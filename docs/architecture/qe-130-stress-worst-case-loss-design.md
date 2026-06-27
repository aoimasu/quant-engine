# QE-130 — Stress / worst-case-loss scenarios — design note

`Phase: P1` · `Area: ⑥/risk` · `Depends on: QE-127`
`Branch: qe-130/stress-worst-case-loss`

## Goal (from backlog)

*(Reviewer-added.)* "Tail-aware returns" optimises the distribution; it does **not** bound worst-case
capital loss. A pre-declared max-loss needs evidence.

- Run candidate ensembles through historical crash windows + synthetic shocks (gap, funding-spike, ADL);
  produce a worst-case-loss figure per vintage.

**Acceptance criteria.**
- [ ] Each vintage carries a worst-case-loss figure under the stated stress set, feeding G3 (QE-308).

**Out of scope.** Live margin enforcement (QE-215).

## Current-state evidence & placement

- `qe_ensemble::objective` already gives the building blocks: `combined_returns(pool, members)` (the
  ensemble's per-period return path — equal-weight), `cdar` (the equity/peak drawdown path → CVaR), and
  `stress_overlay` (append synthetic shocks before the tail). QE-115's CVaR/CDaR optimise the *average*
  tail; QE-130 instead needs the **single worst peak-to-trough capital loss** under an explicit stress set.
- **Stress engine in `qe-ensemble`** (Area ⑥) — it operates on the same per-strategy return-series domain
  as QE-126/127 and needs nothing the crate doesn't already have (no new deps; firewall untouched).
- **The vintage carries the figure as a plain `Option<f64>`** (`qe-vintage`, QE-129) — NOT the
  `StressReport` type, so `qe-vintage` keeps **no `qe-ensemble` dep** (the firewall-clean, pure-data
  property from QE-129 holds). The assembly pipeline runs the stress engine and attaches the bare figure.

## Design

### D1 — Loss metric: worst peak-to-trough drawdown

`max_drawdown(returns) -> f64` walks the equity curve (`equity *= 1+r`, tracking `peak`) and returns the
most negative `equity/peak − 1` as a **positive fraction** (`0.35` = a 35% capital loss). This is
worst-case capital loss — the single worst trough — distinct from QE-115's CVaR (an average over the tail).

### D2 — The ensemble's actual return path

`weighted_combined(series, weights) -> Vec<f64>` combines the **selected** strategies' return series by
their (capacity-capped, QE-128) `weights` — the ensemble's *actual* allocation the vintage records, not
the equal-weight `combined_returns`. Truncated to the shortest member series; empty ⇒ empty.

### D3 — The stress set

`StressScenario` (each named, so the binding one is identifiable):
- `HistoricalWindow { name, start, len }` — replay a known crash window: loss = `max_drawdown` of the
  base path restricted to `[start, start+len)`. (The caller supplies the index ranges of known crashes.)
- `Gap { name, adverse_return }` — a sudden adverse price jump.
- `FundingSpike { name, per_period, periods }` — a sustained funding-cost drag over `periods`.
- `Adl { name, haircut }` — auto-deleveraging: the venue force-closes at a `haircut` in the crash.

**Synthetic-shock model (worst-case = shock coincides with the existing worst drawdown).** Let `d0` be the
base path's `max_drawdown` and `gross = Σ|weights|` the exposure the shock scales with. Each synthetic
shock contributes an extra instantaneous loss `e` *compounded at the trough*:
`compound(d0, e) = 1 − (1−d0)·(1−e)`, with
- Gap: `e = adverse_return · gross`
- FundingSpike: `e = per_period · periods · gross`
- Adl: `e = haircut · gross`

Compounding at the trough is the conservative worst case (the shock lands when the book is already down),
which is exactly what a *pre-declared max-loss* should be evidenced against.

### D4 — The worst-case figure

`worst_case_loss(series, weights, scenarios) -> StressReport { worst_case_loss, binding_scenario,
per_scenario }` = the **maximum** scenario loss, the scenario that produced it, and the full per-scenario
breakdown (so G3/QE-308 can audit which stress binds). A default constructor
`default_synthetic_shocks()` supplies gap/funding/ADL at documented default magnitudes; historical
windows are caller-supplied (they encode calendar knowledge the engine doesn't have).

### D5 — Vintage integration

`VintageContent` gains `worst_case_loss: Option<f64>` (the D4 figure), and `VINTAGE_FORMAT_VERSION`
bumps `1 → 2` (the schema — and thus the content hash — changed; the QE-129 hashing contract requires the
version bump to be explicit). `validate()` additionally rejects a **negative or non-finite**
`worst_case_loss` (a loss fraction is `≥ 0`). No new `qe-vintage` dep.

## Module / API plan

- `crates/ensemble/src/stress.rs` (new): `StressScenario`, `ScenarioLoss`, `StressReport`,
  `max_drawdown`, `weighted_combined`, `scenario_loss`, `worst_case_loss`, `default_synthetic_shocks`,
  `DEFAULT_GAP_*`/`DEFAULT_FUNDING_*`/`DEFAULT_ADL_HAIRCUT`. Re-exported from `lib.rs`.
- `crates/vintage/src/lib.rs`: add `worst_case_loss: Option<f64>` to `VintageContent`, bump the version,
  extend `validate()`.

## Test plan (TDD)

1. **`max_drawdown`** — a known up/down path has the expected worst trough; a monotonically rising path ⇒ 0.
2. **`weighted_combined`** — weighting reproduces a hand-computed path; differs from equal-weight when
   weights are skewed.
3. **Scenario losses** — each synthetic shock compounds at the trough (`compound(d0,e)` exact);
   a historical window returns its window drawdown; a bigger shock ⇒ a bigger loss (monotone).
4. **`worst_case_loss` (AC)** — over a set (crash window + gap + funding + ADL), the report's figure is the
   max, `binding_scenario` names the right one, and `per_scenario` covers all. Heavier shocks raise it.
5. **Vintage carries it** — a sealed/round-tripped vintage carries its `worst_case_loss`; `seal` rejects a
   negative/non-finite figure; the version is `2` and is in the hash.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-ensemble`,
`cargo test -p qe-vintage`, `cargo test --workspace`, `cargo deny check`.

## Risks

- **Synthetic-shock magnitudes are modelling choices.** Defaults are documented constants; callers
  override per venue/asset. The compound-at-trough model is deliberately conservative (worst-case), not a
  best-estimate — appropriate for a max-loss bound, and the per-scenario breakdown keeps it auditable.
- **Historical windows are caller-supplied index ranges**, not calendar-resolved here — the engine has no
  calendar; QE-308 (G3) / the assembly layer supply known crash ranges. Documented.
- **Vintage format bump (`1→2`).** Greenfield — no persisted vintages exist — so the additive field +
  version bump is safe; the bump keeps the QE-129 hashing contract honest.
- **`gross = Σ|weights|`** assumes weights express exposure fractions (QE-128 capacity-capped, `[0,1]`);
  a leveraged/short book would carry that in the weights, which the `Σ|·|` handles.
