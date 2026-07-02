# QE-212 — Circuit-breaker layer — design note

`Phase: P2` · `Area: ④ Live pipeline / risk` · `Depends on: QE-116, QE-208, QE-210` · `Branch: qe-212/circuit-breaker-layer`

## Goal (from backlog)

Per-strategy/direction/ensemble limits + slow/med/fast DD thresholds clamp gated strategies to flat before
netting.

- Implement the breaker model (QE-116) consuming the **calibration profile** and the **equity stream**
  (smoothed mark per spec; raw-mark fast tier only if QE-116 adopts it); clamp gated strategies to flat
  **before netting**.

**Acceptance criteria.**
- [ ] A breach clamps the affected scope to flat before netting; behaviour **matches the historical
  backtest (QE-116) on replay**.

**Out of scope.** Pre-trade caps (QE-215); kill-switch (QE-216); netting (QE-213) — this ticket *produces*
the clamped (post-breaker) decisions netting consumes.

## Current-state evidence & placement

- QE-116 already implemented the breaker primitive: `qe_risk::CircuitBreaker::{new, observe, peak, reset}`
  (`crates/risk/src/breaker.rs`), a pure function of an equity stream that fires the most severe of
  slow/med/fast tiers, plus `breaker::replay(thresholds, fast_window, equity) -> Vec<(usize, BreakerTier)>`
  for historical replay. QE-212 **reuses** it — no new breaker math.
- QE-116 also defined the calibration sidecar `qe_risk::CalibrationProfile { per_strategy:
  BTreeMap<String, BreakerThresholds>, per_cohort, ensemble_fast_drop: Fraction }`.
- QE-208 produces the smoothed-mark tick stream; QE-210's `CommittedPeak` is the all-time anchor the breaker
  uses internally. The **equity stream** the breaker observes is built from the smoothed mark × positions,
  net-of-cost — that live feed is QE-217's concern; QE-212 consumes the resulting per-scope equity ticks
  (the same declared-input boundary QE-210 established for equity).
- **Placement: new `crates/runtime/src/live_breakers.rs`** (Area ④ `live_breakers`), exported from `lib.rs`.
  `qe-runtime` already depends on `qe-risk` and `qe-signal`. No new dependency, no cross-crate edge → QE-132
  firewall unaffected.

## Design

### D1 — `BreakerLayer` — per-strategy + ensemble breakers, latched gating

Holds one `CircuitBreaker` per strategy (aligned to the vintage's chromosomes) plus one **ensemble**
breaker, and a **latched** gated flag per scope (once a scope trips it stays gated until `reset` — a
flattened strategy does not un-flatten on a noisy recovery):

- `new(per_strategy: Vec<BreakerThresholds>, ensemble_fast_drop: Fraction, fast_window: usize)`.
- `from_calibration(profile, strategy_ids: &[String], fast_window)` — **consumes the calibration profile**:
  strategy `i` uses `profile.per_strategy[strategy_ids[i]]`; a strategy **missing** calibration gets a
  zero-threshold breaker (fires immediately — the fail-safe QE-116's `calibrate_threshold` already uses for
  an empty distribution: never trade an uncalibrated strategy). The ensemble breaker uses
  `profile.ensemble_fast_drop` as a **fast-drop-only** breaker (slow/med set to `1.0`, i.e. never).
- `observe_strategy(i, equity) -> Option<BreakerTier>` / `observe_ensemble(equity) -> Option<BreakerTier>`
  — feed a scope's breaker; latch it gated on any trip; return the tier for observability.
- `is_gated(i) -> bool` — `ensemble_gated || strategy_gated[i]` (ensemble gates **all**).
- `clamp(decisions: &[ChromosomeDecision]) -> Vec<ChromosomeDecision>` — for each decision, if its strategy
  is gated, replace it with `Decision::Exit` (flatten to flat); otherwise pass through. This is the
  "**clamp gated strategies to flat before netting**": `Exit` drives the position to flat and keeps it flat,
  so the gated strategy's netted contribution (QE-213) is zero.
- `reset()` — clear all gating + breakers (new vintage / session rollover).

**Why per-strategy + ensemble (scope note).** These are the two unambiguously-keyed scopes (strategy index;
the single ensemble). Per-**direction** and per-**cohort** gating use the *identical* `CircuitBreaker` +
latched-gate mechanism keyed by those scopes; they are deferred here because the strategy→direction/cohort
map and the per-direction **aggregate equity** stream arrive with QE-213 netting / the vintage-ensemble
metadata (QE-129). The AC — a breach clamps the affected scope + matches QE-116 replay — is fully met with
strategy + ensemble: a strategy breach clamps that strategy; an ensemble breach clamps all.

### D2 — Replay parity (the AC's second half)

Because `observe_strategy` delegates to the same `CircuitBreaker::observe`, feeding a strategy's breaker an
equity series yields exactly the tiers `breaker::replay(thresholds, fast_window, equity)` produces on the
same series — proven directly by a test that compares the live layer's `(i, tier)` events to `replay`'s.

## Test plan (deterministic)

1. `live_layer_matches_qe116_replay` (**AC, half 2**) — feed a crafted equity series (rise then a
   slow/med/fast drawdown) through `observe_strategy` and assert the emitted `(index, tier)` events equal
   `breaker::replay(thresholds, fast_window, series)`.
2. `strategy_breach_clamps_that_strategy_to_flat` (**AC, half 1**) — a 2-strategy layer; drive strategy 0
   past its threshold; `clamp` turns strategy 0's `Enter/Hold` into `Exit` while strategy 1 passes through.
3. `ensemble_breach_clamps_all_strategies` — an ensemble fast-drop breach gates every strategy; `clamp`
   flattens all.
4. `gating_is_latched` — after a trip, `is_gated` stays true even when later equity recovers above the
   threshold (the strategy stays flattened until `reset`).
5. `from_calibration_wires_profile_and_fails_safe_on_missing` — `from_calibration` maps `per_strategy` by id
   and the ensemble fast-drop; a strategy id absent from the profile gets an immediately-firing breaker.
6. `reset_clears_gating` — `reset` un-gates and re-arms the breakers.

## Risks

- **Equity stream is a declared input (as in QE-210).** The net-of-cost live equity feed built from the
  QE-208 smoothed mark is QE-217; QE-212 consumes per-scope equity ticks. Documented; the replay-parity AC
  is met at the breaker layer.
- **Missing-calibration policy.** Fail-safe (immediate gate) rather than fail-open — deliberate and
  consistent with QE-116's empty-distribution behaviour; documented.
