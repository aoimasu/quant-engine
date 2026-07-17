# QE-437 — Gate G1 on the already-computed PBO (design / evidence note)

`Phase: Review R2 (P2 — panel #8, unanimous)` · `Area: gate` · `Depends on: QE-134`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-437`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [Review R2.b](../backlog.md#review-r2)

## 1. Problem

`crates/validation/src/pbo.rs` is a faithful CSCV Probability-of-Backtest-Overfitting
(Bailey–Borwein–López de Prado–Zhu 2017), and `assess()` records it into
`RobustnessReport.pbo` (`crates/validation/src/lib.rs:87`, `:115`). But the G1 acceptance
gate (`crates/gate/src/lib.rs::evaluate_g1`) reads only **holdout Sharpe**, **DSR**,
**SPA p-value**, and the **OOS-vs-in-sample tolerance** — it **never reads `pbo`**. The most
direct probability of the §5.4 overfitting concern is computed, sealed into the training
result sidecar, and then ignored at the gate. This is a wiring gap, not a math gap:
**zero new math — the value already exists.**

## 2. Evidence (file:line)

- `crates/validation/src/pbo.rs:33` — `pbo_cscv` computes the CSCV overfit probability.
- `crates/validation/src/lib.rs:87` — `RobustnessReport.pbo: f64` field.
- `crates/validation/src/lib.rs:115` — `assess()` populates `pbo: pbo_report.pbo`.
- `crates/gate/src/lib.rs:60` — `G1Criteria` (5 fields: `min_holdout_sharpe`, `dsr_threshold`,
  `spa_alpha`, `oos_tolerance`, `min_holdout_samples`) — **no PBO threshold**.
- `crates/gate/src/lib.rs:143` — `evaluate_g1` builds 5 `CriterionResult`s — **none reads `pbo`**.
- `crates/cli/src/jobs/train.rs:468` — the only production caller; passes the same
  `RobustnessReport` (which already carries `pbo`) into `evaluate_g1`.

## 3. What to build

Add a **pre-registered** criterion `pbo < 0.5` to `G1Criteria` and `evaluate_g1`:

1. New const `DEFAULT_MAX_PBO: f64 = 0.5` — the pre-registered ceiling. 0.5 is the neutral
   overfit line: a strategy whose in-sample winner ranks *below* the OOS median more than
   half the time is more likely overfit than not. Not tuned to any fixture.
2. New field `G1Criteria.max_pbo: f64`, defaulted from `DEFAULT_MAX_PBO` in `with_defaults()`.
3. New `CriterionResult` in `evaluate_g1` named `pbo_below_overfit_threshold`:
   `passed = robustness.pbo.is_finite() && robustness.pbo < criteria.max_pbo`,
   `value = robustness.pbo`, `threshold = criteria.max_pbo`.

### Fail-closed on a missing / absent PBO

The gate already has a "refuse rather than pass vacuously" discipline (the
`holdout_has_sufficient_samples` guard: an undersized holdout is **blocked**, not passed).
`pbo` is a plain `f64` (not an `Option`), always populated by `assess()`, so "absent" here
means a **non-finite** value (`NaN`/`±∞`) — the shape a degenerate CSCV would produce. The
criterion uses `is_finite() && pbo < max_pbo`, so a non-finite PBO yields `passed = false`
and **blocks promotion**. (Even the bare `pbo < 0.5` comparison fails closed on `NaN` since
`NaN < 0.5` is `false`; the explicit `is_finite()` guard makes the intent auditable and also
blocks `+∞`.) This mirrors the undersized-holdout discipline: an unusable statistic blocks,
never passes vacuously.

## 4. Scope / diff

Single-file behaviour change: `crates/gate/src/lib.rs` (const + one field + one criterion +
doc/tests). One test-assertion follow-through in `crates/cli/tests/train_job.rs`
(`g1.criteria.len()` 5 → 6). No other crate changes.

## 5. Golden / content_hash impact — analysed BEFORE coding

- **Vintage `content_hash`: does NOT move.** `G1Decision`/`G1Criteria` are **not** part of
  `VintageContent` (`crates/vintage/src/lib.rs:39`) — the sealed, hashed artefact. The G1
  decision rides only in the **`TrainResultDoc` sidecar** (`crates/cli/src/jobs/train.rs:225`,
  `g1: G1Decision`), which is *not* fed into `VintageContent::content_hash`. Adding a hashed
  field to the criteria therefore does **not** re-address any vintage.
  ⇒ **No `VINTAGE_FORMAT_VERSION` bump** (currently `5`, `crates/vintage/src/lib.rs:35`).
- **`crates/cli/tests/fixtures/golden_result.json`: does NOT move.** That golden is the
  **backtest** job output (trades / equity curve / drawdown / metrics) — it carries no `g1`
  field, and its embedded `content_hash` is a static `sample_vintage` fixture, independent of
  the gate. Confirmed by inspection.
- **`crates/cli/tests/train_job.rs:163`: the only golden-ish assertion that moves** —
  `assert_eq!(outcome.result.g1.criteria.len(), 5)` becomes `6`. This is a test constant, not
  a regenerated artefact; updated by hand in the test (it asserts a structural count, not a
  hashed value).

Net: no artefact is regenerated because none moves. Only source (`gate/src/lib.rs`) and the
one test count change.

## 6. Tests (TDD)

In `crates/gate/src/lib.rs`:
- `a_clean_vintage_is_promoted` — `good_robustness()` has `pbo = 0.10 < 0.5`, so the new
  criterion passes and promotion still holds (regression guard for the pass path).
- Extend `each_criterion_blocks_promotion_alone` — set `pbo = 0.60 ≥ 0.5` and assert
  `failed_criteria() == ["pbo_below_overfit_threshold"]` (rejection + per-criterion evidence).
- `a_high_pbo_blocks_and_records_evidence` (new) — `pbo = 0.75` blocks; the recorded
  `CriterionResult` carries `passed = false`, `value = 0.75`, `threshold = 0.5`.
- `a_non_finite_pbo_fails_closed` (new) — `pbo = f64::NAN` blocks (fail-closed discipline).
- Update `decision_is_recorded_with_evidence` — `criteria.len()` 5 → 6.

## 7. Acceptance-criteria mapping

- *Add `pbo < 0.5` to `G1Criteria` and `evaluate_g1`* → §3.
- *Covered by focused unit/property tests* → §6.
- *If any golden/vintage moves, regenerate via real code and track `content_hash`* →
  §5: nothing moves (G1 not sealed); only a test count updates.
- *Full green gate + determinism* → run in step 4.

`Spec ref: §5.4 (overfitting is the central risk; PBO is its most direct probability).`
