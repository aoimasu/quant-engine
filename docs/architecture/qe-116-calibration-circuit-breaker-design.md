# QE-116 — SPIKE: Calibration profile & circuit-breaker model — design / decision record

`Phase: P1` · `Area: ⑥/⑦ + risk` · `Depends on: QE-113` · **Blocks: QE-129, QE-212**
`Branch: qe-116/calibration-circuit-breaker`

## Goal (from backlog)

The breaker thresholds are calibrated per-vintage before deployment; the breaker model must be
backtestable on history (not first seen live).

**Scope / requirements.**
- Define per-vintage calibration profile contents: per-strategy / per-cohort (slow + fast DD) /
  ensemble fast-drop thresholds.
- **Baseline (spec, A2):** thresholds **calibrated prior to deployment based on observed behaviour**
  (the per-vintage sidecar) per `docs/specs.md` Robustness.
- **Documented alternative (reviewer):** calibrating on an **OOS/stressed** distribution with an
  explicit safety margin — recorded as an option to revisit; not the baseline.
- Define the smoothed-mark (EMA τ½=60s) tick observer driving the equity stream per spec. **Documented
  alternative (reviewer, A3):** an additional unsmoothed **raw-mark fast tier** so smoothing can't blind
  the fast breaker to gap events — recorded as an option, not baseline.
- Make the breaker model runnable inside the WFO harness on history.

**Acceptance criteria.**
- [ ] Decision record specifies the spec-baseline calibration (observed behaviour / per-vintage sidecar)
  and documents the OOS/stressed and raw-mark-fast-tier alternatives.
- [ ] A historical replay shows slow/med/fast breakers firing across distinct regimes.

**Out of scope.** Runtime breaker wiring (QE-212); the kill-switch mechanics (QE-009 — the breaker
*decides*, the kill switch *acts*).

## Current-state evidence

- **QE-009** (`qe_risk`) gives the risk vocabulary the breaker lives beside: `Fraction` (a validated
  `Decimal ∈ [0,1]`), `RiskError`, `LimitKind::DrawdownCap`, and the kill-switch contract the breaker's
  decision ultimately routes to. The breaker model belongs in `qe-risk` because **both** WFO (calibration
  on history) and runtime (QE-212) consume it — a shared low-level crate, no float money.
- **QE-113** fixes the net-of-cost equity philosophy; the breaker observes an **equity stream**, and the
  spec derives that stream from a **smoothed mark** (EMA τ½=60s).

## Decision

### D1 — Smoothed-mark EMA tick observer (τ½=60s), with a raw-mark alternative

`MarkEma::with_half_life(half_life_secs, tick_secs)` is an exponential moving average over the mark
price with the per-tick smoothing `alpha = 1 − 2^(−tick/half_life)` (so τ½ = 60s at 1s ticks per spec).
It produces the **smoothed mark** that drives the slow-DD equity probe — smoothing rejects 1-tick noise
so the slow/medium breakers don't trip on jitter. `Decimal` throughout (no float money; `alpha` is a
derived smoothing coefficient, not a price).

**A3 alternative (reviewer, documented, not baseline).** A smoothed mark can *blind the fast breaker to
a gap*: a genuine instantaneous crash is averaged away. The breaker therefore *also* exposes the **raw
(unsmoothed) mark** to the fast tier as a documented option — the fast-drop tier can watch raw equity so
a gap fires immediately. Baseline uses the smoothed stream per spec; the raw-mark fast tier is wired only
if that option is adopted.

### D2 — Three-tier circuit breaker (slow / med / fast)

`CircuitBreaker::observe(equity) -> Option<BreakerTier>` walks the equity stream tick by tick, tracks
the running peak, and fires the **most severe** triggered tier:

- **Fast** — a *rapid* drop: equity falls ≥ `fast_drop` within the last `fast_window` ticks (speed, not
  depth). The most urgent (flatten-now) signal; fires even at small total drawdown.
- **Med** — total drawdown from peak ≥ `med_dd`.
- **Slow** — total drawdown from peak ≥ `slow_dd` (the gentlest grind-down probe).

with `slow_dd < med_dd`. Priority Fast > Med > Slow (return the worst applicable). The model is a pure
function of the equity stream — **runnable inside the WFO harness on history** (calibration replay) and,
later, on the live stream (QE-212), with identical code.

### D3 — Per-vintage calibration profile contents

`CalibrationProfile` is the per-vintage sidecar handed to runtime (the QE-129 artefact):

- **per-strategy** thresholds (`BTreeMap<strategy_id, BreakerThresholds>`),
- **per-cohort** slow + fast DD thresholds (`BTreeMap<cohort_id, CohortThresholds>`),
- **ensemble** fast-drop threshold (`Fraction`).

`BreakerThresholds { slow_dd, med_dd, fast_drop }` are all `Fraction`. The profile is `serde`-serialisable
(it rides in the vintage artefact, QE-129) and deterministically reproducible from its inputs (QE-006).

### D4 — Calibration: observed-behaviour baseline (spec A2), OOS/stressed alternative

**Baseline (A2, spec).** Thresholds are **calibrated from observed behaviour** — the in-sample drawdown
distribution of the strategy/cohort/ensemble over the vintage's training history.
`calibrate_threshold(observed_drawdowns, quantile, margin)` = the `quantile` of the observed |drawdown|
distribution, scaled by a `margin ≥ 1`, clamped to `[0,1]` — e.g. `slow_dd` from a mid quantile,
`med_dd`/`fast_drop` from a high quantile. The thresholds therefore sit just beyond what the strategy
*normally* does, so the breaker fires on genuinely abnormal losses, "calibrated prior to deployment".

**Alternative (reviewer, documented, not baseline).** Calibrate on an **OOS / stressed** drawdown
distribution (worse than in-sample) with an explicit **safety margin**, so thresholds are not fit to the
same data the strategy was optimised on. Mechanically identical — `calibrate_threshold` is
distribution-agnostic, so the alternative just passes a stressed distribution and a larger margin —
recorded to revisit if in-sample calibration proves too loose (QE-130/QE-134 evidence). Baseline is A2.

## Module / API plan

Two modules in `qe-risk`, re-exported:

- `crates/risk/src/breaker.rs`
  - `MarkEma::{with_half_life, update, value}` (smoothed mark; raw mark is the caller's input).
  - `BreakerTier { Slow, Med, Fast }`; `BreakerThresholds { slow_dd, med_dd, fast_drop }` (serde).
  - `CircuitBreaker::{new, observe, peak, reset}`; `fast_window`.
  - `replay(thresholds, fast_window, &equity) -> Vec<(usize, BreakerTier)>` (the WFO-harness replay).
- `crates/risk/src/calibration.rs`
  - `CohortThresholds { slow_dd, fast_dd }`; `CalibrationProfile { per_strategy, per_cohort, ensemble_fast_drop }` (serde, round-trips).
  - `calibrate_threshold(observed: &[Decimal], quantile, margin) -> Fraction` (A2 baseline; distribution-agnostic).
- Consumes `qe_risk::Fraction` / `RiskError`; `Decimal` math; no new dependencies.

## Test plan (TDD)

1. **EMA half-life.** After `half_life/tick` ticks of a step input, the smoothed value is ~halfway
   (`1 − 2^(−1) = 0.5`); first sample seeds the EMA; smoothing rejects a 1-tick spike.
2. **Historical replay across regimes (AC).** A synthetic equity series with three regimes — calm
   (no fire), slow grind-down (Slow then Med fire as drawdown deepens), sharp crash within `fast_window`
   (Fast fires) — yields events containing all of Slow, Med, **and** Fast. Runs purely on history.
3. **Tier priority / peak tracking.** Fast beats Med beats Slow when several trip; the peak resets the
   drawdown baseline on new highs; `reset` clears state.
4. **Calibration from observed (A2).** `calibrate_threshold` returns the `quantile·margin` of the
   observed |drawdown| distribution, clamped to `[0,1]`; a larger margin / stressed distribution raises
   the bar (the documented alternative), proving the function is distribution-agnostic.
5. **Profile serde round-trip.** `CalibrationProfile` → JSON → profile is identity (it rides the vintage
   artefact, QE-129).

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-risk`,
`cargo test --workspace`.

## Risks

- **In-sample calibration is optimistic (the A2 concern).** Thresholds fit to training drawdowns may be
  too loose live; carried deliberately per spec fidelity, with the OOS/stressed alternative (D4)
  parameter-ready and flagged for QE-130/QE-134 evidence.
- **Smoothing blinds the fast tier (the A3 concern).** The EMA can average a gap away; the raw-mark fast
  tier (D1) is the documented mitigation, not enabled by default.
- **`fast_window` / quantiles / margin are pre-data constants.** Constructor params now, config (QE-002)
  once real per-vintage drawdown distributions are measured.
- **EMA `alpha` derived via `f64`.** The smoothing coefficient is computed from the half-life in `f64`
  then stored as `Decimal`; it is a coefficient, not money, so no exactness guarantee is lost on prices.
