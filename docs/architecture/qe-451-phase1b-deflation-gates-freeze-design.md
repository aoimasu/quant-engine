# Design Note QE-451 — Phase 1b: GP-aware deflation + tradability gates + formula-pool freeze

*Completes QE-451. Delivers the deflation/gate/freeze **engine** (QE-450 §5, §4.6, §9). Default-off — no
golden moves, `CATALOGUE_VERSION` unchanged, default vintage byte-identical.*

> **Status:** Implementation note (this ticket). Spec of record:
> [`qe-450-gp-indicator-evolution-design.md`](./qe-450-gp-indicator-evolution-design.md) §5 (overfitting
> controls — the backbone), §4.6 (fitness + tradability gates), §8 (prerequisites), §9 (Phase 1b row),
> §12 (dissents). Builds on merged Phase 0 (#152) + Phase 1a (#153).

`Area: search / signal (qe-wfo, qe-validation, qe-signal, qe-ensemble)` · `Reuses (merged): QE-439, QE-434, QE-436, QE-430, QE-440, QE-431, QE-441`

---

## 1. Scope of this ticket (Phase 1b only)

Phase 1a already illuminates a separate `Elite<ExprTree>` MAP-Elites archive under a trivial fixed head and
emits a **distinct-canonical trial count** into `PoolLineage`. Phase 1b makes that search **honestly
deflated + tradability-screened**, then **freezes** `K ≤ 16` trees into a content-addressed pool identity.

Everything here is **opt-in machinery exercised by tests** with a synthetic pool. **Nothing is wired into
the default `train`/`backtest`/`catalogue`/vintage pipeline.** With no GP pool sealed (default/empty), the
schema, `CatalogueIdentity`, and vintage are unchanged, so no golden moves and `CATALOGUE_VERSION` /
`VINTAGE_FORMAT_VERSION` are unchanged.

**Not built here — deferred (explicit):** the production-mode `DEFLATION_BASIS_VERSION` const gate, server
sealing, RBAC/audit, admin UI — that is the ops-safety ticket **QE-454** (design §13). Co-evolution is
**QE-452/Phase 2**. This ticket delivers only the deflation/gate/freeze **engine**.

## 2. Firewall placement (§6, non-negotiable)

- GP search / deflation / gates / freeze live in **`qe-wfo` + `qe-validation` + `qe-signal`**. The firewall
  (`crates/architecture/src/lib.rs::firewall_rules`) forbids `qe-wfo → qe-ensemble` and `qe-wfo → qe-runtime/venue`,
  but **permits `qe-wfo → qe-validation`** (validation reaches only `qe-determinism`). So the new `qe-wfo`
  dep on `qe-validation` is firewall-legal, and `cargo test -p qe-architecture --test firewall` stays green.
- **QE-430 provenance penalty (item 4)** cannot be an edge into `qe-ensemble` from `qe-wfo`. It is therefore
  implemented **inside `qe-ensemble`** (`objective.rs`), operating on member return series **+ a provenance
  label per member** — the evolved members cross as **DATA**, not a `qe-wfo → qe-ensemble` code edge.
- **Capacity (item 5c)** — `capacity.rs` lives in `qe-ensemble` (forbidden). Per design §4.6(c) the capacity
  floor is an **inlined `capacity()` in `qe-wfo` with impact coefficients duplicated from the shared
  `qe_risk::SlippageCalibration`** (single source of truth; `qe-wfo` already depends on `qe-risk`), guarded by
  a **coefficient-parity unit test** against the same calibration the `qe-ensemble` capacity derives from.
- The **freeze into the vintage** crosses as sealed **DATA** — `formula_hash` strings on `CatalogueIdentity`,
  not a `qe-wfo → qe-ensemble/qe-vintage` code edge.

## 3. Per-control implementation (each control → merged prerequisite reused)

| # | Control (design §) | Reuses (merged) | Where | Test |
|---|---|---|---|---|
| 1 | **GP-aware deflation basis** — `N = max(distinct-canonical, cells·gens·windows floor)`; **uncensored** dispersion/PBO over **every evaluated** formula; uncensored PBO **primary**, DSR necessary-not-sufficient | QE-439 `effective_trials`/`expected_max_sharpe_ln`; QE-414 `variance_returns`/`assess` | `wfo/gp/deflation.rs` | `gp_trial_basis` floors correctly; censored PBO ≤ uncensored; gate blocks on high uncensored PBO |
| 2 | **IC pre-screen** — purged rank-IC two-fold sign-consistency + BH-FDR; every screened (pass+fail) counts toward N | QE-434 `rank_ic`/`benjamini_hochberg`/`screen_catalogue` | `wfo/gp/gates.rs::ic_screen` | screen filters compute; N still counts rejects |
| 3 | **In-search MDL/parsimony rent** — `penalised = mean − (1/T)·[n_struct·ln(4·f_eff·t) + n_const·½·ln(T_eff)]`; hard caps already in `ExprTree::repair` | QE-436 caps in `expr.rs::repair` | `wfo/gp/deflation.rs::mdl_penalised_fitness` | rent shrinks with node count; caps enforced by repair |
| 4 | **Deflated + purged ensemble corr penalty** + leave-one-**PROVENANCE**-out floor + evolved-share cap | QE-430 `pairwise_corr_penalty`/`CorrDeflation` | `ensemble/objective.rs` (provenance-aware) | dropping a provenance cluster changes the floor; share cap rejects over-concentration |
| 5 | **Tradability gates at insertion** — (a) cost-stress `min{1×,2×}` net `log_growth` finite & >0; (b) max-turnover reject (avg hold ≥ 4h); (c) capacity floor ≥ `$250k`. Rejects still count toward N | QE-431 `cost_sweep`/`SlippageCalibration`; QE-440 capacity form | `wfo/gp/gates.rs::TradabilityGate` | each of cost/turnover/capacity genuinely blocks a formula; parity test |
| 6 | **Cross-asset pooled fitness** — effective-independent `T_eff` from **measured** cross-asset return correlation | new (design §4.6 / §7 risk 3) | `wfo/gp/deflation.rs::pooled_t_eff` | measured correlation raises `T_eff` toward `Σ·(1−ρ̄)` |
| 7 | **Label-shuffle / block-bootstrap null** — calibrate so shuffled-champion DSR ≈ 0.5 across node-size bands | extends QE-131 `nulls.rs` | `validation/nulls.rs` | headline honesty test (below) |
| 8 | **Freeze K ≤ 16** — canonical S-expression SHA-256 `formula_hash`; sealed vintage pool; load boundary asserts exact identity | QE-451 Phase 1a `canonical_hash`; QE-402 `CatalogueIdentity`/`assert_schema`; QE-444 `skip_serializing_if` | `wfo/gp/freeze.rs` + `signal/feature.rs` | stable hash; load rejects mismatch; **default empty pool byte-identical** |

### Deflation basis details (item 1)

`assess(VintageStats, …)` (QE-131/D6, merged) already separates the three populations exactly as Phase 1b
needs: `n_trials` (the GP-aware basis), `variance_returns` (the **uncensored** dispersion population whose
cross-trial Sharpe variance sets `E[max SR]`), and `trial_returns` (the CSCV/PBO columns). Phase 1b's
`build_uncensored_stats` feeds the **return series of every evaluated formula** (not just archive
champions) into **both** `variance_returns` and `trial_returns`, so **uncensored PBO** is computed over the
full evaluated population — the design's primary GP gate. `gp_trial_basis(distinct_evaluations, cells,
gens, windows) = max(distinct_evaluations, effective_trials(cells,gens,windows))` feeds `n_trials` through
QE-439's finite-at-large-N `expected_max_sharpe_ln` path. **Errs conservative:** the floor never
under-counts; over-counting raises the bar ⇒ over-deflate/false-reject (safe), never under-deflate.

**DSR-honest basis = `max(distinct, N*)`, NOT the analytic floor alone (carry-forward to QE-454).** The
analytic `max(distinct, cells·gens·windows)` basis is honest for the **PBO/dispersion** axis but can
*under*-deflate the **DSR** axis: the empirical best-of-N in-sample Sharpe is heavier-tailed than the
parametric Gumbel `E[max SR]`, so a label-shuffled champion sits at DSR `> 0.7` on the raw basis and only
reaches ≈ 0.5 once deflated against the **shuffle-null-calibrated `N* ≥ P`** (`calibrate_null_basis`). Both
`gp_trial_basis` and `assess_gp_champion` are **basis-agnostic** (they deflate against whatever `N` the
caller supplies), and **nothing auto-applies `N*` yet** — the **QE-454 production seal MUST fold `N*` in**
(deflate G1 against `max(distinct, N*)`); sealing on the analytic floor alone would ship the under-deflated
DSR. This is the load-bearing carry-forward.

### Freeze + default-byte-identical (item 8)

`CatalogueIdentity` gains one field:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub formula_pool: Vec<String>,   // canonical S-expression SHA-256 per frozen tree, K ≤ 16, empty by default
```

Mirrors the QE-444 precedent (`skip_serializing_if = "Decimal::is_zero"`): an **empty** pool is omitted from
the serialised JSON, so `CatalogueIdentity::current()` (empty) and every default vintage's `content_hash`
are **byte-identical** to before — no golden moves, `VINTAGE_FORMAT_VERSION` unchanged. A sealed **non-empty**
pool serialises the sorted hash list; the load boundary `assert_schema` already compares the full
`CatalogueIdentity`, so a pool mismatch is rejected exactly like a catalogue reorder. Freezing a non-empty
pool would move that vintage's hash — but that path is exercised **only in tests / behind the opt-in**, never
in the default `train` vintage. `formula_hash` = SHA-256 over the canonical S-expression (exact,
`rust_decimal` render, no `f64`), reusing Phase 1a `ExprTree::canonical_hash`.

## 4. Shuffle-null calibration (the headline honesty result)

`nulls.rs` gains `label_shuffle_returns(signal_aligned_returns, seed)` (DetRng-seeded permutation of the
signal→forward-return alignment, destroying predictive structure while preserving the return marginal) and
`block_bootstrap_returns(returns, block_len, seed)` (moving-block resample preserving short-range
autocorrelation). Calibration target (design §5 κ-null row, AC 5): on a label-shuffled null, the
**evolved-champion DSR sits at ≈ 0.5 across node-size bands** — i.e. once the trial basis counts how hard the
search rummaged, best-of-N noise no longer clears the deflation bar. The test
`shuffle_null_champion_dsr_is_near_half_across_node_bands` selects the max-Sharpe champion per node-size band
from a label-shuffled population and shows the two-step honesty result: (1) on the **raw** basis `N = P`
the champion DSR is **> 0.7** — the raw `max(distinct, analytic-floor)` basis *under-deflates* the DSR axis
(the empirical best-of-N Sharpe is heavier-tailed than the Gumbel `E[max SR]`); (2) `calibrate_null_basis`
finds `N* ≥ P` at which the shuffled-champion **DSR ∈ [0.45, 0.55] ≈ 0.5** for every band. **The honest DSR
basis is therefore `max(distinct, N*)`, and QE-454's seal must fold `N*` in** (see §3). This is the
non-vacuous proof the deflation is honest.

## 5. Determinism / constraints

- All randomness via `DetRng` / `task_rng` (shuffle null seeded + reproducible).
- `rust_decimal` wherever a value feeds a hash (`formula_hash` = exact canonical S-expression SHA-256).
- Deflation **errs conservative**: over-deflate/false-reject safe; under-deflate/false-accept never.
- Reuses merged QE-439/434/436/430/440/431 machinery — no reimplementation of DSR / IC / MDL caps /
  correlation penalty / capacity form / cost sweep.
- Default-off: no golden moves; `cargo test -p qe-architecture --test firewall` green.

## 6. Test plan (non-vacuous)

1. **Shuffle-null champion DSR ≈ 0.5 across node-size bands** (headline).
2. Uncensored PBO uses the **full evaluated population** (censored top-N inflates DSR — asserted lower PBO on
   uncensored vs a strict guard).
3. **N counts rejects** — a run with IC/cost/turnover/capacity rejects still reports the rejects in the basis.
4. A **cost-stress / turnover / capacity reject genuinely blocks** a formula (three separate tests).
5. Freeze produces a **stable `formula_hash`**; the load boundary **rejects a mismatch**.
6. **Default vintage byte-identical** (empty pool ⇒ `CatalogueIdentity::current()` and `content_hash`
   unchanged).
7. Coefficient-parity: inlined wfo capacity slippage == shared `SlippageCalibration` form.
8. Provenance floor: dropping a whole lineage cluster changes the leave-one-provenance-out penalty.

## 7. What completes QE-451 vs deferred

- **Completed by this ticket (Phase 1b):** GP-aware deflation basis + uncensored PBO/dispersion, IC
  pre-screen, in-search MDL rent, provenance-aware ensemble penalty, cost/turnover/capacity gates,
  cross-asset pooled `T_eff`, label-shuffle/block-bootstrap null (calibrated), K≤16 formula-pool freeze +
  identity + load boundary. **QE-451 is complete.**
- **Deferred:** QE-452 (co-evolution / Phase 2), QE-453/454 (production seal governance:
  `DEFLATION_BASIS_VERSION` const, server routes, RBAC, tamper-evident audit, admin UI — design §13).
