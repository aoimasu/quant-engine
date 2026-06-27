# QE-134 — GATE G1: Holdout embargo & over-fit acceptance — design note

`Phase: P1` · `Area: gate` · `Depends on: QE-133` · **Blocks: all of Phase 2**
`Branch: qe-134/gate-g1-holdout-embargo`

## Goal (from backlog)

Phase 1 is "validated" only when a vintage clears an **untouched** holdout that no prior P1 ticket
(gating, parent selection, operator credit) was allowed to read.

- Maintain a final time-blocked OOS slice never touched by training/selection.
- Promotion requires: net-of-cost edge persists; DSR > deflated threshold; SPA beats the best-of-N null at
  the stated significance; OOS metrics within pre-registered tolerance of in-sample.

**Acceptance criteria.**
- [ ] A vintage failing any G1 criterion is not promoted; pass/fail is recorded with evidence.

**Out of scope.** Live trust gates (QE-222, QE-308).

## Current-state evidence & placement

- The diagnostics G1 judges already exist: `qe_validation` gives DSR and the SPA p-value (QE-131) and
  `sharpe_ratio` (the net-of-cost edge metric); QE-133 renders the human evidence pack. G1 is the
  **decision**: a pure, pre-registered acceptance function over that evidence, plus the holdout-split
  discipline that guarantees an untouched OOS slice.
- **A new crate `qe-gate`** (Area gate). It depends only on `qe-validation` (`RobustnessReport`,
  `sharpe_ratio`) + `serde` — a downstream decision crate, not an upstream in the firewall rules (QE-132
  guard unaffected). It does not depend on `qe-report` (both are parallel consumers of the same
  diagnostics).

## Design

### D1 — The embargoed holdout split

`split_with_embargo(n, holdout_len, embargo) -> Holdout { train, embargo, holdout }` carves the dataset
into three contiguous, **disjoint** time blocks: `train = 0..t`, `embargo = t..h` (a purged gap belonging
to neither), `holdout = h..n` where `h = n − holdout_len`, `t = h − embargo`. The holdout is the **final**
time-blocked slice and never overlaps train; the embargo gap prevents look-ahead leakage across the
boundary (the same purge discipline as QE-113's CV). If `holdout_len + embargo ≥ n` the train range
clamps to empty (degenerate, but well-defined). This *maintains the untouched OOS slice* the AC requires;
the information-firewall (QE-132) and the data discipline together keep it untouched by training/selection.

### D2 — The four pre-registered G1 criteria

`evaluate_g1(in_sample_sharpe, holdout_returns, robustness, criteria) -> G1Decision` evaluates, on the
**holdout** (`holdout_sharpe = sharpe_ratio(holdout_returns)`):

1. **Net-of-cost edge persists** — `holdout_sharpe ≥ criteria.min_holdout_sharpe` (the edge is still there
   OOS, not just in-sample).
2. **DSR exceeds the deflated threshold** — `robustness.dsr > criteria.dsr_threshold` (the deflated Sharpe
   beats best-of-N data-snooping, QE-131).
3. **SPA beats the best-of-N null** — `robustness.spa_pvalue < criteria.spa_alpha` (significant vs the
   reality-check null at the stated level).
4. **OOS within tolerance of in-sample** — `holdout_sharpe ≥ in_sample_sharpe · (1 − oos_tolerance)` (the
   OOS performance has not collapsed relative to IS — the over-fit acceptance bound; expressed as a
   product, so no division / div-by-zero, and OOS *above* IS trivially passes).

`promoted = all four pass`. Each yields a `CriterionResult { name, passed, value, threshold }` so the
decision is **recorded with evidence** (serde). `G1Criteria::with_defaults()` supplies the pre-registered
values (`min_holdout_sharpe = 0.0`, `dsr_threshold = 0.95`, `spa_alpha = 0.05`, `oos_tolerance = 0.5`).

### D3 — The recorded decision

`G1Decision { promoted, criteria }` (serde) is the auditable record: the boolean promotion verdict and the
per-criterion value-vs-threshold evidence. `failed_criteria()` lists the blockers. Because it is `serde`,
it persists alongside the vintage/report.

## Module / API plan

New crate `crates/gate` (`qe-gate`), `[workspace.dependencies]`-registered:
- `Holdout { train, embargo, holdout }` + `split_with_embargo(n, holdout_len, embargo)`.
- `G1Criteria { min_holdout_sharpe, dsr_threshold, spa_alpha, oos_tolerance }` + `with_defaults()`.
- `CriterionResult { name, passed, value, threshold }`, `G1Decision { promoted, criteria }` +
  `failed_criteria()`, `evaluate_g1(...)`.
- Deps: `qe-validation`, `serde`; dev: `serde_json`.

## Test plan (TDD)

1. **A clean vintage promotes.** Strong holdout edge, DSR above threshold, SPA below alpha, OOS retaining
   IS ⇒ `promoted == true`, all four `CriterionResult.passed`.
2. **Each criterion blocks alone (AC).** Four cases, each flipping exactly one input below its threshold
   (holdout edge gone / DSR too low / SPA p too high / OOS collapsed vs IS) ⇒ `promoted == false`, with the
   *correct* criterion failed and the others still passing — proving "failing any criterion is not
   promoted".
3. **Evidence recorded.** `G1Decision` round-trips through serde; `failed_criteria()` names the blockers.
4. **`split_with_embargo`.** train/embargo/holdout are contiguous, disjoint, cover `0..n`, the holdout is
   the final `holdout_len`, the embargo is the `embargo` block before it; clamps when `holdout_len +
   embargo ≥ n`.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-gate`,
`cargo test --workspace`, `cargo deny check`.

## Risks

- **Pre-registered thresholds are policy, not law.** Defaults are documented constants the deployment owner
  sets once and freezes (pre-registration is the point — changing them post-hoc defeats G1). Recorded in
  `G1Criteria` so the decision evidence carries the exact thresholds used.
- **The gate judges evidence; it can't *prove* the holdout was untouched.** That guarantee comes from the
  data-split discipline (D1) + the information firewall (QE-132). G1 assumes the caller evaluates the
  vintage on the `split_with_embargo` holdout and supplies IS metrics from train only; the orchestration
  (QE-209) wires this, and the firewall guard prevents code-level leakage.
- **Criterion 4 is one-sided** (penalises OOS *below* IS, not above) — intentional: out-performing OOS is
  not over-fit. Expressed as a product to avoid a div-by-zero when `in_sample_sharpe` is small.
