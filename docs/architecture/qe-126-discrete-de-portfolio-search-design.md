# QE-126 — Discrete DE portfolio search — design note

`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-115, QE-123`
`Branch: qe-126/discrete-de-portfolio-search`

> Note: reconstructed on the QE-127 branch — the QE-126 branch's archive commit accidentally staged only
> code, so this design note was lost on the post-merge reset. PR #40 merged correctly (commit `d92eba2`).

## Goal (from backlog)

Assembles ensembles from the strategy pool with a tail-aware, wide-basin objective.

- Implement discrete DE per QE-115; fold cross-validation; net-of-cost candidate scoring.

**Acceptance criteria.**
- [x] DE converges to robust-basin portfolios on a fixture; scoring is net-of-cost.

**Out of scope.** Correlation/regime constraints (QE-127); capacity (QE-128).

## Current-state evidence

QE-115 (SPIKE) already provides the operators and the objective, so QE-126 is the **search loop**:
- `qe_ensemble::de` — `EnsembleMask` (binary inclusion mask), parameter-free `de_mutant` (`a XOR (b XOR
  c)`), and the seeded `binomial_crossover`. The module doc says the search *loop* is QE-126.
- `qe_ensemble::objective` — `objective(pool, members, cfg)` = `mean + tail_weight·CVaR − corr_weight·corr`
  on the **net-of-cost** member series, and `leave_one_out_min` (the QE-115/D6 wide-basin floor).
- The pool is the per-strategy net-of-cost return series; scoring on these is net-of-cost by construction.

`qe-ensemble` keeps its no-`qe-wfo` dep (QE-001/QE-132 search⟂portfolio firewall).

## Design

### D1 — DE/rand/1/bin over the binary mask

Classic DE, discrete variant, elitist (greedy) selection: initialise `pop_size` random masks (repaired to
≥ 1 member); each generation, for each target form `mutant = de_mutant(a,b,c)` over three distinct donors,
`trial = binomial_crossover(target, mutant, cr)`, and greedily replace iff `score(trial) ≥ score(target)`.
Greedy selection makes the best-so-far monotonic non-decreasing — the sense in which the search
"converges". One seeded `DetRng` ⇒ byte-deterministic.

### D2 — Fold cross-validation → robust-basin score (AC)

`cross_val_score` partitions the common time axis into `folds` contiguous folds, scores `leave_one_out_min`
within each fold, and takes the **minimum across folds** — the worst-fold, worst-member-dropped objective.
A portfolio only wins if it is robust across both time folds and removal of any single member: a robust
basin, not a sharp peak. `mean + tail_weight·CVaR` keeps it tail-aware.

### D3 — Net-of-cost scoring

The objective consumes the pool's net-of-cost return series directly (QE-115/D3); no gross path. The AC is
demonstrated by showing that subtracting a per-period cost strictly lowers the converged score.

## Module / API plan

New module `crates/ensemble/src/search.rs`:
- `SearchConfig { pop_size, generations, cr, folds, init_density, objective }`.
- `SearchResult { best, score, generations_run, history }`.
- `cross_val_score(pool, members, cfg)`, `search_portfolio(pool, cfg, seed)`.
- Consts `DEFAULT_POP_SIZE = 32`, `DEFAULT_GENERATIONS = 40`, `DEFAULT_FOLDS = 4`, `DEFAULT_INIT_DENSITY = 0.5`.
- Promotes `qe-determinism` to a normal dep.

## Test plan (TDD)

1. Converges to a robust basin (AC): three decorrelated trough-filling diversifiers + one fat-tailed bad
   strategy → monotonic best-score trace, bad strategy excluded, genuine ensemble (`count ≥ 2`) beating
   every singleton.
2. Net-of-cost (AC): subtracting a per-period cost strictly lowers the converged score.
3. Determinism; empty pool; single strategy.

## Risks

- Greedy DE can stall in a local basin — acceptable and on-spec (the AC asks for a robust basin, and the
  fold-CV + leave-one-out score prefers wide basins). Correlation/regime (QE-127) and capacity (QE-128)
  layer on top later. Worst-fold-min is a deliberately conservative aggregation behind `SearchConfig`.
