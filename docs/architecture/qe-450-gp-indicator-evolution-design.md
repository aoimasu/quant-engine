# Design Note QE-450 — Genetic-Programming Indicator Evolution

*Constrained Level-3 / FIR symbolic regression behind the Indicator trait*

> **Status:** Design proposal (not yet scheduled). Produced by a six-discipline panel (2 senior quant
> researchers, 2 mathematicians, 1 senior software engineer, 1 trading expert) — independent analysis →
> debate → chair synthesis, 2026-07-17. Related: the [Max Dama panel review](./../reviews/maxdama-vs-quant-engine.html)
> and its tickets [QE-430..449](../backlog.md#review-r2), several of which are hard prerequisites below.

`Area: search / signal (qe-wfo, qe-signal, qe-validation)` · `Depends on (hard): QE-439, QE-434, QE-436, QE-432, QE-430`

---

## 1. Recommendation

**Go — with conditions.** The engine's determinism, purge/embargo leakage safety, and search⊥portfolio
firewall are strong enough to make GP indicator evolution *sound*, but only behind a strict set of
prerequisites.

> **Headline condition:** **No evolved formula may reach the G1 gate until the deflation trial basis is made
> GP-aware** (QE-439: a distinct-canonical-formula count from the determinism lineage, floored by
> `cells·gens·windows`, plus the `dsr.rs` log-N numerical fix and uncensored dispersion). On the *current*
> basis, DSR/PBO would silently pass noise — the trial counter is blind to how hard the GP search rummaged.

The single load-bearing design decision, unanimous across the panel: **variable structure lives only in the
feature layer, behind the existing `Indicator`/`Kernel` trait — never in the strategy genome.** That one
choice isolates GP from the engine's fixed-locus assumptions and hands us batch=streaming parity and exact
FIR max-lookback *for free*.

---

## 2. Motivation

The hand-written 22-indicator catalogue fixes the feature layer at roughly **spectrum level 2**: functional
forms and window lengths are frozen — deliberately, to kill the maxdama §5.4 tuned-length overfitting vector.

GP offers genuine expressivity: discovering novel FIR combinations of price and perp-microstructure inputs
(funding / OI / premium / basis) that the fixed catalogue cannot express — **without touching the decision
layer**, because the engine already has a clean two-layer split: indicators produce quantised `QState` by
feature index, and `Genome::decide` reads those indices.

The prize is real **only if the search is honestly deflated.** The capacity mathematics is sobering: a single
crypto asset's 1h history gives ≈1,800 independent returns (`T_eff`), which supports only ~5 *learnable*
nodes. Without cross-asset pooling, GP degenerates into parametric window-tuning and the extra machinery is
pure overfitting surface. So the feature is worth building **only alongside cross-asset pooling and a coherent
multiple-testing account.** Done right, evolved indicators are first-class, content-addressed, leakage-safe,
determinism-preserving members of the schema. Done wrong, they manufacture spurious alphas that pass a
deflation bar that no longer reflects how many formulas were examined.

---

## 3. Scope

### In scope
- **FIR-only expression-tree indicators** (`Expr`) compiled to `Box<dyn Indicator>` via the existing `Kernel`
  blanket impl, evolved in a **separate offline QD stage** and **frozen** into the `FeatureSchema`.
- A **typed, closed primitive grammar** with a strongly-typed normalising root (rank/zscore) so outputs feed
  the existing point-wise `Quantiser` unchanged.
- **Exact structural max-lookback** fed verbatim to `IndicatorSpec.lookback` and `cv.rs` (purge/embargo).
- A separate `Elite<ExprTree>` MAP-Elites archive with structural descriptors (family / timescale /
  complexity), reusing the `archive`/`operator` patterns.
- **GP-aware deflation**: distinct-canonical trial count, uncensored dispersion / PBO, MDL parsimony, IC
  screening, and cost/turnover/capacity selection gates.
- **Cross-asset pooled** net-of-cost fitness reusing `fitness.rs` `log_growth` and the immutable backtest
  harness.
- **Content-addressed vintage sealing** of the frozen formula pool (`formula_hash` over a canonical
  S-expression).

### Out of scope (explicitly rejected or deferred)
- **Turning the strategy `Genome` into a variable-length tree** — *unanimously rejected*: reopens DE/MAP-Elites
  fixed loci, descriptor stability, and per-eval compute all at once.
- **Co-evolving the formula pool inside the strategy MAP-Elites loop** — Phase 2 only; invalidates the
  feature-matrix cache on every mutation and couples two intractable-to-count deflation surfaces.
- **True EWMA/IIR and all transcendentals** (`tanh`/`exp`/`ln`) — IIR breaks FIR closure; transcendentals
  threaten `Decimal` byte-identity.
- **Adaptive / dataset-fit / expanding-window quantisers** — would break the point-wise, no-dataset-fit invariant.
- **Evolving any part of the execution/decision policy** — would bypass the turnover firewall.
- **Live impact-coefficient measurement** — the capacity gate uses duplicated static config (see QE-431).

### Spectrum position
**Constrained Level-3 (effectively ~2.5):** full GP over formula *structure*, but FIR-only, typed grammar,
windows on a fixed lattice `{5,10,20,50,100}`, constants on a fixed grid, hard caps **depth ≤ 4 / nodes ≤ 16 /
lookback ≤ 200 bars**, strongly-typed normalised root — evolved **offline and frozen**, not co-evolved.

---

## 4. Design

### 4.1 The load-bearing decision: two representations, cleanly separated

- **Strategy genome — UNCHANGED.** `signal/src/genome.rs` stays a fixed-length `RuleSet[Clause;4]×2` over
  `feature: u16`. `Genome::decide` / `repair` / `is_valid` / `referenced_features` and `REP_VERSION = 1` need
  **zero** changes. The `FeatureSchema` simply becomes `[22 catalogue] ++ [K ≤ 16 frozen evolved]`; a `u16`
  trivially indexes it. **This is the turnover firewall** — the cost-blind, structurally-fixed decision loop
  in `backtest.rs` is what stops evolved code from inflating trading frequency.
- **Indicator genome — NEW.** A variable-structure expression tree:

```rust
enum Expr {
    Input(Field),                          // Close, High, Low, Volume, Typical, Funding, OI, Premium
    Const(Decimal),                        // snapped to a fixed rational grid
    Unary(UnOp, Box<Expr>),                // abs, sign, neg
    Binary(BinOp, Box<Expr>, Box<Expr>),   // add, sub, mul, protected_div
    Window(WinOp, Box<Expr>, Period),      // roll_{mean,max,min,std}, mean_abs_dev, delta, lag, rank, zscore
}
```

The tree compiles to a `Kernel`: each `Window` node owns a `Roll`; `observe(sample)` folds every leaf/window
in one pass, `raw()` reads the current value, `warm()` = all sub-windows full. It gets `Indicator` +
**batch=streaming parity for free** via the existing `impl<K: Kernel> Indicator` (`indicator/mod.rs:105`). The
interpreter is **`rust_decimal` only, no `f64`.**

A pure `max_lookback(Expr)` recursion (leaf→1, const→0, unary→child, binary→max, `Window(op,child,n)`→`(n−1)+child`)
yields the **exact** FIR span, fed verbatim to `IndicatorSpec.lookback` → `FeatureSchema.lookbacks` →
`PurgedKFold.lookback` — so `cv.rs` purge/embargo stays correct with **zero changes**.

### 4.2 Primitive grammar (typed, closed, FIR-only)

| Class | Primitives | Lookback rule / notes |
|---|---|---|
| **Price terminals** | `Close, High, Low, Volume, Typical` | leaf lookback = 1 (already on `Sample`/`Bars`) |
| **Flow terminals** | `Funding, OpenInterest, Premium` (basis proxy) | **GATED** — admitted only once dense bar-aligned forward-filled flow (QE-108) lands; else lookback is in *present scalars* not bars and `cv.rs` embargo is undersized. Phase 1 may restrict entirely. |
| **Const terminal** | `Const(Decimal)` on a fixed rational grid | lookback = 0. A finite grid is a **countability correctness** requirement — continuous constants make the reachable set uncountable and `E[maxSR]` ill-posed. |
| **Arithmetic** | `add, sub, mul, protected_div` (`|denom|<ε → 0`) | lookback = max(children); fixed zero convention; no new transcendental surface |
| **Pointwise** | `abs, sign, neg` | lookback = child |
| **Windowed** | `RollMean, RollMax, RollMin, RollStd, MeanAbsDev` | exact-Decimal online forms already in `roll.rs`; lookback = child + (n−1). `std_pop`'s single `sqrt` is the only transcendental, already golden-tested. |
| **Temporal** | `Delta(x,k), Lag(x,k)` | Delta = child + k; Lag = child + k |
| **Normalising roots** | `Rank(x,n)→[0,1)`, `Zscore(x,n)` clipped `[−4,4]` | **strongly-typed: every tree root must be one of these.** Both strictly causal FIR. **Rank is the default** (monotone-invariance collapses equivalence classes → fewer distinct trials, more turnover-stable). |

**Excluded grammar-wide:** true EWMA/IIR, `tanh`/`exp`/`ln`, `cumsum`/expanding/all-time-rank/`lead`/forward-shift,
window sizes `< 5`, and `Delta(x,1)` at any root-frequency-determining node.

### 4.3 Variation operators (`ExprTree::repair` is the analogue of `Genome::repair`)

- **`ExprTree::repair`** — deterministic, idempotent; called at the end of **every** operator: (a) force root
  to a normalising `Window` op; (b) cap total lookback ≤ 200; (c) cap depth ≤ 4 / nodes ≤ 16, pruning the
  deepest subtree deterministically on over-cap; (d) snap periods to `{5,10,20,50,100}`; (e) snap constants to
  the fixed grid; (f) protected-div zero convention; (g) recompute + cache lookback. The "mutate freely, then
  repair" contract is preserved.
- **LocalRefine** (exploit arm of the `OperatorSelector` bandit): constant-tweak ±grid-step, window
  lattice-step, same-family input-swap; re-roll if the descriptor cell changes.
- **Explore** (cell-changing): subtree crossover between two elites, subtree-replace, grow, prune.
- **FreshRandom:** ramped-half-and-half random trees to `D_max = 4`.
- All RNG through `DetRng` via `task_rng(master, index)` exactly like `variation.rs`; node selection = uniform
  index over a deterministic pre-order traversal. The `operator.rs` credit-firewall (no OOS reward reaches the
  bandit) is reused **as a pattern**. A new `Elite<ExprTree>` archive is required — only the descriptor-band
  math and bandit pattern are reused, not the storage.
- A **golden-value test** pins the tree-mutation stream + a canonical eval vector.

### 4.4 Quantiser — no new quantiser, no change to `quant.rs`

Normalisation is a **strongly-typed root `Window` node inside the tree** (`Rank`→`[0,1)` or `Zscore`→clipped),
whose window is already charged by the `max_lookback` recursion. Its bounded output feeds the **existing
stateless point-wise** `Quantiser::Linear{0,1,states}` (rank root) or `Quantiser::Bands{symmetric edges}`
(zscore root). This preserves `quant.rs`'s invariant verbatim ("point-wise on purpose: no rolling quantiles,
no dataset-wide fit"), and the existing `ac2_latest_output_independent_of_out_of_window_samples` test
generalises to trees to prove no forward peek.

> The panel converged on **state-in-the-tree** over the original state-in-the-quantiser proposal because it
> reuses the existing lookback recursion + ac2 proof and changes `quant.rs` by zero lines. *(Dissent §12.2.)*

### 4.5 Niching — a separate `Elite<ExprTree>` MAP-Elites archive

Three **pure-structural** descriptor axes (window-invariant → `cell_reassignment_rate = 0.0`, respecting
`STABILITY_THRESHOLD = 0.05`):

1. **Family** — dominant input/op mapped to the existing 5 `IndicatorFamily` variants via a **new structural
   classifier** derived from the tree's dominant `Field`/root-op (funding/OI/premium leaves → `Flow`),
   replacing `family_of`'s id-prefix match which would break on auto-named formulas.
2. **Timescale** — band from structural lookback (reuse `TimescaleBand`).
3. **Complexity** — node-count band `{≤2 / 3–4 / ≥5}` — the **parsimony-illuminating** axis, so a simple
   2-node formula is a first-class elite a complex one can never out-compete in its band.

Grid ≈ `5×3×3 = 45` cells, Deep-Grid subpop 8. Plus an **input-cadence sub-descriptor** so flow/carry alphas
niche by real (8h) update frequency, not phantom per-bar reactivity. Anti-collapse: uniform-non-empty-cell
parent sampling + in-sample behavioural dedup (reject an offspring whose quantised series correlates > 0.95
with an existing elite in its target cell — firewall-safe, in-sample only).

### 4.6 Fitness — net-of-cost `log_growth`, cross-asset pooled, wrapped by tradability gates

Reuse `fitness.rs` net-of-cost geometric `log_growth` as the **core**, evaluated on **cross-asset pooled
returns** (one shared formula scored across ~20 liquid perps; effective-independent `T_eff` set from
**measured** cross-asset return correlation, not assumed). Wrap it with gates applied **at archive insertion**
(they tighten the survivor set but do **not** reduce the trial count N):

> **DSR-honest basis correction (QE-451 Phase 1b).** The trial count `N` that deflates the DSR axis is
> **`max(distinct-canonical count, N*)`**, where `N*` is the **shuffle-null-calibrated** effective count
> ([§5 κ-null row](#5-overfitting-controls-the-backbone); `qe_wfo::gp::calibrate_null_basis`), **not** the
> raw `max(distinct, cells·gens·windows floor)` alone. On pure noise the raw `max(distinct, analytic-floor)`
> basis **under-deflates the DSR** (the empirical best-of-N Sharpe is heavier-tailed than the parametric
> `E[max SR]` Gumbel assumes, so DSR sits `> 0.7` on a label-shuffled champion); only `N* ≥ P` brings the
> shuffled-champion DSR to ≈ 0.5. `assess_gp_champion`/`gp_trial_basis` are **basis-agnostic** (they deflate
> against whatever `N` the caller supplies) and nothing auto-applies `N*` yet — **the QE-454 production seal
> MUST fold `N*` in** (deflate against `max(distinct, N*)`) before G1; sealing on the analytic floor alone
> would ship the under-deflated DSR.

- **(a) Cost-stressed fitness** = `min` over friction multiplier `m ∈ {1.0, 2.0}` of re-costed `log_growth`
  via the existing `friction::cost_sweep` (free, because `decide()` is cost-blind so the event stream is
  identical). Require finite and `> 0`; optional `m = 3.0` non-ruin check; persist the chosen multiplier in
  the vintage.
- **(b) Max-turnover REJECT gate** (`fitness.mean = NEG_INFINITY`), symmetric to the existing `min_trades`
  gate — reject if `trades > max_turnover_frac · n_bars` (default 0.25 ⇒ avg hold ≥ 4h). Kills the
  `sign(delta(close,1))` flip-flop noise-scraper that passes every parsimony control.
- **(c) Capacity floor at selection** via an inlined `capacity()` with impact coefficients **duplicated** into
  wfo config (firewall-safe, mirroring `capacity.rs`'s own documented pattern) guarded by a coefficient-parity
  unit test; reject below `CAPACITY_FLOOR ≈ $250k`.

Plus the in-search MDL parsimony rent (§5). The immutable backtest harness (flat-only, next-bar,
single-position, taker-only) stays the turnover firewall.

---

## 5. Overfitting controls (the backbone)

This is where the feature is won or lost. Ordered by severity.

| Control | Ticket | Detail |
|---|---|---|
| **GP-aware deflation trial basis** | **QE-439 (#10) — HARD BLOCKER** | Replace `effective_trials(cells,gens,windows)` blindness with `N = max(distinct-CANONICAL formulas ever scored, cells·gens·windows floor, complexity floor, **shuffle-null-calibrated N***)`. Canonicalise (constant-fold, normalise commutative order, collapse rank-monotone wrappers, algebraic dedup) then content-hash. **Count every evaluated formula — including IC-screen and cost/turnover/capacity rejects** (screening filters compute, never the hypothesis count). Record `distinct_evaluations` in `RobustnessReport`. **The DSR-honest basis is `max(distinct, N*)`, NOT the analytic floor alone** — the raw `max(distinct, cells·gens·windows)` can *under*-deflate the DSR axis (best-of-N in-sample Sharpe is heavier-tailed than the Gumbel `E[max SR]` assumes; a label-shuffled champion sits at DSR `> 0.7` on the raw basis, ≈ 0.5 only once `N* ≥ P` from the κ-null calibration is folded in — see the κ-null row). QE-451 Phase 1b ships `gp_trial_basis`/`assess_gp_champion` **basis-agnostic** + `calibrate_null_basis`; **the QE-454 production seal MUST deflate against `max(distinct, N*)`** (nothing auto-applies `N*` yet). **Numerical fix:** `expected_max_sharpe` computes `normal_ppf(1−1/n)`; for `n ≳ 4.5e15`, `1−1/n` rounds to 1.0 → `+∞`. Add a log-space `expected_max_sharpe_ln(ln_n)` path (`~√(2 ln N)`); the bar self-caps near 8–13 even at `N ~ 1e20`. |
| **Uncensored dispersion + PBO** | extends QE-414 | Feed the return series of **every** evaluated formula (not just archive champions) into `variance_returns` (the DSR dispersion population) and a stratified full-population sample into PBO/CSCV. MAP-Elites keeps only per-cell maxima → under-disperses → **inflates** DSR; uncensoring restores honesty. Make **uncensored PBO the primary GP gate**; demote DSR to necessary-not-sufficient. |
| **In-search MDL / parsimony** | **QE-436 (#7)** | Two-part rent in the offline pool-selection objective: `penalised_mean = mean_log_growth − (1/N_bars)·[n_struct·ln(4·f_eff·t) + n_const·½·ln(T_eff)]`, `ln(4·f_eff·t) ≈ 7.5` nats/node. Lexicographic tie-break to the shorter tree inside `should_replace`'s noise band. Keep MDL **out** of the per-genome fitness that feeds DSR (avoids interacting with the deflation stage). Backed by hard caps in `Expr::repair`. |
| **Complexity-stratified trial variance** | `dsr.rs::trial_sharpe_variance` | Bigger trees overfit harder ⇒ wider in-sample Sharpe dispersion. Estimate `V` **within node-count strata** and deflate each candidate against the `V` of its own size band. Replaces the retracted `B^{κn}` N-multiplier (*dissent §12.1*). |
| **IC pre-screen with FDR** | **QE-434 (#5)** | Purged rank-IC (Spearman of signal vs forward 1h return) on fold A; admit only if fold B shows same-sign IC of comparable magnitude **and** it clears a Benjamini-Hochberg FDR threshold across all formulas screened this generation. Filters compute, never N. (Greenfield — no IC/Spearman exists today.) |
| **Slow-reference oracle** | **QE-432 (#3)** | A naive non-incremental batch recompute of each tree (`O(n·lookback)`, zero shared state) asserts bar-for-bar equality with the streaming Kernel interpreter — an **independent** second implementation so an incremental `Roll`-folding bug cannot hide. Cheaper for compositional trees than for the hand catalogue. |
| **Deflated + purged ensemble correlation penalty** | **QE-430 (#1)** | Replace `objective.rs`'s raw in-sample floored Pearson with (a) purged cross-fold correlation, (b) a best-of-M chance floor `√(2 ln M / T)`, (c) Fisher-z shrinkage by length-T SE. With a large GP pool you can always find in-sample-decorrelated pairs in noise. Add a **leave-one-PROVENANCE-out** floor (cluster evolved members by lineage, drop whole clusters) and cap the evolved-formula share of any ensemble. |
| **Holdout-consultation budgeting** | new | A GP program runs many vintages against the **same** G1 holdout → silent multiple-testing. Escalate the DSR threshold with cumulative consultations recorded in lineage, or rotate/embargo the holdout per campaign; gate G1 on **accumulated** N. |
| **κ / search-efficiency calibration null** | extends `nulls.rs` | Add a label-shuffle / block-bootstrap-target null (today only HODL + random-entry). Calibrate any complexity discount to the smallest value that holds shuffled-champion DSR ≈ 0.5 across node-size bands; persist in lineage. |

---

## 6. Invariants preserved

| Invariant | How it is preserved |
|---|---|
| **FIR closure / no look-ahead** | FIR-only grammar; `max_lookback(Expr)` pure recursion yields exact span → `IndicatorSpec.lookback` → `PurgedKFold.lookback`, so `windows_disjoint(lookback,horizon)` holds with zero `cv.rs` changes. CI runs the generalised ac2 perturbation test over every evolved tree + a "declared ≥ computed reach" property test. |
| **No dataset-wide/adaptive fit** | Normalisation is a causal trailing-window FIR **root node**; the value→state map is the unchanged stateless `Quantiser`. `quant.rs` changes by zero lines. |
| **Batch = streaming determinism** (QE-006/206) | Tree compiles to a `Kernel` on the single `update()` path that *is* `compute_batch` — parity is structural. `rust_decimal`-only interpreter; the slow-reference oracle is an independent impl. Mutation draws from `DetRng`; golden stream pinned. |
| **Search ⊥ portfolio firewall** (QE-001/132) | GP variation + IC/cost/capacity gating live in `qe-wfo`; the pure `Expr` interpreter lives in `qe-signal`. Trial counts and impact coefficients cross as **sealed DATA / duplicated CONFIG**, never a code edge. Coefficient-parity test prevents silent drift. |
| **Vintage identity** | `CatalogueIdentity` extended with a `formula_hash` = SHA-256 over each pooled formula's canonical S-expression. `CATALOGUE_VERSION` bumps only on primitive-set/interpreter-semantics change; individual formulas are data-versioned by their sexpr hash; the load boundary asserts exact identity. |
| **Turnover firewall** | GP evolves only indicator trees producing quantised feature states; `decide`, sizing, and the fill loop stay byte-fixed. A firewall assertion confirms evolved trees emit only feature states, never orders. |

---

## 7. Risks

| # | Risk | Severity | Mitigation |
|---|---|:---:|---|
| 1 | GP shipped on the current `effective_trials(cells,gens,windows)` basis → n_trials counts ~45 cells, not the 10³–10⁶ trees/cell actually searched → **DSR/PBO silently pass noise**. | **blocker** | QE-439 distinct-canonical count + uncensored dispersion + log-N fix, calibrated to a shuffle null before any evolved alpha reaches G1. |
| 2 | Even with correct N, the DSR bar grows only `~√(2 ln N)` (~37% for a ~2000× space) → a passing DSR treated as sufficient → noise ships. | **blocker** | Defense-in-depth: DSR necessary-not-sufficient; gate jointly on uncensored PBO (primary) + FDR IC + MDL + holdout budgeting. |
| 3 | Single-asset 1h history (`T_eff ≈ 1,800`) supports only ~5 learnable nodes → GP without pooling collapses to window-tuning; expressivity prize illusory. | major | **Cross-asset pooling is a prerequisite** (`T_eff ≈ 9k`, `n_max ≈ 20–35`); pooling discount from measured cross-asset correlation. Do not ship single-asset. |
| 4 | Any EMA/IIR primitive is not finite-lookback → declared lookback under-states all-history dependency → undersized embargo → leakage with every invariant green. | **blocker** | Phase-1 grammar is FIR-only; EMA excluded. If ever added, only the catalogue's seeded-windowed form, enforced by the slow-reference oracle. |
| 5 | Flow sparsity: funding/OI/premium post ~every 8h vs 1h bars → lookback in present-scalars is ~8× smaller than true bar-span → embargo undersized → leakage; forward-fill manufactures phantom turnover. | major | Require dense bar-aligned forward-filled flow (QE-108) or size embargo at coarsest cadence (QE-128); add input-cadence descriptor. Phase 1 restricts flow terminals until dense-fill lands. |
| 6 | Continuous real constants make the reachable space uncountable → `E[maxSR]` over finite N is ill-defined → DSR invalid. | major | Quantise constants to a finite grid + windows to the fixed lattice; with caps the canonical set is finite. |
| 7 | A maximally-parsimonious noise-scraper (`sign(delta(close,1))`) maxes turnover (~1.5%/day cost drag), passes every complexity/MDL/depth control, marginally net-positive at 1× friction — then dies live. | major | Cost-stress `min{1×,2×}` + max-turnover reject + capacity floor + grammar signal-frequency floor. Rejected candidates still count toward N. |
| 8 | Co-evolution invalidates the feature-matrix cache on every mutation and couples two moving-cardinality deflation spaces. | major | Phase 1 is strictly freeze-then-search; co-evolution deferred to Phase 2 with DAG-interned caching + separate budget. |
| 9 | Friction slippage impact is per-unit-qty (price-scale dependent) while capacity impact is per-dollar; pooling lets GP prefer low-price perps where cost is under-charged. | minor | Normalise `friction.rs` impact to bps-of-notional before pooling (QE-431); cross-instrument cost-invariance test. |
| 10 | Semantic-duplicate trees waste coverage and inflate raw eval count. | minor | Canonical-hash dedup for the count + in-sample behavioural dedup (>0.95 → reject) at insertion. |

---

## 8. Prerequisites (hard blockers first)

1. **QE-439 (#10)** — GP-aware distinct-canonical trial basis + `dsr.rs` log-N fix + uncensored
   `variance_returns`/PBO. *The single hard blocker* — without it DSR/PBO/SPA are decorative.
2. **QE-434 (#5)** — per-formula IC screening (two-fold sign-consistency + Benjamini-Hochberg FDR). Kills the
   bulk of noise formulas cheaply. Greenfield.
3. **QE-436 (#7)** — in-search MDL/parsimony + hard depth/node/lookback caps. Greenfield.
4. **QE-432 (#3)** — independent slow-reference oracle for trees.
5. **QE-430 (#1)** — deflate + purge the ensemble correlation penalty.
6. **Cross-asset pooled evaluation harness** — raises `T_eff` so the node budget and PSR are meaningful.
7. **Dense bar-aligned forward-filled flow (QE-108)** or coarsest-cadence embargo (QE-128) — before flow
   terminals are admitted.
8. **Label-shuffle / block-bootstrap-target null** added to `nulls.rs` — to calibrate the deflation basis.
9. **Phase-0 seam-proof refactor** — `Expr` + `Kernel` interpreter reproducing a subset of the 22 catalogue
   indicators, full pipeline byte-identical.
10. **`friction.rs` impact normalised to bps-of-notional + coefficient-parity test** (QE-431) — before
    capacity gating and cross-asset pooling.

---

## 9. Phased rollout

| Phase | Goal | Deliverable |
|---|---|---|
| **Phase 0 — Seam proof (no search)** | Prove the two-layer decomposition + interpreter parity with zero new search. | `Expr` + `Kernel` interpreter (`rust_decimal` only) + `max_lookback`, seeded *only* with formulas that reproduce a subset of the 22 catalogue indicators; the full existing pipeline runs **byte-identically** over the formula-backed catalogue; ac1/ac2 generalised to trees + slow-reference oracle (QE-432) green. Behind a feature flag, off by default. |
| **Phase 1a — Offline GP pool under a trivial head** | Illuminate a tree archive cheaply with clean, countable deflation. | `Elite<ExprTree>` MAP-Elites archive (family/timescale/complexity descriptors, uniform-cell sampling, behavioural dedup); `ExprTree::repair` + tree-aware operators on `DetRng`; FIR-only grammar with fixed lattice/grid + caps; illuminated under a **trivial fixed decision head** (threshold-cross). Golden mutation-stream test. Distinct-canonical trial count emitted to lineage. |
| **Phase 1b — Deflation + gates + freeze** | Make the GP search honestly deflated + tradability-screened, then freeze. | QE-439 + QE-434 + QE-436 + QE-430 wired; cost-stress + turnover-reject + capacity-floor gates; cross-asset pooled fitness; label-shuffle null calibrated. **K ≤ 16 trees frozen**, canonically hashed into `CatalogueIdentity`/vintage; existing MAP-Elites/DE runs **unchanged** over the enlarged static schema. G1 reads accumulated N. |
| **Phase 2 — Co-evolution (deferred, gated)** | Find genome-formula synergies only after Phase 1 is proven safe. | Two-timescale co-evolution (outer slow GP proposes vintages; indicator fitness = marginal IC contribution) with DAG-interned sub-expression caching + a **separate** compute/deflation budget. Not started until Phase-1 vintages pass out-of-sample. |

---

## 10. Acceptance criteria

1. **Phase-0 equivalence:** the formula-backed catalogue reproduces the 22 hand-written indicators and full
   pipeline output is **byte-identical** to the current build; `ac1_batch_equals_streaming` and
   `ac2_latest_output_independent_of_out_of_window_samples` pass over every evolved tree.
2. The **slow-reference oracle** (naive non-incremental recompute) equals the streaming Kernel interpreter
   bar-for-bar over a randomly sampled tree population.
3. For every evolved tree, declared `IndicatorSpec.lookback ≥ computed structural reach`, and a sample
   perturbed older than the declared lookback leaves the latest `QState` byte-identical.
4. The interpreter contains **no `f64`** in any evaluation path (grep/CI guard); re-running a GP vintage twice
   is byte-identical.
5. On a **label-shuffled / block-bootstrap** null, evolved-champion DSR sits at ≈ 0.5 across all node-size
   bands after calibration — i.e. once deflated against the **calibrated `N* ≥ P`** (`max(distinct, N*)`),
   **not** the raw `max(distinct, analytic-floor)` (which under-deflates the DSR to `> 0.7` on pure noise);
   a passing DSR on real data is accompanied by a passing uncensored PBO.
6. The lineage records the **distinct-canonical trial count including all rejects**; `RobustnessReport`
   surfaces it beside `variance_trials`; `effective_trials` never under-counts below `cells·gens·windows`.
7. `expected_max_sharpe` returns a **finite** bar (≈8–13) at N up to `1e20` via the log-N path.
8. `check_firewall` stays green (no `qe-wfo → qe-ensemble/qe-validation` code edge); the coefficient-parity
   test confirms duplicated impact coefficients match `capacity.rs`.
9. No evolved genome enters the archive unless `min{1×,2×}` re-costed `log_growth` is finite and `> 0`,
   realised turnover ≤ `max_turnover_frac·n_bars`, and capacity ≥ `CAPACITY_FLOOR`.
10. `CATALOGUE_VERSION` bumps only on primitive-set/interpreter-semantics change; a sealed genome referencing
    an evolved feature re-evaluates against a byte-identical formula (`formula_hash` exact-match at load).
11. Flow terminals are admitted only when dense bar-aligned forward-filled flow is present (or embargo is
    sized at the coarsest cadence); otherwise Phase 1 runs price-only.

---

## 11. Open questions

1. **Phase-1 fitness:** trivial fixed decision head (cheap, clean deflation) vs full pooled backtest per
   candidate (realistic, costlier)? *Lean:* trivial head + IC screen for 1a; pooled backtest only for screened
   survivors in 1b — keeps expensive search off the critical path.
2. **Replace vs add** the 22 catalogue entries? *Lean:* fixed blend `22 + K≤16` (keep proven baselines).
3. **Trial-basis dedup granularity:** canonical structural hash vs behavioural equivalence? *Lean:* canonical
   structural hash primary; behavioural dedup only for archive coverage, not the count.
4. **Within-cell max & dispersion:** does within-cell selection deflate apparent dispersion and thus the
   E[max] bar? *Lean:* complexity-stratified variance over the uncensored population.
5. **Power collapse:** does the much larger empirical distinct count collapse DSR power so far nothing passes
   G1? *Lean:* accept the honest count; buy power back via **pooling** (raise `T_eff`), not a softer N —
   requires a synthetic-edge calibration study.
6. **Root normaliser mix:** does forcing a normalised root miss useful raw-difference formulas? *Lean:* rank
   default, zscore where tails matter; revisit if the archive shows gaps.
7. **Capacity target AUM + cost-stress multiplier:** single `$250k` + fixed 2× vs a sweep + data-driven
   multiplier? *Lean:* fixed conservative defaults for Phase 1, persisted in the vintage; measured multipliers
   once live slippage data exists.

---

## 12. Debate — resolved dissents

The panel did not agree on everything; these are recorded honestly.

1. **Charging model complexity in the deflation basis.** Math#1 proposed `N_eff = trials · B^{κ·n_nodes}`;
   Math#2/QR#2/SSE argued an empirical distinct-canonical count is the honest basis and that multiplying by
   class-cardinality double-counts. **Resolution:** Math#1 **retracted** the `B^{κn}` N-multiplier;
   `N = max(empirical distinct-canonical, analytic floor)`, and complexity enters via **node-count-stratified
   trial variance** instead. `B^{κn}` survives only as an analytic ceiling / calibration target.
2. **Where trailing normalisation lives.** Math#2 wanted stateful `TrailingRank`/`TrailingRobustZ` in the
   `Quantiser` enum; SSE/QR#1 wanted a strongly-typed FIR root node feeding the unchanged quantiser.
   **Resolution:** state-in-the-tree — a stateful quantiser would break `quant.rs`'s invariant and need a
   second lookback path + a fresh ac2 obligation. `quant.rs` changes by zero lines.
3. **Rank vs zscore default.** Rank collapses equivalence classes (a deflation benefit) but a sliding-window
   rank re-buckets every bar (turnover on slow inputs). **Resolution:** rank default for its deflation
   benefit + stability at fixed bands; zscore where tails matter; add a fixed-edge deadband if churn persists;
   the max-turnover reject gate is the real backstop. *(Residual medium-confidence divergence.)*
4. **Is net-of-cost `log_growth` sufficient?** QR#1 leaned on structural pressures and wanted parsimony/turnover
   penalties off; Trading held a 3-node formula can be the worst turnover offender (cost ⊥ size).
   **Resolution:** necessary but **not** sufficient — add cost-stress, max-turnover reject, capacity floor.
   QR#1 partially retracted "MDL off": MDL-in-fitness is **on** (it shapes the search interior at a different
   stage from validation-time deflation).
5. **Counting every evaluated/screened formula in N.** Search-side voices worried subtree reuse correlates
   trees so the effective independent count is smaller; Math#1 noted evolutionary search can also *under-count*
   by optimising toward the class supremum. **Resolution:** over-counting is conservative and the safe
   default; canonicalisation removes exact equivalence classes; the true N lies between the empirical distinct
   count (floor) and the class ceiling, **interpolated by calibration against a shuffle null**, not assumed.
6. **Single-asset viability.** Math#1 held single-asset `T_eff` affords `<1` node of genuine structure.
   **Resolution:** accepted as a **hard prerequisite** — cross-asset pooling required before G1; do not ship
   single-asset GP.
7. **DSR as headline metric at GP scale.** QR#2 argued DSR grows only `~√(2 ln N)` and should be demoted.
   **Resolution:** DSR demoted to a necessary floor; **uncensored PBO + FDR IC + MDL + holdout budgeting** are
   the joint primary gates. Documented that DSR is a weak logarithmic control at GP scale.

---

## 13. Admin UI & operational safety

> Produced by a second six-discipline panel (Senior Frontend, Senior Backend, Quant Researcher [review-gate
> stats], Trading Operator, SRE/Ops-Safety, Security/Governance) — analysis → debate → chair synthesis,
> 2026-07-17. The panel converged at high confidence. Implementation tickets: **QE-451..QE-454**.

### 13.1 Principle — the safe path is the only path

The admin UI is not a convenience layer; it is **where the human control for a dangerous capability lives.** The
governing rule, unanimous across the panel: **every gate is enforced server-side; the SPA only makes the safe
path the easy path.** A determined operator issuing a crafted `POST` that skips the UI must be *unable* to seal
an un-deflated or sandbox pool — because authorization, mode, prerequisites, and the deflation/tradability gate
are all re-derived by the server from immutable, hash-verified artifacts, and every attempt (success or 409) is
audited. The client's "seal" button state is **cosmetic**.

### 13.2 Integration surface (reuse, don't rebuild)

GP reuses the existing **job → server → SPA** machinery:

- **`evolve` CLI job** — `RunSpec::Evolve(EvolveParams)` (`EvolveParams` in the `qe-run-protocol` leaf crate,
  `PROTOCOL_VERSION 1→2`, **`seed` REQUIRED**, every field `#[serde(default)]`). `spawn.rs` gains a
  `qe evolve … --run-dir --json` arm reusing the QE-419 config pin + `kill_on_drop`. `create_run` gains an
  `evolve` arm in `build_spec` + `validate_evolve` (caps `depth≤4/nodes≤16/lookback≤200`, windows ∈
  `{5,10,20,50,100}`, `K≤16`, seed present, `mode ∈ {sandbox,production}`). The terminal `Done` line emits
  **`pool: Option<String>`, NEVER `vintage`** (with a manager-level assertion/test that an evolve run never
  writes the vintage repo).
- **Frozen-pool artifact** — a dedicated **`qe-formula-pool` leaf crate** (not `qe-vintage`: different content
  shape — `K` canonical S-expression strings + a deflation-summary block + review lineage — and a different
  lifecycle; runtime never needs pools). It reuses `Vintage`'s `seal()/verify()/load()` SHA-256 discipline
  verbatim (load never yields an unverified pool). Pool artifacts live under a **separate directory root**.
- **Server** — new read routes `GET /api/formula-pools[/{id}]` and `GET /api/runs/{id}/archive`; governance
  routes `POST /api/formula-pools/{id}/{approve,seal,reject,revoke}`; `POST /api/runs/{id}/halt`. All inside
  `protected_routes`; governance routes additionally carry `require_role`.
- **Firewall** — pool/audit code stays in `qe-server` + `qe-formula-pool` + `qe-vintage`/`qe-validation`; the
  `firewall.rs`/`dependency_topology` test is extended to assert **no `qe-runtime`/`qe-venue` edge** after the
  pool code lands.
- **Sealing does NOT mint a vintage directly.** A sealed pool **registers its `K` formulas into
  `CatalogueIdentity`**; a subsequent ordinary **`train` run** produces the real vintage over the enlarged
  static schema, and the **train job's existing G1 gate is the final production gate** (QE-450 Phase 1b). This
  keeps one production gate, not two.

### 13.3 Two lifecycles — run vs pool (debate-resolved)

The frontend's round-1 idea to add approval states to `RunStatus` was **rejected**. The **run** stays the
4-variant `RunStatus` and terminates normally at `succeeded` when the artifact is written. The **pool** is a
*separate resource* with its own human-paced, revocable governance lifecycle:

```
Illuminated → PendingReview → (AwaitingSecondSignoff) → Sealed
                            ↘ Rejected                 ↘ Revoked
```

Pool state is authoritatively the **append-only signed audit log**; a `governance/<pool_hash>.json` file is a
*rebuildable cache the seal gate never reads*. The SPA polls the pool via a thin `usePollingPool` hook
mirroring `usePollingRun` — no SSE/WebSocket, no non-terminal run statuses.

### 13.4 Screens — a new `web/src/app/evolve/` area (mirrors `app/training/`)

`EvolveArea.tsx` is a router-less `useState<View>` machine `{list|new|monitor|review|pool}`; nav id `evolve`
("Indicator evolution") wired into `App.tsx`. `runs.ts` gains an `evolve` variant of the discriminated union
(kept in lockstep with `qe-run-protocol`).

| Screen | Key elements | Operator actions |
|---|---|---|
| **NewCampaign** | Caps (`depth≤4/nodes≤16/lookback≤200`) + windows lattice shown as **fixed, disabled guardrail chips**; flow terminals disabled with a hint ("admitted only when dense forward-filled flow QE-108 lands"); `Sandbox / Production` segmented control with **Production disabled unless the server reports all prereqs satisfied** (inline callout naming the missing QE-439/434/436/432/430). | Configure + launch → `createEvolveRun`. |
| **CampaignMonitor** | Persistent **mode banner** (sandbox = "RESEARCH — cannot reach a production vintage"); **ArchiveHeatmap** (5 family × 3×3 timescale×complexity small-multiples, reusing `.qe-cov__grid`); **TrialCountBar** (distinct-canonical N vs the analytic `cells·gens·windows` floor vs the `E[maxSharpe]` deflation bar, amber if `N < floor`); **reject ledger** (turnover/capacity/IC/cost/dedup reject rates per generation). | Watch; **Halt** (authz'd SIGTERM). |
| **FormulaSexpr** | Token-coloured single-line canonical form (`rank(delta(close)/roll_std(close,20),50)`) in an `overflow-x:auto` container + expandable indented tree; a **declared-vs-computed `max_lookback` chip** (danger badge if declared < computed — the leakage tell). | Read/inspect. |
| **PoolReview** *(the gate)* | Sandbox = **read-only, no seal affordance at all**. Production = a **non-collapsible Deflation-basis card** (the four numbers together), a **per-formula table** with tradability traffic-lights (cost-stress `min{1×,2×}`, turnover, capacity) as the **headline** and Sharpe/DSR demoted, and an ordered server-asserted checklist. | Acknowledge each of the K cards; type the pool hash + rationale; approve (dual sign-off). |
| **PoolBrowser** | Read-only audit table: both approver emails + timestamps + rationales, launch entry, content-addressed `pool_hash`, and the vintage-lineage join. | Audit / chain-verify (`GET /api/audit`). |

**Anti-alarm-fatigue:** a neutral "review required" resting state; danger red is reserved strictly for a genuine
threshold failure. Two mechanically distinct tiers everywhere — **BLOCK** (red, disables seal, non-overridable
in production) vs **WARN** (amber, explicit per-item acknowledge, never blocks).

### 13.5 The operator review gate — the minimum honest stat set

The Deflation-basis card renders **four numbers together**, never a lone green tile: **distinct-canonical N vs
the analytic floor** (with an "includes rejects" marker), the **finite `E[maxSharpe]` bar** with observed Sharpe
overlaid, **uncensored PBO** with its population size, and **DSR labelled "necessary — not sufficient, weak
~√(2 ln N) control."** Below it, a per-formula table (K ≤ 16). A pool is **`sealable` only as a server-recomputed
AND** — a visible decomposition, never a standalone health badge.

**HARD-BLOCK conditions (non-overridable in production; every *absent* stat is a block, never a vacuous pass):**
1. `gp_aware == true` and **`distinct_evaluations` present and `> cells·gens·windows`** floor (QE-439). `N == floor` exactly ⇒ "trial basis is the blind floor — QE-439 not wired" ⇒ block.
2. Finite `E[maxSharpe]` via the log-N path (guards the `dsr.rs` `+∞` bug at `n ≳ 4.5e15`).
3. **Uncensored PBO ≤ threshold** (primary gate), estimated over `variance_trials ≥ distinct_evaluations` — a **censored** population (top-N only) ⇒ block.
4. DSR ≥ 0.95 (necessary-not-sufficient floor).
5. Every formula: IC two-fold same-sign + Benjamini-Hochberg FDR pass (QE-434).
6. Every formula: cost-stress `min{1×,2×}` net log-growth **finite and > 0**; realised turnover ≤ `max_turnover_frac·n_bars` (0.25); capacity ≥ `CAPACITY_FLOOR ≈ $250k`.
7. Every formula within MDL / node-count / depth / lookback caps (QE-436), deflated against its **own node-count stratum**.
8. No formula fails its **turnover-matched random-entry null** ("SCRAPES NOISE", `nulls.rs`).

**WARN tier (acknowledge, never block):** descriptor/perturbation instability, evolved-share concentration,
elevated holdout-consultation count, zscore-root clip saturation, formula opacity at the depth/node cap.

**Displayed = enforced = evidenced:** the numbers shown, the numbers `seal_allowed` enforces, and the numbers
captured in the audit `evidence_hash` are one set. This requires adding **uncensored PBO + `variance_trials` +
`distinct_evaluations` to `GateSnapshot`/`evaluate_g1`** (currently omitted).

### 13.6 Sandbox vs production — three independent structural barriers

No mode flag is the sole control; all three are fail-closed:

1. **Compiled-in `qe_validation::DEFLATION_BASIS_VERSION` const** (0 today; bumped **only** in the QE-439/434/436/432/430 landing commit, recorded in `Lineage.code_commit`). `validate_evolve` rejects `mode:production` with `400` when `const < REQUIRED` — a tampered client cannot even *launch* a production campaign. The artifact's own `deflation.gp_aware` (emitted by the real trial-counter code path) is the **primary** guard; production requires **both** `gp_aware && const`.
2. **Physically separate research artifacts root** (`data/artifacts/research/`) that `GET /api/vintages` (`read.rs`) and the runtime **never list/load** — a sandbox pool is off the production load path entirely, a directory boundary no flag can flip.
3. **Fail-closed `assert_production_eligible` reusing the `assert_schema`/`CatalogueIdentity` boundary** (`vintage/schema.rs:52`) — a sandbox-identity pool is **structurally unloadable** in production even if the file is copied into the prod dir.

Prerequisites "unlock" production purely by the const bump, provable from `Lineage.code_commit`.

### 13.7 The seal predicate (server-authoritative)

`POST /api/formula-pools/{id}/seal` runs `seal_allowed(pool.json[hash-verified], audit-log replay,
DEFLATION_BASIS_VERSION)` in `spawn_blocking` (under the QE-425 deadline). It reads **only** those three inputs —
**no request field feeds it.** It requires *all* of §13.5's hard-blocks **plus** `mode == production`, the const
satisfied, and **two distinct valid approver signatures**. Any failure ⇒ `409` with a **named blocker list** and
an **appended rejected-attempt audit entry**. The stored `review.json` status is *never* trusted as
authorization — approval is re-derived from the `pool_hash`-bound signature events; a mismatched `pool_hash`
invalidates every signature.

### 13.8 RBAC & separation of duties

- **Roles resolved per-request server-side** from env allowlists (`QE_ROLE_OPERATORS` launch/monitor;
  `QE_ROLE_APPROVERS` approve/reject/revoke; `QE_ROLE_ADMINS`), parsed with the existing fail-closed
  `parse_allowlist`. **Never carried in the session cookie** — so revoking an approver takes effect on their
  *next request* and cannot be spoofed by a cookie or body field. A `require_role(Role)` extractor mirrors
  `require_session`, reading the `AuthedEmail` extension.
- **Dual sign-off:** production seal needs **two distinct approver identities, neither equal to the launcher**
  (launcher committed as the first audit entry at create time). First approve → `AwaitingSecondSignoff`; a
  second distinct approver → the two-signature clause passes. Reject/revoke are single-approver but audited.
- `/api/me` exposes `capabilities:{canLaunch,canApprove}` as **UX-only** hints that can only *remove*
  affordances; the server enforces regardless of what the client renders.

### 13.9 Tamper-evident audit + governance↔lineage binding

- **Audit log** `<data_dir>/audit/log.jsonl` (sibling of `runs/`): each entry
  `{seq, ts_ms, actor_email, action(launch|approve|reject|revoke|role_change), subject_hash(pool), run_id,
  vintage_id, evidence_hash, prev_hash, entry_hash}` where `entry_hash = SHA256(canonical_json ‖ prev_hash)` and
  an **HMAC** under a persistent `QE_AUDIT_SIGNING_KEY`. Appends serialised under an `index_lock`-style mutex via
  `atomic_write`. `GET /api/audit` is paginated with chain-verification status. **Fail-closed:** refuse to enable
  production-seal capability if the signing key is unset/ephemeral (mirrors `check_session_secret_policy`). Chain
  + HMAC is tamper-*evidence* (sufficient for v1); external WORM checkpointing of the chain head is a follow-up.
- **Governance↔lineage without breaking determinism:** approver identity is **never** embedded in
  `VintageContent` (its `content_hash` covers the whole struct incl. lineage, so embedding post-hoc identity
  would change `vintage_id` and break QE-450 **AC4** byte-identity). Instead a separate content-addressed
  **`GovernanceRecord{vintage_content_hash, pool_formula_hashes, launch_entry_hash, approval_entry_hashes[2],
  evidence_hash}`** joins governance to the reproducible hash — anyone recomputes the pool from `Lineage`,
  recomputes its content hash, and confirms two valid approvals against that exact hash.
- **Revocation** is a new append-only `revoke` entry referencing the approval's `entry_hash` **plus** an addition
  to `governance/revocations.json` that **both** the G1/promotion path **and** the production evolved-catalogue
  read path filter against — so a revoked pool becomes inert on the live path (even if previously sealed)
  **without rewriting history**. (Lean: forward-only deregistration; already-sealed vintages keep their
  immutable `formula_hash` pin.)

### 13.10 Reproducibility, supervision & blast-radius

- **`seed` REQUIRED** (diverges from `TrainParams`' optional seed); `campaign_id` = canonical-JSON SHA-256 over
  `EvolveParams`; `Lineage.input_snapshot_id` pins the market snapshot so `reproduce_from` byte-matches and
  **refuses on snapshot mismatch** (an approval must stay re-derivable). Enforce the `VintageContent::content_hash`
  hashing contract (no `HashMap` in the hashed struct; a `serde_json` major bump bumps the format version) with a
  golden-hash test.
- **Per-run wall-clock deadline** (~24h hard ceiling) wrapping the stdout drain in `tokio::time::timeout`, reusing
  the existing `abort → kill_on_drop → terminally-mark` pattern; a **separate `QE_SERVER_MAX_EVOLVE_CONCURRENCY`
  semaphore (default 1)** so a multi-hour campaign never starves interactive backtests; an authz'd **Halt**.

### 13.11 API surface

| Method | Path | Purpose | AuthZ |
|---|---|---|---|
| POST | `/api/runs` `{type:"evolve"}` | Launch a campaign (`validate_evolve`; production refused if `const<REQUIRED`) | `require_role(Operator)` |
| GET | `/api/runs/{id}` · `/api/runs/{id}/archive` | Campaign progress + ~45-cell archive snapshot | session |
| POST | `/api/runs/{id}/halt` | SIGTERM a runaway campaign → terminal `halted` | `require_role(Operator)` |
| GET | `/api/formula-pools[/{id}]` | List / detail a frozen pool (formulas + deflation + gates) | session |
| POST | `/api/formula-pools/{id}/approve` | First/second sign-off (append-only) | `require_role(Approver)`, `≠ launcher` |
| POST | `/api/formula-pools/{id}/seal` | Server re-derives `seal_allowed`; 409 + named blockers on any failure | `require_role(Approver)` |
| POST | `/api/formula-pools/{id}/reject` · `/revoke` | Audited single-approver rejection / revocation | `require_role(Approver)` |
| GET | `/api/audit` | Paginated, chain-verified audit trail | session (admin for `role_change` detail) |

### 13.12 Acceptance criteria (UI/ops)

1. A crafted direct `POST /seal` that skips the SPA **cannot** seal a sandbox or un-deflated pool: authz from the
   session `AuthedEmail`, mode/prereqs from the compiled const + recorded run mode, all deflation clauses
   re-derived from the hash-verified `pool.json`, two distinct signatures — and every attempt (incl. 409) is an
   immutable audit entry.
2. In sandbox mode no code path `POST`s an approve/seal; the research artifacts root is never listed by
   `GET /api/vintages` and never loaded by runtime; an attempt to load a sandbox-identity pool in production fails
   at `assert_schema`.
3. Production seal requires two distinct approver identities, neither the launcher; a single approver, or two
   identical, cannot seal.
4. Every hard-block stat that gates `seal_allowed` is also displayed on PoolReview **and** captured in the audit
   `evidence_hash`; `GateSnapshot` carries `pbo/variance_trials/distinct_evaluations`.
5. The run stays 4-state `RunStatus` and terminates at `succeeded`; pool governance state is reconstructable by
   replaying the signed audit log.
6. Re-running a campaign from its `campaign_id` + pinned `input_snapshot_id` reproduces the pool byte-identically;
   `reproduce_from` refuses on snapshot mismatch.
7. `firewall`/`dependency_topology` stays green (no `qe-server → qe-runtime/qe-venue` edge) with the pool crate
   added; the audit signing-key fail-closed policy refuses production-seal capability on an ephemeral key.

### 13.13 Open questions (UI/ops)

1. **`DEFLATION_BASIS_VERSION` — scalar or a 5-bit set** of the prereq tickets, so a partial landing is
   representable and the eligibility report names exactly which prereq is missing? *(Lean: bitset.)*
2. **Prereq-ready signal source** — a linked const vs a `qe-cli evolve --capabilities` runtime probe (avoids the
   server needing a fresh build to see the flip)? Must be server-derivable, not operator-editable.
3. **Uncensored PBO numeric bar** — QE-450 makes it primary but names none; propose **0.5** pending a shuffle-null
   calibration study.
4. **Does the second approver re-acknowledge all K cards**, or a lighter confirmation of the first's rationale?
   (friction vs safety trade-off).
5. **Heatmap cell fill** — `best_fitness` (continuous) vs champion DSR (honest, deflated) vs occupancy; maybe a
   toggle.
6. **Break-glass emergency revoke** bypassing dual sign-off for pulling a live-but-dangerous vintage — needed?
   How rate-limited + audited?
7. **`GET /api/audit` visibility** — all-authenticated-read (identities + evidence hashes) vs admin-only for
   `role_change` detail.
8. **Is sealing synchronous** or itself a supervised `seal` subprocess (keeps heavy verification in the
   deterministic CLI)?

### 13.14 Key risks (UI/ops)

| Risk | Severity | Mitigation |
|---|---|---|
| `mode` treated as a UI toggle / request field and the seal writes the production repo — a sandbox pool reaches production (the catastrophic mode). | **blocker** | The three structural barriers of §13.6, all fail-closed. |
| Client-side gating mistaken for the real control; operator `POST`s approve directly. | **blocker** | Every gate enforced server-side (§13.1, §13.7); SPA gating is only ergonomics. |
| PBO shown over the **censored** top-N population, understating overfitting. | **blocker** | Hard-block when `variance_trials < distinct_evaluations`; the evolve job feeds the full-population matrix into `pbo_cscv`. |
| `N == cells·gens·windows` floor renders as a plausible green count while QE-439 isn't wired. | **blocker** | Show N/floor ratio; `N == floor` ⇒ hard-block "blind floor". |
| Single flat allowlist — anyone who can launch can also approve. | **blocker** | RBAC role layer + `require_role(Approver)` **before** the endpoints are exposed. |
| Role in the cookie ⇒ a revoked approver keeps power for the session TTL. | **blocker** | Resolve authorization per-request; the cookie authenticates identity only. |
| Review leads with in-sample Sharpe ⇒ the beautiful-backtest-dies-live rubber-stamp. | **blocker** | Invert hierarchy: cost-stress/turnover/capacity are the headline traffic-lights; Sharpe/DSR demoted; approve inert until guardrails green. |
| No per-run wall-clock cap ⇒ a runaway multi-hour campaign starves the ops UI. | **blocker** | Per-run deadline + separate `evolve` semaphore. |
| Alarm fatigue — everything red trains click-through. | major | Neutral resting state; BLOCK vs WARN mechanically distinct; group into 3 summary gates (Deflation / Tradability / Readability). |
| Same G1 holdout consulted across many campaigns → silent multiple testing. | major | Holdout-consultation budget; escalate threshold or refuse past budget. |
| Hash-chain is tamper-evident, not tamper-proof (attacker with data dir + key). | major | Keep `QE_AUDIT_SIGNING_KEY` off the operator hosts; checkpoint the chain head to external WORM. |

---

*Generated by two 13-agent design panels (6 specialists × 2 rounds + chair, ×2). Every claim is grounded in specific
repo files; dissents are preserved. This is a design proposal, not a code change.*
