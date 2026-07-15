# QE-417 — Time-aware (gap-aware) mark EMA for the drawdown-breaker feed

**Ticket:** QE-417 (spec: `docs/reviews/2026-07-15-team-improvement-review.md` → `### QE-417`).
**Area:** trading / runtime-risk. **Phase:** P2 runtime-risk. **Out of scope:** wss reconnection plumbing.

## 1. Current state (file:line evidence)

### 1.1 The EMA fixes `alpha` from an *assumed* 1s spacing
`crates/risk/src/breaker.rs:31-45` — `MarkEma::with_half_life`:

```
alpha = 1 − 0.5^(tick_secs / half_life_secs)      // tick_secs is a *constant* (1.0 for the baseline)
```

`alpha` is computed **once** at construction from a fixed `tick_secs` and frozen into `self.alpha`
(`breaker.rs:27`). `update` (`breaker.rs:57-64`) then applies that same `alpha` on every sample regardless of
how much real time elapsed between ticks:

```
Some(prev) => prev + self.alpha * (price - prev)
```

So one call to `update` == exactly one nominal 1s step, always.

### 1.2 `MarkEmaLoop` carries `event_time_ms` but never uses it
`crates/runtime/src/live_mark.rs:66-73` — `MarkEmaLoop::observe`:

```
pub fn observe(&mut self, event_time_ms: i64, raw: Decimal) -> MarkTick {
    let smoothed = self.ema.update(raw);          // <-- event_time_ms is NOT read here
    MarkTick { event_time_ms, raw, smoothed }
}
```

`event_time_ms` is only echoed into the emitted `MarkTick`; it never influences the smoothing. There is no
`prev_event_time_ms`, no Δt, no gap detection, and no staleness signal.

### 1.3 Failure mode
markPrice@1s streams gap on wss reconnects (multi-minute). After a gap, the first post-gap sample is smoothed as
a *single* 1s step (`alpha ≈ 0.0115`), so the smoothed mark moves only ~1.1% toward the true post-gap price and
lags reality by minutes. The slow/med-DD probe (`CircuitBreaker::observe`, `breaker.rs:161`) then runs on a stale
equity proxy and can miss or late-fire a real drawdown precisely during disconnect-and-recovery windows.

### 1.4 Consumers (blast radius)
`MarkEma` is used only in `breaker.rs` and re-exported (`crates/risk/src/lib.rs:28`). `MarkEmaLoop` / `MarkTick`
are used only in `live_mark.rs` and re-exported (`crates/runtime/src/lib.rs:86`). No other crate reads
`MarkTick.raw`/`.smoothed`. Adding a field / method is safe. `qe-runtime` already depends on `qe-risk`, so no new
firewall edge is introduced (`crates/architecture/tests/firewall.rs`).

## 2. Design — time-aware alpha (chosen approach)

Derive `alpha` **per tick** from the actual elapsed time `Δt = event_time_ms − prev_event_time_ms`:

```
alpha(Δt) = 1 − 0.5^(Δt_secs / half_life_secs)
```

This is the continuous-time EMA. It is **mathematically identical** to the textbook form
`alpha = 1 − exp(−Δt / τ)` with time-constant `τ = half_life / ln 2 ≈ 86.56 s` for `half_life = 60 s`. I keep the
**base-0.5 half-life** parameterisation (not `exp(−Δt/τ)`) deliberately: it is the *same formula the code uses
today*, with the constant `tick_secs` replaced by the measured `Δt_secs`. That gives **exact** backward-compat and
zero golden churn (§4). Both use `f64` transcendental functions that are deterministic (§5).

**Why time-aware alpha instead of an explicit gap re-seed:** a time-aware alpha handles the gap case for free. At
`Δt = 300 s`, `alpha = 1 − 0.5^(300/60) = 1 − 0.5^5 = 0.96875`, so the EMA jumps 96.875% of the way to the fresh
sample — i.e. it *nearly re-seeds*, and does so smoothly (no discontinuous branch, no threshold to tune). As
`Δt → ∞`, `alpha → 1` (a full re-seed). One formula covers both the nominal and the gap regime.

### 2.1 Numbers
| Δt | `alpha` | smoothed after seed=100 then step→200 |
|----|---------|----------------------------------------|
| 1 s (nominal) | 0.0114859796… | 101.1486 |
| 300 s (gap) | 0.96875 | 196.875 |

At Δt=1s the value is byte-identical to today's fixed alpha; at Δt=300s it nearly jumps to the new price.

### 2.2 Where the logic lives
- `MarkEma` becomes time-aware: new `update_after(dt_secs, price)` computes the per-tick alpha from `Δt`. The
  existing `update(price)` is retained unchanged (uses the frozen nominal alpha) for the existing MarkEma unit
  tests and any fixed-cadence caller. `MarkEma` stores `half_life_secs: Option<f64>` — `Some` when built via
  `with_half_life` (time-aware enabled), `None` when built via `with_alpha` (an explicit fixed coefficient, no
  half-life to derive from → `update_after` falls back to that fixed alpha).
- `MarkEmaLoop` tracks `prev_event_time_ms`, computes `Δt`, calls `update_after`, and raises the staleness flag.
  `event_time_ms` already flows here (§1.2) — no wall-clock read is introduced (§5).

### 2.3 Defensive handling (panic-free prod path; `clippy::unwrap_used = deny`)
- **First sample** (`value == None`): seed to `price`, return it; `Δt` is irrelevant (unchanged from today).
- **Δt ≤ 0** (duplicate timestamp / out-of-order / clock skew): clamp `Δt_secs = Δt_secs.max(0.0)`. At `Δt = 0`,
  `alpha = 1 − 0.5^0 = 0`, i.e. the EMA does **not** move — a zero-elapsed-time sample cannot advance smoothing.
  This is deterministic and cannot panic.
- `f64 → Decimal`: reuse the existing `Decimal::from_f64_retain(a).unwrap_or(Decimal::ONE).clamp(0,1)` idiom
  (`breaker.rs:41-43`) — no bare `unwrap`/`expect`/`panic`.
- `half_life_secs ≤ 0`: `alpha = 1` (no smoothing / always re-seed), as today.

## 3. Staleness health signal
`MarkTick` gains `stale: bool`. `MarkEmaLoop` carries a **configurable** `staleness_bound_secs`
(`DEFAULT_STALENESS_BOUND_SECS = 5.0 s` — 5× the nominal 1s cadence, tolerant of minor jitter but tripping on a
genuine stall). On each non-seed tick, `stale = Δt_secs > staleness_bound_secs`. The flag rides the existing
`MarkTick` stream to every `MarkTickObserver` (the breaker layer QE-212 / cockpit QE-304 seam), so a consumer can
halt or annotate on a stale mark with no new pipeline. Config: `with_config(half_life, tick, bound)` +
`staleness_bound_secs()` getter; `with_half_life`/`spec_baseline` use the default bound (today's behaviour: no
stale flag ever set at 1s cadence, since 1 ≤ 5).

## 4. Backward-compat (steady-state 1s unchanged)
At the nominal 1s cadence `Δt_secs = 1.0`, so `alpha(1s) = 1 − 0.5^(1/60)` — **exactly** the constant the code
computes today in `with_half_life(60, 1)`. Therefore:
- Every existing `MarkEma`/`live_mark` test that feeds 1s-spaced samples produces an identical smoothed series
  (verified by a new equivalence test comparing `update_after(1s)` against the old `update`).
- No goldens change; existing tests (`ema_half_life_is_correct`, `both_raw_and_smoothed_reach_the_observer`, …)
  keep passing untouched.

## 5. Determinism
The EMA stays a pure deterministic function of `(samples, timestamps)`. All time comes from `event_time_ms`
flowing through `MarkEmaLoop` — **no wall-clock reads**. `0.5_f64.powf` / `exp` are deterministic
IEEE-754 operations (bit-reproducible across runs on a target); the same `(price, Δt)` sequence always yields the
same smoothed series. `Δt` is derived by integer subtraction of the event timestamps.

## 6. Risks / mitigations
- **f64 cross-platform reproducibility:** `powf`/`exp` are deterministic per target; the engine already relies on
  f64 alpha here (`breaker.rs:37`). No change in exposure.
- **Bad venue timestamps** (non-monotone / duplicate event times): clamped to `Δt ≥ 0` (§2.3) — never panics,
  never moves the EMA backward.
- **Staleness bound default:** 5s is a policy choice; it is config-driven so ops can tune it. Out-of-scope wss
  reconnection is untouched — this only makes the *smoothing* correct across gaps and *surfaces* the stall.

## 7. Test plan (added)
- (a) **300s gap** → step: smoothed nearly jumps to the post-gap price (≈196.9, ≫ the ≈101.1 a single 1s step
  would give).
- (b) **1s backward-compat:** the time-aware smoothed series at 1s spacing equals the old fixed-alpha series.
- (c) **staleness:** a `Δt` beyond the bound sets `stale = true`; a nominal 1s tick sets `stale = false`.
- Plus MarkEma unit tests: `update_after(1s) == update`, large-Δt near-reseed, `Δt ≤ 0` no-move.
