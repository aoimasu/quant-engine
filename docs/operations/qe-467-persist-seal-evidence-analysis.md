# QE-467 — Persist seal evidence + net-of-cost holdout series + provenance analysis (R1 foundation)

*Evidence note written before implementation. Anchors are real `file:line` citations at the time of writing.*

## 1. Goal (from `docs/mds/tickets/QE-467.md` + `qe-455-research-flow-design.md` §4.1/§10)

Persist, in **exactly one** `VINTAGE_FORMAT_VERSION` 7→8 bump, into the sealed `VintageContent`:

1. **Full seal evidence** (content-addressed): the tradability + deflation outputs the gate produces —
   DSR/SPA/PBO (already on `GateSnapshot`), plus IC/FDR (QE-434), cost-stress `min{1×,2×}` net
   (QE-431/450 §4.6a), realised turnover, and `capacity_usd` (QE-431/440).
2. **A canonical net-of-cost holdout return series on the DEPLOYED capacity-capped weights** (QE-438),
   content-addressed and addressable by a stable handle — never gross / equal-weight / lone-Sharpe.
3. **Hashed `data_provenance ∈ {real,synthetic,mixed}` + the extended lineage** the research flow needs:
   holdout split `{holdout_range, embargo, train_range}`, holdout regime composition (QE-125),
   per-holdout consultation count (QE-460, overlap-keyed), and the steer delta (QE-458). The **whole**
   schema is defined here; downstream tickets **populate** the deferred fields.

Downstream reads (never recomputes) this. No change to `evaluate_g1`, the seal predicate, gate
thresholds; no `PROTOCOL_VERSION` bump; no new `qe-vintage → qe-wfo`/`qe-ensemble` code edge.

## 2. Current-state evidence

- `crates/vintage/src/lib.rs:41` — `pub const VINTAGE_FORMAT_VERSION: u16 = 7;`
- `crates/vintage/src/lib.rs:44` — `struct VintageContent { format_version, vintage_id, chromosomes,
  weights, calibration, slippage, sizer, shocks, worst_case_loss, catalogue, lineage }`. The content
  hash is SHA-256 over canonical JSON (`content_hash`, `lib.rs:146`); every embedded type must serialise
  deterministically (no `HashMap`).
- `crates/vintage/src/lib.rs:532` — `assert_eq!(VINTAGE_FORMAT_VERSION, 7, …)` in `shocks_are_part_of_the_hash`.
- `crates/determinism/src/lineage.rs:26` — `Lineage { config_hash, input_snapshot_id, code_commit, seeds }`.
  No provenance field. Used widely; kept **untouched** (we add a sibling block on `VintageContent`, which
  the ticket explicitly permits: "extending `Lineage` **or a sibling lineage block on `VintageContent`**").
- `crates/server/src/runs/model.rs:150` — `GateSnapshot { promoted, failed, in_sample_sharpe,
  holdout_sharpe, dsr, spa_pvalue, n_trials, uncensored_pbo?, variance_trials?, distinct_evaluations? }`.
  Lives on `meta.json` only — thrown away at seal.
- Real seal path: `crates/cli/src/jobs/train.rs:556` builds `VintageContent`. At that point these are in
  scope: `robustness` (`qe_validation::RobustnessReport` with `dsr`, `pbo`, `spa_pvalue`, `n_trials`,
  `crates/validation/src/lib.rs:109`), `weights` (deployed capacity-capped, `train.rs:463`),
  `holdout_bars`/`holdout_returns` (`train.rs:480`), `strategy_capacities` (per-member $ capacity,
  `train.rs:759`), `train_adv_notional` (`train.rs:412`), `combine` (deployed-weight net-of-cost series,
  `train.rs:707`), `hash_stable` (`train.rs:94`).
- Cost-stress lever: `FrictionConfig::with_multiplier` (`crates/wfo/src/friction.rs:199`) +
  `BacktestConfig.friction.cost_multiplier` (`backtest.rs:55`, applied at `backtest.rs:380`). So the
  deployed ensemble can be re-priced at `2×` friction by combining with a scaled `BacktestConfig`.
- Committed sealed fixtures that MUST be regenerated (hash-verified on load, so the 7→8 bump breaks them):
  `crates/cli/tests/fixtures/sample_vintage.json` (regen via existing `#[ignore] regenerate_fixtures`,
  `backtest_job.rs:299`) and `crates/server/tests/fixtures/sample_vintage.json` (no generator today — add
  an `#[ignore]` regen test). Server asserts only structural props (`read.rs:107`: id/label/chromosomes≥1/
  64-hex hash/format_version is u64) + valid seal (`audit_governance.rs:434`).
- All `VintageContent` struct-literal sites to extend (10): `lib.rs:431`, `schema.rs:138`,
  `backtest_job.rs:188`, `train.rs:556`, `hedger/{vintage_rollover.rs:187, bootstrap.rs:339,
  cutover.rs:383, evaluator.rs:224}`, `runtime/tests/{cutover_breaker_wiring.rs:119, restart_parity.rs:102}`.
- Firewall: `crates/architecture/tests/firewall.rs` builds the real dep graph; `qe-vintage` deps are
  `qe-signal, qe-risk, qe-determinism, serde, serde_json, sha2, thiserror` — no `qe-wfo`/`qe-ensemble`.
  New types are pure data (f64 / String / enum) so no new edge is introduced.

## 3. Implementation decisions

New **pure-data** types in `crates/vintage/src/lib.rs` (no train-side deps → firewall safe):

- `SealEvidence` (content-addressed, on `VintageContent.seal_evidence`):
  `dsr`, `pbo`, `spa_pvalue` (f64), `n_trials` (u64), `realised_turnover`, `capacity_usd` (f64),
  `cost_stress_net_min: Option<f64>`, `uncensored_pbo: Option<f64>`, `ic: Option<f64>`,
  `fdr: Option<f64>`. `Default`. Options use `skip_serializing_if` (matches `GateSnapshot`).
- `HoldoutReturnSeries { returns: Vec<f64> }` on `VintageContent.holdout_series`, with
  `handle() -> Result<String, VintageError>` (SHA-256 over the series' canonical JSON) — the stable
  ref the detail endpoint (QE-456) returns instead of a re-run. Finiteness validated at seal.
- `ResearchProvenance` on `VintageContent.provenance` (the "sibling lineage block"):
  `data_provenance: DataProvenance` (`real|synthetic|mixed`, `serde(rename_all="lowercase")`),
  `holdout_split: HoldoutSplit { holdout_range, train_range: Option<TimeRange>, embargo_bars: u64 }`,
  `regime_composition: Vec<RegimeShare { regime: String, bars: u64 }>`,
  `consultation_count: u64`, `steer_delta: Option<SteerDelta { indicator_subset_hash: String,
  generations, population, windows, folds: u64 }>`.

**Population on the real train seal path (`train.rs`), gate outputs only — no gate change:**
- `dsr/pbo/spa_pvalue/n_trials` ← `robustness` (already computed).
- `realised_turnover` ← weighted member turnover `Σ wᵢ·(tradesᵢ·2·size_fracᵢ / n)` — the exact
  turnover the sealed capacity model (`strategy_capacities`) already uses. `hash_stable`-rounded.
- `capacity_usd` ← `Σ` selected-member `strategy_capacities` (the deployed book's modelled $ capacity at
  `TARGET_AUM_USD`). `hash_stable`-rounded.
- `cost_stress_net_min` ← `Some(min(net_1×, net_2×))` where `net_m×` is the deployed ensemble's total
  net-of-cost return over the holdout, re-priced by scaling `BacktestConfig.friction.cost_multiplier`.
  `hash_stable`-rounded. (Reuses `combine`; does not touch `evaluate_g1`.)
- `holdout_series` ← `combine(chromosomes, weights, holdout_bars, cfg)` (the DEPLOYED weights),
  each return `hash_stable`-rounded so it round-trips byte-identically (same rule as `weights`).
- `data_provenance` ← `Real` on the train path (deterministic offline train over a real/loaded store).

**Deferred (schema defined here, populated by downstream tickets; default/empty at seal now):**
- `ic`, `fdr` — `None`. Rationale: IC/FDR (QE-434, `crates/validation/src/ic.rs`) is a **per-indicator
  factor-admission screen**, not a scalar the ensemble train gate computes; synthesising one here would
  be *new* evidence, which "persist the gate's own outputs only" forbids. Slot defined + hashed; the
  IC-screen/evolve path (or QE-458) populates it. Parallels `uncensored_pbo` being GP-only/`None` on the
  train path today (`train.rs:530`). **Flagged as a product-scoping decision in the return.**
- `holdout_split`, `regime_composition`, `consultation_count` — default/empty (QE-460 populates, §4).
- `steer_delta` — `None` (QE-458 populates).

**Single bump / golden safety:** `VINTAGE_FORMAT_VERSION` 7→8; add the `8` doc row; `== 7` → `== 8` at
`lib.rs:532`; regenerate BOTH committed `sample_vintage.json` via the real seal path (update the literal,
run the `#[ignore]` regen tests), plus `golden_result.json` (cli). No unrelated hash drift — only the new
fields + the version move the id.

## 4. Test plan (TDD / prove-it)

In `crates/vintage/src/lib.rs` tests:
- `seal_evidence_is_part_of_the_hash` — changing a `SealEvidence` figure changes the vintage id; round-trips.
- `holdout_series_is_part_of_the_hash_and_addressable` — a different series changes the id; `handle()` is
  64-hex and stable; non-finite series rejected at seal.
- `provenance_is_part_of_the_hash` — flipping `data_provenance` real→synthetic changes the id;
  the deferred fields round-trip; `steer_delta`/split populate-and-round-trip (proving downstream can write).
- Update `format_version_is_part_of_the_hash` / `shocks_are_part_of_the_hash` assertion to `8`.

In `crates/cli/src/jobs/train.rs` tests: a train run seals a vintage whose `seal_evidence.capacity_usd`,
`realised_turnover`, `cost_stress_net_min` are finite and the `holdout_series` length equals the holdout
combine length; `data_provenance == Real`.

Regen + determinism: rerun cli `regenerate_fixtures` + the new server regen; then the full gate incl.
QE-006 determinism harness and the `firewall` test must be green with no unrelated drift.

## 5. Risks

- **Golden churn beyond the intended fields.** Mitigated: only the new fields + version bump feed the
  hash; all newly-persisted f64s are `hash_stable`-rounded (same rule the existing weights use), so the
  regenerated goldens move only intentionally. Verified by re-running the determinism harness.
- **Firewall regression.** New types are pure data; no `qe-wfo`/`qe-ensemble` import added to `qe-vintage`.
  The `firewall` test guards this.
- **Deferred-field ambiguity (IC/FDR).** Documented above and flagged in the return as a product decision.
- **10 construction sites.** Mechanical; new types get `Default` so test sites use default constructors.
