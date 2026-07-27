# QE-469 — CPCV-distributed OOS evidence: current-state evidence, decisions, test plan, risks

> **Ticket:** `docs/mds/tickets/QE-469.md` (R3 — AFML panel #2). **Depends on:** QE-113 (purged CV),
> QE-439 (DSR), QE-467 (persisted seal evidence); pairs with QE-468. **Spec ref:** López de Prado
> Ch. 12.4 p. 163 (CPCV); reuses CSCV (`pbo.rs`) + `PurgedKFold` (`cv.rs`).

## 1. Problem (from the spec)

The out-of-sample verdict rides on **one** path. The **G1 terminal holdout** "remains the **only true
OOS gate** in the pipeline" (`crates/wfo/src/cv_fitness.rs:34-35`) — a single train/holdout split
producing a single OOS number. Ch. 12 of *Advances in Financial Machine Learning* prescribes
**Combinatorial Purged CV (CPCV)**: over the `C(S, S/2)` balanced block partitions, hold out `S/2`
blocks at a time to obtain a **distribution** of held-out Sharpe/DSR rather than a point estimate. The
hard machinery already exists — this ticket is a **reuse-and-emit**, not new leakage-control code.

## 2. Current-state evidence (file:line)

- **The single OOS gate** — `crates/wfo/src/cv_fitness.rs:34-35`: the module explicitly documents that
  the in-window purged k-fold is *not* held-out and that the G1 terminal holdout is the only true OOS
  gate. That is the point estimate this ticket surrounds with a distribution.
- **CSCV block-partition enumeration** — `crates/validation/src/pbo.rs:44-50,117-141`: `pbo_cscv` splits
  the time axis into `S` contiguous blocks (`bounds`) and iterates every balanced `C(S, S/2)` partition
  via the private `combinations(n, k)` odometer. **Reused verbatim** — I expose `combinations` as
  `pub(crate)` and consume it from the new `cpcv` module (same crate, zero reimplementation).
- **Purge + embargo primitives** — `crates/wfo/src/cv.rs:35-99`: `PurgedKFold` with
  `purge = lookback + label_horizon`, default `embargo = lookback`, the per-block exclusion
  (`excl_lo = t_start − purge`, `excl_hi = t_end + purge + embargo`), and the `Fold::windows_disjoint`
  invariant (`|tr − te| > lookback + label_horizon`). This is the exact arithmetic CPCV must apply per
  held-out block.
- **DSR / Sharpe primitives** — `crates/validation/src/dsr.rs:39,74,146` (`probabilistic_sharpe_ratio`,
  `expected_max_sharpe`, `deflated_sharpe_ratio`), `stats.rs` `sharpe_ratio`. The distribution reducer
  calls these per path — no new statistics.
- **Fail-closed gate posture** — `crates/wfo/src/gp/deflation.rs:196-203`: `GpDeflationGate::passes`
  returns `false` when the PBO could not be estimated (`None`) — the mirror for "under-powered ⇒ reject".
- **Persisted seal evidence** — `crates/vintage/src/lib.rs:61-93` `SealEvidence`, carried on
  `VintageContent::seal_evidence` (`:299-303`), hashed into the vintage id (`content_hash`, `:401`). The
  `Option` fields `cost_stress_net_min` / `uncensored_pbo` / `ic` / `fdr` were added (QE-454) as
  `#[serde(default, skip_serializing_if = "Option::is_none")]` slots **without a version bump** — the
  precedent this ticket follows.
- **Real seal path** — `crates/cli/src/jobs/train.rs:903-917` builds `SealEvidence`; `:816`
  `variance_returns = cell_champion_returns(...)` (the DSR dispersion population), `:772` `in_sample_returns`
  (the deployed ensemble's combined net-of-cost train series), `:807` `n_trials`. These are exactly the
  inputs the CPCV summary needs at seal time.
- **Determinism** — `crates/determinism/src/rng.rs:41` `task_rng(master, index)`. CPCV path **geometry**
  is RNG-free (lexicographic partition enumeration + arithmetic purge), so determinism is *structural*;
  the `task_rng` scheme is threaded for any future per-path stochastic reduction (none needed for the
  analytic DSR reduction here).
- **Golden coupling** — `crates/cli/tests/fixtures/golden_result.json` embeds the sample vintage's
  `content_hash` + `format_version: "8"`, produced from `SealEvidence::default()`
  (`crates/cli/tests/backtest_job.rs:207`). `sample_vintage.json` (cli + server) is likewise version 8.

## 3. The path-count formula (López de Prado Ch. 12.4)

Split the series into `S` contiguous blocks (groups). Each CPCV split holds out `k = S/2` blocks and
trains on the other `S/2`. The number of balanced splits — the number of held-out configurations that
form the distribution — is

```text
n_splits = C(S, S/2)            (the CSCV partition count reused from pbo.rs)
```

and the number of distinct full-length backtest **paths** reconstructable from them (Ch. 12.4, p. 163) is

```text
φ(S) = C(S, S/2) · (S/2) / S = C(S − 1, S/2 − 1)
```

because each of the `S` blocks serves as a test block in `C(S−1, S/2−1)` splits, and those predictions
tile into `φ` full-length paths. Examples: `S=4 ⇒ n_splits=6, φ=3`; `S=6 ⇒ n_splits=20, φ=10`;
`S=2 ⇒ n_splits=2, φ=1`. So `S ≥ 4` guarantees `φ ≥ 2` and `n_splits ≥ 6`.

**Fixed-candidate note.** Classic CPCV re-fits per train set, so the `φ` paths differ. Here the candidate
is the *already-selected* deployed ensemble (no per-split refit), so a block's held-out return is
candidate-fixed and the `φ` reconstructed full-length paths coincide. The *informative* multiplicity is
therefore the `n_splits = C(S, S/2)` **held-out configurations** — each concatenates the candidate's
net-of-cost returns over its `S/2` held-out blocks into one leak-free series → one Sharpe + one DSR. The
distribution is taken over those `C(S, S/2)` held-out Sharpes/DSRs. Both counts are exported
(`balanced_partition_count`, `cpcv_path_count`) and documented so the reuse is auditable.

## 4. Reuse plan (`pbo.rs` + `cv.rs`) and the crate-firewall decision

`qe-wfo` already depends on `qe-validation` (`crates/wfo/src/gp/deflation.rs:14-17` imports
`qe_validation::…`), so `qe-validation` **cannot** depend on `qe-wfo` — importing the `PurgedKFold`
*type* into the new validation module would be a dependency cycle. The ticket places the generator "in
`crates/validation`, sibling to `pbo_cscv`", so:

- **Enumeration:** reuse `pbo.rs::combinations` **verbatim** (exposed `pub(crate)`), same crate.
- **Purge + embargo:** reuse the **exact arithmetic** of `PurgedKFold` (`purge = lookback + label_horizon`,
  `embargo = lookback`, `excl = [start − purge, end + purge + embargo)`) applied per held-out block. It is
  **not** re-derived: it is the same one-line span, and a **cross-crate equivalence test** in
  `crates/wfo` (which *can* see both types) pins that the validation CPCV single-block geometry agrees
  with `PurgedKFold::folds` on a shared config — so "reuse `PurgedKFold` as-is" is proven, not asserted.
- **Statistics:** reuse `sharpe_ratio` + `deflated_sharpe_ratio` per path.

This is the only firewall-respecting realisation of "reuse both, don't reinvent".

### Module shape (`crates/validation/src/cpcv.rs`)

- `cpcv_path_count(S) -> C(S−1, S/2−1)`; `balanced_partition_count(S) -> C(S, S/2)`.
- `CpcvPath { test: Vec<Range<usize>>, train: Vec<usize> }` with
  `windows_disjoint(lookback, label_horizon)` mirroring `Fold::windows_disjoint`.
- `cpcv_paths(n_obs, blocks, lookback, label_horizon, embargo) -> Result<Vec<CpcvPath>, ValidationError>`
  — contiguous `S`-block bounds, one path per balanced partition (held-out = the chosen `S/2` blocks;
  train = every obs outside every held-out block's purge+embargo exclusion). Errors `OddBlockCount` for
  `S` odd/`<2`, `EmptyMatrix` for `n_obs < S`.
- `path_returns(candidate, &CpcvPath) -> Vec<f64>` — concatenate the candidate's per-period returns over
  the path's held-out ranges in time order.
- `CpcvDistribution::from_path_returns(&[Vec<f64>], trial_variance, n_trials, dsr_floor)` — per-path
  Sharpe + DSR vectors and the summary: `median_sharpe`, `sharpe_iqr` (25th/75th), `sharpe_p05/p95`,
  `median_dsr`, `dsr_p05` (the lower percentile the gate reads), `frac_dsr_ge_floor`, `n_paths`.
- `CpcvGate { min_paths, dsr_percentile, dsr_floor }` (default `4, 0.05, 0.95`): `passes` requires
  `n_paths ≥ min_paths` **and** `percentile(dsrs, dsr_percentile) ≥ dsr_floor` — **fails closed** on an
  under-powered / degenerate distribution (too few paths ⇒ reject; the lower percentile, not the mean,
  must clear the floor).

Percentiles use a deterministic sorted-vector linear-interpolation estimator (no RNG, exact f64 order).

## 5. Seal-evidence / version decision

**No `VINTAGE_FORMAT_VERSION` bump.** Following the QE-454 precedent, the summary rides a new
`SealEvidence.cpcv: Option<CpcvSummary>` field marked `#[serde(default, skip_serializing_if =
"Option::is_none")]`. Every existing artefact (all fixtures, the `SealEvidence::default()` sample vintage)
has `cpcv = None` ⇒ the field is **omitted** ⇒ their serialised bytes and `content_hash` are
byte-identical ⇒ **`golden_result.json` / `sample_vintage.json` do not move**, `format_version` stays 8,
the QE-006 determinism harness sees no drift, and the `assert_eq!(VINTAGE_FORMAT_VERSION, 8)` assertion is
untouched. The field is still **content-addressed** — when the real train seal populates it, it enters
`VintageContent`'s hashed JSON and changes that vintage's id (proven by a new `cpcv_is_part_of_the_hash`
test). `CpcvSummary` is a plain serde struct **defined in `qe-vintage`** (no `qe-validation` dep on the
runtime-facing vintage crate); the CLI maps `qe_validation::CpcvDistribution → qe_vintage::CpcvSummary`.
It carries the per-path Sharpe vector (rounded hash-stable, like `weights`) plus the summary stats and is
finiteness-checked in `VintageContent::validate` (same round-trip guard as the other evidence figures).

The single terminal-holdout figure (`holdout_sharpe` in the sidecar, `holdout_series` in the seal) is
**retained**; CPCV is added **alongside** it as the promotion-facing OOS evidence.

## 6. Wiring the live promotion verdict (`train.rs`) — the fail-closed gate

Immediately after `evaluate_g1`, compute the CPCV distribution over the deployed ensemble's
`in_sample_returns` (its combined net-of-cost train series), split into `DEFAULT_CPCV_BLOCKS = 6` blocks
with `lookback = schema.max_lookback()`, `label_horizon = DEFAULT_LABEL_HORIZON = 1`, `embargo = lookback`,
`trial_variance` from the existing `variance_returns` cell-champion population, `n_trials`, floor `0.95` —
so each held-out path deflates against the **same basis the G1 DSR used**.

**The CPCV gate decides (AC #4, B1).** The distribution is a **real** promotion criterion, not just
recorded evidence: an auditable `CriterionResult` `"cpcv_oos_distribution_clears_floor"` (value = the lower
`dsr_percentile` of held-out DSR, threshold = `0.95`) is appended to `g1.criteria`, and the verdict is
conjoined — `g1.promoted = g1.promoted && CpcvGate::default().passes(&dist)`. This keeps PBO primary and
the point-estimate criteria intact while making CPCV an additional hard conjunct. **Fail-closed on
under-power:** `CpcvDistribution::build → Err` (series too short for 6 purged blocks) or
`n_paths < min_paths` makes `cpcv_gate_pass == false`, which **flips the verdict to rejected** and records a
failed criterion — never default-accept. The same `dist` is mapped to the sealed `CpcvSummary`
(`cpcv = None` exactly when under-powered), all figures rounded via `hash_stable` so the seal round-trips
byte-identically and two same-seed runs are byte-identical (train_job determinism test). PBO
(`robustness.pbo`) is **unchanged** — CPCV is the OOS distribution *alongside* PBO, not a replacement.

## 7. Test plan (TDD)

`crates/validation/src/cpcv.rs`:
- `path_count_matches_lopez_de_prado_formula` — `cpcv_path_count`/`balanced_partition_count` for `S ∈
  {2,4,6,8}` equal `C(S−1,S/2−1)` / `C(S,S/2)`; `S=4 ⇒ φ=3, n_splits=6` (≥2 paths).
- `cpcv_paths_are_window_disjoint_under_purge_embargo` — **the leakage AC**: every path's held-out
  ranges are `windows_disjoint(lookback, label_horizon)` from its own train indices (mirror of the
  `PurgedKFold` invariant); a no-purge control leaks (non-vacuous).
- `cpcv_paths_reject_odd_or_too_few_blocks` — `OddBlockCount` / `EmptyMatrix` fail-closed.
- `distribution_summarises_median_iqr_percentiles_and_dsr_floor_fraction` — on a constructed per-path
  Sharpe set, median/IQR/p05/p95 and `frac_dsr_ge_floor` are correct.
- `gate_decides_on_lower_percentile_and_fails_closed` — a distribution whose *median* clears 0.95 but
  whose 5th-percentile DSR does not is **rejected**; too-few-paths is **rejected**; a uniformly-strong
  distribution passes.
- `cpcv_path_set_is_deterministic` — two `cpcv_paths` calls with identical inputs are byte-identical.

`crates/vintage/src/lib.rs`:
- `cpcv_is_part_of_the_hash_and_round_trips` — populating `cpcv` moves the `content_hash`, round-trips
  through disk verify, and a non-finite summary figure is rejected at seal.
- `default_cpcv_is_absent_and_keeps_the_hash` — `SealEvidence::default()` (cpcv `None`) serialises
  identically to pre-QE-469 (the golden-safety guarantee) and `VINTAGE_FORMAT_VERSION` is still 8.

`crates/wfo` (cross-crate reuse proof):
- `validation_cpcv_purge_matches_purged_kfold` — the validation CPCV single-held-out-block geometry
  equals `PurgedKFold::folds` train-exclusion on a shared `(lookback, label_horizon, embargo, n_obs)`.

`crates/cli/tests/train_job.rs`:
- the seal test asserts the sealed vintage carries a 20-path CPCV distribution (`C(6,3)`) with
  `dsr_p05 ≤ median_dsr` and a recorded `"cpcv_oos_distribution_clears_floor"` criterion (7 criteria total);
  the same-seed determinism test proves the populated `cpcv` seals byte-identically.
- **`cpcv_gate_is_wired_into_the_live_promotion_verdict_and_fails_closed` (B1)** — the LIVE promotion path:
  (wiring) `g1.promoted == (all point-estimate criteria pass) && (CPCV gate passes)`, so any CPCV failure
  blocks promotion even when the terminal-holdout point estimate would pass; (fail-closed) an under-powered
  run (`holdout = 115` ⇒ tiny train window ⇒ absent distribution) yields `seal_evidence.cpcv == None`, a
  failed CPCV criterion, and `promoted == false` — rejected, not sealed-and-promoted by default.

## 8. Risks / rollback

- **No golden drift by construction** (§5): the `Option`+`skip_serializing_if` field leaves every
  `None`-bearing artefact byte-identical. The only new content is the train seal's populated summary,
  whose golden is determinism-only (two-runs-equal), not a committed hash. Rollback = revert the branch.
- **Under-powered seal windows.** A short train series can't fill 6 purged blocks ⇒ degenerate
  distribution. Handled by fail-closed gate semantics; the summary records `n_paths` so the under-power is
  visible, never silently accepted.
- **Firewall (§4).** The purge arithmetic is *mirrored*, not imported, to respect wfo→validation. The
  cross-crate equivalence test makes the mirror provably identical to `PurgedKFold`, so it cannot silently
  diverge from the purge/embargo arithmetic the ticket forbids changing.
- **Lint discipline.** No `unwrap`/`expect`/`panic!` outside `#[cfg(test)]`; percentile/summary guards
  return neutral values (never NaN) on empty/degenerate input; money paths untouched (CPCV is `f64`
  statistics); the summary vector is a `Vec<f64>` rounded `hash_stable` for a stable content hash.
</content>
