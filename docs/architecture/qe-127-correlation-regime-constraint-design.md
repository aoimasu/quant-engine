# QE-127 — Correlation penalty + per-regime expectancy constraint — design note

`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-126, QE-125`
`Branch: qe-127/correlation-regime-constraint`

## Goal (from backlog)

*(Reviewer-added.)* Enforce return-correlation diversity and require the ensemble to be net-positive in
each labelled regime, not only on blended history.

- Add the correlation/covariance penalty (QE-115) to the DE objective.
- Constrain/score on per-regime expectancy using QE-125 labels.

**Acceptance criteria.**
- [ ] Highly P&L-correlated combinations are penalised; a per-regime expectancy table is part of the
  ensemble's score; a regime-fragile ensemble is rejected/penalised.

**Out of scope.** Capacity gating (QE-128).

## Current-state evidence

- **QE-115 `objective`** already subtracts `corr_weight · positive_mean_pairwise_corr(members)` — the
  return-correlation penalty. The QE-126 search scores through `objective` (via `leave_one_out_min`), so
  highly P&L-correlated combinations are *already* penalised in the DE search. QE-127 makes that explicit
  (a test pinning it) and **adds the missing half**: a per-regime expectancy constraint.
- **QE-125 `expectancy_table`** (in `qe-signal`) turns a combined return series + per-bar regime labels
  into a per-regime expectancy table. QE-127 feeds the ensemble's combined net-of-cost returns through it.
- **QE-126 `run_de`** is the DE engine; this ticket refactors it to take a `score(members)` closure so the
  regime-aware score can reuse the *identical* loop (determinism preserved — QE-126's tests still pass).

`qe-ensemble` keeps its no-`qe-wfo` firewall; the only inputs are `qe-signal` (regime types) + the pool.

## Design

### D1 — Per-regime expectancy of an ensemble

`per_regime_expectancy(pool, members, labels)` = `expectancy_table(combined_returns(pool, members),
labels)`: the ensemble's equal-weight combined **net-of-cost** returns, bucketed by the QE-125 regime
label of each bar. `worst_regime_expectancy(table)` = the minimum `mean_return` across regime rows — the
ensemble's weakest regime. A *regime-fragile* ensemble (net-positive on blended history but net-negative
in some regime) has a negative worst-regime expectancy.

### D2 — Regime-aware objective (AC)

`regime_aware_objective(pool, members, labels, cfg)` = the QE-115 `objective` (which already carries the
correlation penalty) **minus** a regime shortfall penalty:

```
score = objective(pool, members)                       // mean + tail·CVaR − corr·corr   (QE-115)
        − regime_weight · max(0, regime_floor − worst_regime_expectancy)
```

So the per-regime expectancy table is literally part of the score (AC #2), a regime whose expectancy
falls below `regime_floor` (default `0` = "net-positive in every regime") is penalised in proportion to
the shortfall (AC #3), and the correlation penalty rides along from `objective` (AC #1). With a large
`regime_weight` the penalty dominates, so a regime-fragile ensemble is effectively rejected.

### D3 — Regime-aware robust-basin search

Mirroring QE-126/D2, `leave_one_out_min_regime` is the wide-basin floor over the regime-aware objective,
and `regime_aware_cv_score` is the **min across folds** of it (slicing *both* the pool and the labels per
fold, so each fold scores its own regimes). `search_portfolio_regime_aware(pool, labels, cfg, seed)` runs
the shared `run_de` engine with this score, so the DE converges on an ensemble that is robust **and**
regime-positive, net-of-cost. Same seeded `DetRng` ⇒ deterministic.

## Module / API plan

New module `crates/ensemble/src/regime.rs`, re-exported (distinct from `qe_signal::regime`):

- `RegimeAwareConfig { search: SearchConfig, regime_floor, regime_weight }` (+`Default`/`with_defaults`),
  `DEFAULT_REGIME_FLOOR = 0.0`, `DEFAULT_REGIME_WEIGHT = 10.0`.
- `per_regime_expectancy(pool, members, labels) -> ExpectancyTable`,
  `worst_regime_expectancy(&ExpectancyTable) -> f64`.
- `regime_aware_objective(pool, members, labels, cfg) -> f64`,
  `leave_one_out_min_regime(...) -> f64`, `regime_aware_cv_score(...) -> f64`.
- `search_portfolio_regime_aware(pool, labels, cfg, seed) -> SearchResult`.
- `search.rs` refactor: extract `pub(crate) run_de(...)` (the DE engine) so both the base and regime-aware
  searches share one loop. No new deps (`qe-signal` already a dep).

## Test plan (TDD)

1. **Correlation penalised (AC #1).** A two-strategy decorrelated (anti-correlated) ensemble scores
   strictly higher than an otherwise-identical highly-correlated one under `objective` — the corr penalty
   bites.
2. **Per-regime expectancy in the score (AC #2).** `per_regime_expectancy` returns a table with a row per
   labelled regime; an ensemble net-negative in one regime has a negative row, and
   `regime_aware_objective < objective` for it (the table changes the score), while an all-regime-positive
   ensemble's regime-aware score equals its base objective (no shortfall).
3. **Regime-fragile rejected (AC #3).** A strategy with a high blended return but net-negative in one
   regime has a *lower* regime-aware CV score than a steadier all-regime-positive strategy (the blended
   ranking is flipped), and `search_portfolio_regime_aware` excludes the fragile strategy.
4. **Determinism.** Same seed ⇒ identical `SearchResult`.
5. **Engine parity.** The refactored `run_de` leaves QE-126's `search_portfolio` results unchanged (the
   existing QE-126 tests, incl. determinism, still pass).

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-ensemble`,
`cargo test --workspace`.

## Risks

- **Penalty vs hard reject.** A soft shortfall penalty (scaled by `regime_weight`) is used rather than a
  hard `−∞`, so a marginally-fragile ensemble is ranked down rather than discarded outright — more stable
  for the DE landscape and config-ready. A large default weight (`10×`) makes a genuinely net-negative
  regime decisive.
- **Label alignment.** `labels` are paired with the combined returns by index (QE-125's contract); the
  caller aligns labels to the pool's bar index. Per-fold slicing slices labels with the same bounds as the
  pool, so each fold's regimes stay aligned.
- **Regime granularity.** Scoring on the worst single regime is deliberately conservative; richer
  per-regime weighting is a downstream refinement behind `RegimeAwareConfig`. Capacity gating is QE-128.
