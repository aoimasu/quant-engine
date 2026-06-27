# QE-113 — SPIKE: Geometric fitness, noise-robust eval & purged/embargoed CV — design / decision record

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-109` · **Blocks: QE-120, QE-117**
`Branch: qe-113/geometric-fitness-purged-cv`

## Goal (from backlog)

Fitness must be net-of-cost, robust to the fat-tailed noise of financial series, and validated without
leakage; standard k-fold CV is invalid on autocorrelated series.

**Scope / requirements.**
- Define geometric (time-average growth) fitness on **net-of-cost** equity; document sensitivity to
  near-ruin periods and the compounding resolution.
- Define noise-robust evaluation (multi-window / bootstrap resampling) and how elite replacement
  accounts for standard error (don't replace on a noisy single improvement).
- Specify **purged + embargoed** cross-validation (purge = max indicator lookback + label horizon;
  embargo after each test fold). Standard k-fold is explicitly rejected.

**Acceptance criteria.**
- [ ] Decision record defines fitness + CV scheme.
- [ ] A fixture proves train/test bar sets are provably disjoint **including lookback**.
- [ ] Documented embargo length.

**Out of scope.** Statistical deflation suite (QE-131); the WFO window manager (QE-117); the backtester
that produces the return series (QE-120); minimum-trade-count gate (QE-120).

## Current-state evidence

- **QE-109** (`qe_wfo::friction`) produces the **net-of-cost** P&L (`PnlBreakdown`: gross − fees −
  slippage − funding). This ticket consumes the resulting per-period net returns; fitness never sees a
  gross number.
- **QE-107** declares each indicator's `lookback`; `qe_signal::max_lookback(cfg)` /
  `FeatureSchema::max_lookback()` give the catalogue max — the **purge** size's first term.
- **QE-006** determinism: fitness and CV must be pure/ordered. Multi-window evaluation is deterministic
  (no RNG); a bootstrap variant, if used, must draw from `DetRng`.

## Decision

### D1 — Fitness = time-average (geometric) log-growth on net-of-cost returns

Per-period **net** simple returns `r_i` (period = the backtest's compounding step, fixed and consistent
— see D2). The fitness is the **ergodic / time-average growth rate**:

```
fitness(r) = mean_i ln(1 + r_i)        # the per-period log-growth (Kelly/ergodic criterion)
geom_return(r) = exp(fitness(r)) − 1    # the equivalent per-period compound return (human-readable)
```

We optimise the **log-growth mean**, not the arithmetic mean of returns, because compounding is
multiplicative: a strategy is only as good as the *time-average* an investor actually experiences, which
penalises volatility drag automatically (`ln(1+r)` is concave).

**Near-ruin sensitivity (documented, deliberate).** `ln(1+r)` → −∞ as `r` → −100%. A single period of
`r ≤ −1` (total loss) makes `fitness = −∞`: ruin is **absorbing** and the worst possible fitness, by
construction. This is the correct economic behaviour (you cannot recover from a wiped account) and is
why geometric fitness — not arithmetic mean — is mandated. `fitness` returns `f64::NEG_INFINITY` on any
`r_i ≤ −1`; `geom_return` reports `−1.0` (−100%). Empty return series ⇒ `0.0` (no growth; the
minimum-trade-count gate is QE-120, not here).

### D2 — Compounding resolution is fixed and consistent

`r_i` are **per-trade-period net returns at one fixed resolution** (the backtester's step). The mean is
over the same unit throughout, so two genomes are compared on the same compounding base; mixing
per-bar and per-trade returns would make the geometric mean meaningless. The resolution is the
backtester's contract (QE-120); this module assumes the caller passes a single consistent series.

### D3 — Noise-robust evaluation: multi-window distribution + standard error

A single backtest number is one draw from a fat-tailed distribution. We evaluate a genome over **K
windows** (multi-window resampling; a moving/blocked bootstrap is the documented alternative and must
use `DetRng`), producing a **distribution** of per-window fitness:

```
NoiseRobustFitness { mean, std_error, n }
  mean       = mean_k fitness(window_k)
  std_error  = sample_sd(fitness over windows) / sqrt(K)      # SE of the mean
```

If a genome is ruined in **any** window (`fitness_k = −∞`) the mean is `−∞` — a strategy that blows up
in any regime is untrusted. `K = 1` yields `std_error = 0` (no noise estimate); noise-robustness
**requires K ≥ 2**, enforced by the replacement rule below being a no-op when SE information is absent.

### D4 — SE-aware elite replacement (don't replace on noise)

When a challenger contests an incumbent elite, replace **only if the improvement clears the noise band**:

```
replace ⇔ challenger.mean − incumbent.mean  >  k_sigma · sqrt(incumbent.se² + challenger.se²)
```

with `k_sigma = 1.0` default (configurable). A challenger that is better by less than `k_sigma` combined
standard errors is **within noise** and does not displace the incumbent — this is the QE-118/120 archive
hygiene that stops the search from churning elites on lucky single evaluations. (With both `n = 1` the
combined SE is 0 and the rule degenerates to strict-greater; callers must pass K ≥ 2 for the noise guard
to bite — D3.)

### D5 — Purged + embargoed cross-validation (standard k-fold rejected)

Plain k-fold leaks on autocorrelated series two ways: (i) a train bar's **feature lookback** window
reaches into the test block; (ii) a train bar's **label horizon** reaches into the test block; and
(iii) serial correlation bleeds across the test→train boundary even when indices don't overlap. We
therefore use **purging + embargo** (López de Prado):

- **purge `= max_indicator_lookback + label_horizon`.** Around each contiguous test block, drop every
  train bar whose information window `[i − lookback, i + label_horizon]` could overlap the test block.
  Concretely, exclude train indices in `[test_start − purge, test_end + purge + embargo)`.
- **embargo (after each test fold).** Additionally exclude `embargo` bars immediately **after** the test
  block from train, to kill residual serial-correlation leakage that a deterministic purge misses.
  **Documented default: `embargo = max_indicator_lookback`** (one full feature window; configurable).

**Provable disjointness including lookback (the AC).** With purge `= L + H`, every kept train bar `tr`
and every test bar `te` satisfy `|tr − te| > L + H`, so their information windows
`[·−L, ·+H]` are **disjoint** — no feature-lookback or label-horizon overlap in either direction. The
fixture asserts exactly this for all (train, test) pairs across all folds, and contrasts it with naive
k-fold (adjacent bars, `|tr − te| = 1`, windows overlap) which fails the same assertion.

## Module / API plan

Two new modules in `qe-wfo`, re-exported:

- `crates/wfo/src/fitness.rs`
  - `log_growth(returns: &[f64]) -> f64` (D1 fitness; `−∞` on ruin, `0.0` on empty); `geom_return`.
  - `NoiseRobustFitness { mean, std_error, n }`, `NoiseRobustFitness::from_windows(&[Vec<f64>])` (D3).
  - `should_replace(incumbent, challenger, k_sigma) -> bool` (D4); `DEFAULT_K_SIGMA`.
- `crates/wfo/src/cv.rs`
  - `PurgedKFold { n_folds, lookback, label_horizon, embargo }`; `purge()`.
  - `Fold { test: Range<usize>, train: Vec<usize> }`; `PurgedKFold::folds(n_bars) -> Vec<Fold>` (D5).
  - `Fold::windows_disjoint(lookback, label_horizon) -> bool` (the invariant, also used in tests).

No new dependencies (pure `f64`/index math, deterministic).

## Test plan (TDD)

1. **Geometric fitness correctness.** Hand-computed `log_growth` / `geom_return` on a small series; a
   `+50%` then `−50%` round-trip shows the geometric drag (net negative), unlike the arithmetic mean.
2. **Near-ruin is absorbing.** Any `r_i ≤ −1` ⇒ `log_growth = −∞`; empty ⇒ `0.0`.
3. **Noise-robust mean/SE.** `from_windows` matches hand-computed mean and SE; a ruined window ⇒ `−∞`.
4. **SE-aware replacement.** A challenger better by < `k_sigma`·SE does **not** replace; one better by
   > `k_sigma`·SE does; ruin never replaces a finite incumbent.
5. **Purged/embargoed disjointness (AC).** For every fold, all (train, test) pairs satisfy
   `|tr − te| > lookback + label_horizon` (`windows_disjoint`); the embargo region after each test block
   is absent from train; train ∩ test = ∅.
6. **k-fold is rejected (contrast).** The same data without purge/embargo produces adjacent train/test
   bars that **fail** `windows_disjoint`, demonstrating why plain k-fold leaks.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **`label_horizon` is strategy-dependent.** Genomes here are signal-driven (entry/exit on bar close),
  so the natural label horizon is the max holding (or 1 bar for next-bar-open fills). It is a
  `PurgedKFold` parameter, not hard-coded; QE-117/120 set it from the genome/eval contract.
- **SE underestimation from overlapping windows.** Multi-window fitness draws can be correlated, so the
  naive SE understates true uncertainty; the blocked-bootstrap alternative (D3) addresses it and is the
  documented upgrade path if elite churn proves too high (QE-124 evidence).
- **`k_sigma` / `embargo` are pre-data constants.** Constructor/params now, config (QE-002) once real
  fill counts and autocorrelation lengths are measured; embargo default = max lookback is a conservative
  starting point.
- **Fixed compounding resolution assumption (D2).** If QE-120 ever mixes resolutions the geometric mean
  breaks; the contract is documented and should be guarded where the series is assembled.
