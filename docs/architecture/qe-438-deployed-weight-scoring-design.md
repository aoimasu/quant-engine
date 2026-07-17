# QE-438 — Score the DE membership objective on the deployed (capacity-capped) weight vector

`Phase: Review R2 (P2 — panel #9, majority)` · `Area: ensemble` · `Depends on: QE-115, QE-130`
· Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-438`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog: [`docs/backlog.md`](../backlog.md) → Review R2.b

## Problem (the optimize-X-deploy-Y gap)

The discrete-DE ensemble search selects **membership** by maximising the QE-115 objective on the
**equal-weight** combined return series (`crates/ensemble/src/objective.rs:239` `combined_returns` —
`Σ r_i / k`). But the sealed vintage **deploys** that membership under **capacity-capped, non-1/N**
weights, computed *after* selection in `crates/cli/src/jobs/train.rs:684-727`
(`capacity_capped_weights` → `cap_or_equal` → `cap_weights` at `TARGET_AUM_USD = $1M`).

When a per-strategy capacity cap **binds** (a high-turnover / low-edge member caps out at, say, ~$100k
and is water-filled down to a fraction of its 1/N share, with cash possibly left uninvested), the
combined series the book actually runs is **not** the equal-weight series the search optimised. The
selected set is therefore no longer provably optimal for the portfolio that runs — the same
"select-X-deploy-Y" class as the execution-parity gap (QE-435). Whether it bites is a function of the
guessed impact coefficient (QE-431), but the fix is cheap and removes the need to argue how often caps
bind.

## Fix

Thread the **deployed weight vector** into the objective's combined-returns computation during scoring,
**reusing the `weighted_combined` already present in `crates/ensemble/src/stress.rs:134`** (QE-130). The
deployed weights are the QE-128 capacity water-fill (`cap_weights`, intra-crate) of the equal-weight
budget over the candidate membership's per-strategy capacities at the target AUM — the *exact* object
`train.rs` deploys. Because `cap_weights` on an equal-weight input returns the equal weights when **no**
cap binds, the deployed-weight objective **reduces to the equal-weight objective exactly when caps don't
bind** — the change is a no-op precisely in the regime where the old behaviour was already correct.

### Where the capacity data lives

`search_portfolio`/`objective` are capacity-blind today (`search.rs:254`, `objective.rs:295`): they see
only the raw return `pool`. Capacities are a **per-strategy** quantity (`gross_edge`, `turnover` →
`capacity()`), computable for the **whole pool** up front, independent of membership; only the water-fill
depends on which members are in the candidate set (and on the leave-one-out subset). So the search call
site (`train.rs:391`) computes `pool_capacities` for **all** elites and passes them in; the objective
recomputes the water-fill per candidate/LOO subset.

## Design (additive, backward-compatible, scoped)

`ObjectiveConfig`/`SearchConfig` are `#[derive(Copy)]` and carry no `Vec`; the capacity vector is
per-pool and must not live in a `Copy` config. So the deployed weighting is passed as a **separate
borrowed, `Copy` descriptor** rather than stored in config, and existing signatures are preserved by
delegation (regime.rs and every existing test stay byte-untouched):

```
crates/ensemble/src/objective.rs
  pub enum Weighting<'a> {                       // Copy: &[f64] + f64
      EqualWeight,
      CapacityCapped { capacities: &'a [f64], target_aum: f64 },
  }
  impl Weighting { pub fn member_weights(&self, members: &[usize]) -> Vec<f64> }
      // equal 1/N, then cap_weights(equal, caps[members], target_aum) for CapacityCapped

  combined_returns_weighted(pool, members, weighting)  // NEW — builds member series,
                                                       // reuses stress::weighted_combined
  combined_returns(pool, members)                      // = *_weighted(.., EqualWeight)  [byte-identical]
  objective_weighted(pool, members, cfg, weighting)    // NEW
  objective(pool, members, cfg)                        // = *_weighted(.., EqualWeight)
  leave_one_out_min_weighted / leave_one_out_min       // NEW / delegate

crates/ensemble/src/search.rs
  cross_val_score_weighted / cross_val_score           // NEW / delegate
  search_portfolio_weighted(pool, cfg, weighting, seed) / search_portfolio  // NEW / delegate

crates/cli/src/jobs/train.rs
  - keep the full BacktestResult for the elite pool; derive pool_capacities via an extracted
    `strategy_capacities(genomes, bts)` helper (also reused by capacity_capped_weights — one formula).
  - search_portfolio_weighted(&pool, &ens_cfg,
        Weighting::CapacityCapped { capacities: &pool_capacities, target_aum: TARGET_AUM_USD }, seed)
```

Invariants:
- **Equal-weight equivalence.** `member_weights` for `EqualWeight` is `[1/k; k]`; `weighted_combined`
  with those weights equals `combined_returns` (`Σ (1/k) r_i`, same shortest-series truncation), so
  `combined_returns` / `objective` / `search_portfolio` are byte-identical to pre-QE-438.
- **Reduction when caps don't bind.** `cap_weights(equal, caps, aum)` returns `[1/k; k]` when every
  `cap_i ≥ 1/k` share → `CapacityCapped` reduces to `EqualWeight`.
- **Correlation term unchanged.** The QE-430 deflated correlation penalty is a property of the member
  return series (scale-invariant, weight-independent) and stays on the raw member series — untouched, so
  QE-430 and QE-438 remain jointly correct on the shared `combined_returns` path.
- **Firewall.** `cap_weights`/`weighted_combined` are intra-`qe-ensemble`; no new dependency, no
  `qe-ensemble → qe-wfo` edge (search ⟂ portfolio firewall, QE-001/QE-132).

## Golden / vintage impact

Changing scoring from equal-weight to capacity-capped-weight can change which membership the DE selects
→ can change `chromosomes` + `weights` in a sealed vintage, which feed the `content_hash`
(`crates/vintage/src/lib.rs:131`, hashed fields `chromosomes`/`weights`). **However**, the committed
golden fixtures (`crates/cli/tests/fixtures/golden_result.json`, `sample_vintage.json`) are produced by
the **backtest** job loading a pre-sealed fixture vintage — **not** by the train/ensemble job. The train
job's guard is a same-seed **determinism** test (`crates/cli/tests/train_job.rs`,
`assert_eq!(hash_a, hash_b)`), which stays self-consistent because both runs use the new scoring. No
committed golden pins a specific ensemble membership hash to a literal string.

Therefore no `VINTAGE_FORMAT_VERSION` bump is required (no new hashed field is added) and no committed
golden is expected to move. If the full suite reveals any golden movement it will be regenerated via the
real `regenerate_fixtures` path (never hand-edited) and the before→after `content_hash` reported.

## Test plan (TDD)

`objective.rs`:
- `member_weights` reduces to `[1/k; k]` when caps don't bind (and on `target_aum ≤ 0`).
- `combined_returns_weighted` under `CapacityCapped` equals `weighted_combined(member_series, weights)`
  (explicit reuse), and equals `combined_returns` under `EqualWeight`.
- When caps **bind**: the scored combined and the objective differ from equal-weight.
- When caps **don't** bind: `objective_weighted(CapacityCapped) == objective(EqualWeight)` to 1e-12.
- **Ranking flip** (the inconsistency, closed): a pool where equal-weight ranks `{A,B} > {B,C}` but the
  deployed capacity-capped weighting ranks `{B,C} > {A,B}` — the search now optimises the deployed object.

`search.rs`:
- `search_portfolio_weighted(CapacityCapped, non-binding caps) == search_portfolio` (reduction).
- With binding caps the converged score reflects the deployed (diluted) combined — differs from the
  equal-weight search score; determinism preserved.
