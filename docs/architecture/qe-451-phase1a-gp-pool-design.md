# QE-451 Phase 1a — offline GP Expr-tree MAP-Elites pool illumination (design + evidence)

*Scope: **Phase 1a only** of the QE-450 GP-indicator epic — illuminate a tree archive cheaply under a
**trivial fixed decision head**, with clean, countable, byte-reproducible machinery. **No deflation
gates, no IC pre-screen, no cost/turnover/capacity gates, no freeze, no flow terminals** (those are
Phase 1b). Behind an explicit opt-in — **default-off**; the production catalogue/vintage is UNCHANGED,
`CATALOGUE_VERSION` does not bump, no golden moves.*

Spec of record: [`qe-450-gp-indicator-evolution-design.md`](./qe-450-gp-indicator-evolution-design.md)
— §4.2 (grammar + normalising roots), §4.3 (variation + `repair`), §4.4 (quantiser: state-in-the-tree),
§4.5 (niching / archive), §6 (invariants), §9 (Phase 1a row). Builds on the merged Phase-0 seam
([`qe-451-phase0-expr-seam-design.md`](./qe-451-phase0-expr-seam-design.md)): the `Expr` type, the
`Kernel` interpreter, `max_lookback`, and batch=streaming parity.

---

## 1. What Phase 1a delivers (and only 1a)

1. **Complete the FIR grammar** (§4.2) with the strongly-typed **normalising roots** so every tree's
   output is bounded and feeds the *existing* point-wise `Quantiser` unchanged (state-in-the-tree, §4.4).
2. **`ExprTree::repair`** — deterministic + idempotent; forces a normalising root, snaps periods/consts,
   enforces every cap, prunes deterministically, recomputes+caches lookback.
3. **Tree-aware operators** (LocalRefine / Explore / FreshRandom) on `DetRng`, reusing the `operator.rs`
   `OperatorSelector` credit bandit (in-training reward only — no OOS).
4. **`Elite<ExprTree>` MAP-Elites archive** — a **separate** archive (not the strategy archive), three
   pure-structural descriptors (family / timescale / complexity), Deep-Grid subpop 8, uniform-non-empty-
   cell parent sampling, in-sample behavioural dedup (>0.95 quantised-series correlation ⇒ reject).
5. **Illumination** under a **trivial threshold-cross decision head** (cheap, not a pooled backtest).
6. **Distinct-canonical trial count** — canonicalise (constant-fold, commutative-order-normalise, collapse
   rank-monotone wrappers) → content-hash → count distinct formulas evaluated (**including rejects**),
   emitted into the illumination's lineage record (the input QE-439's basis / 1b's deflation will consume).
7. **Determinism** — a golden mutation-stream test (pinned RNG stream + canonical eval vector) and
   same-seed archive reproducibility.

**Explicitly deferred to Phase 1b:** GP-aware DSR/PBO deflation wiring, IC pre-screen + FDR, MDL rent in
selection, cost/turnover/capacity gates, cross-asset pooled fitness, freezing K≤16 into
`CatalogueIdentity`, flow terminals. Not built here.

---

## 2. Where the code lives (firewall-safe, minimal blast radius)

The GP search lives in **`qe-wfo` + `qe-signal`** only — the QE-450 §6 firewall (search ⟂ portfolio):
*variation lives in `qe-wfo`; the pure `Expr` interpreter + grammar semantics live in `qe-signal`.* No
new crate edge is introduced (both crates already depend on each other's prerequisites), so
`check_firewall` stays green with no change.

| Concern | Crate / file | Rationale |
|---|---|---|
| Grammar: normalising roots `Rank`/`Zscore`, `is_normalising`, period lattice, const grid, caps | `qe-signal` `indicator/expr.rs` | grammar semantics belong with the `Expr` type + interpreter |
| `ExprTree` wrapper: `repair`, `canonicalize`, `canonical_hash`, `node_count`, `depth`, `lookback` | `qe-signal` `indicator/expr.rs` | pure grammar algebra; content-hash uses `sha2` (already a `qe-signal` dep) over `rust_decimal` canonical text |
| Structural classifier tree→`IndicatorFamily`, descriptors, `ExprCell`, grid | `qe-wfo` `gp/descriptor.rs` | needs `IndicatorFamily` (defined in `qe-wfo::archive`) |
| `Elite<ExprTree>` archive + behavioural dedup + uniform-cell sampling | `qe-wfo` `gp/archive.rs` | mirrors `mapelites.rs`/`archive.rs` patterns; separate storage |
| Tree operators + node traversal + driver (reuses `OperatorSelector`) | `qe-wfo` `gp/variation.rs` | search machinery; `DetRng` `task_rng(master,index)` |
| Illuminate driver, trivial head, distinct-canonical count, `IlluminationReport` + lineage | `qe-wfo` `gp/mod.rs` | offline stage entry point |

**Default-off:** nothing in the default `train`/`backtest`/`catalogue` path references `gp::` or the new
grammar roots. The offline stage is only reachable through `qe_wfo::gp::illuminate(...)` (an explicit
opt-in call). `CATALOGUE_VERSION` unchanged; `CatalogueIdentity` unchanged; schema width unchanged.

---

## 3. Grammar completion (§4.2)

Phase-0 already ships price terminals (`Close/High/Low/Volume/Typical`), `Const`, arithmetic
(`Add/Sub/Mul/Div`-protected), pointwise (`Abs/Sign/Neg`), and windowed
(`Mean/Max/Min/Std/MeanAbsDev/Delta/Lag`). Phase 1a adds the two **normalising roots** as `WinOp`s so
they ride the same `max_lookback` recursion and `Kernel` fold:

- **`Rank(x, n) → [0,1)`** (default): fraction of the `n`-window values strictly less than the current
  value, `count(v < current) / n`. FIR, lookback `= child + (n−1)`. Monotone-invariant ⇒ collapses
  equivalence classes (a distinct-count benefit).
- **`Zscore(x, n)`** clipped `[−4,4]`: `(current − mean_n) / std_pop_n`, clamped; `std == 0 ⇒ 0`. FIR,
  same lookback rule. Exact-decimal (`std_pop` reuses the golden-tested `Roll::std_pop` `sqrt`).

`WinOp::is_normalising()` ⇒ `Rank | Zscore`. Both bounded, so the **existing stateless point-wise
`Quantiser` is used unchanged** (§4.4): `Rank`→`Linear{0,1,states}`, `Zscore`→`Bands{symmetric edges}`.
`quant.rs` changes by **zero lines** — normalisation is state-in-the-tree, charged by the same lookback
recursion, and the Phase-0 ac2 proof generalises.

**Gated / excluded (grammar-wide, Phase 1a):** flow terminals (funding/OI/premium — dense forward-fill
not present, price-only); EWMA/IIR; transcendentals; expanding/cumulative/forward; window `< 5`;
`Delta(x,1)` at a root-frequency node. Window `<5` and `Delta(x,1)` are made unreachable by snapping
every window period to the lattice `{5,10,20,50,100}` in `repair`.

### Fixed lattices (countability, §4.2 / risk #6)
- **Period lattice** `{5, 10, 20, 50, 100}`.
- **Const grid** — a finite rational grid `{0, ±0.25, ±0.5, ±1, ±2, ±5, ±10, ±100}` (exact `Decimal`).
  A finite grid keeps the reachable canonical set finite ⇒ `E[maxSR]` well-posed for 1b's deflation.

---

## 4. `ExprTree::repair` (§4.3) — deterministic + idempotent

`repair(&mut self)` applies, in order:

1. **Force a normalising root.** If `root` is not `Window(Rank|Zscore, …)`, wrap: `root ←
   Window(Rank, root, DEFAULT_RANK_PERIOD=50)`. Idempotent (an already-normalising root is left alone).
2. **Snap periods** to the lattice (nearest, ties → lower) and **snap constants** to the grid (nearest)
   throughout the tree. This also enforces "window ≥ 5" and kills `Delta(x,1)`.
3. **Cap total lookback ≤ 200:** while `max_lookback > 200`, prune the deterministically-deepest
   *descendant* subtree (never the root wrapper) to a terminal.
4. **Cap depth ≤ 4 and nodes ≤ 16:** while over either cap, prune the deepest descendant subtree.
5. **Recompute + cache** `lookback = max_lookback(root)`.

Pruning replaces the first node at maximum depth (deterministic pre-order) with `Input(Close)`, strictly
reducing node count each step ⇒ terminates. The root wrapper is never pruned, so a repaired tree always
has a normalising root. **Idempotency:** `repair∘repair == repair` — after one pass the root is
normalising, periods/consts are on-lattice/on-grid, and caps hold, so a second pass is a no-op. Proven by
test `repair_is_idempotent` and per-cap enforcement tests.

**Protected-div zero convention (f):** `BinOp::Div` is the only division and is protected at eval
(`|denom| < ε ⇒ 0`); the grammar cannot express an unprotected divide, so repair has nothing structural
to change — the convention is an interpreter invariant, asserted by `protected_div_zero`.

---

## 5. Tree operators (§4.3) — all `DetRng`, node selection uniform over pre-order

Reuse the `operator.rs` `OperatorSelector` (three arms, sliding-window credit, in-training reward only):

- **LocalRefine** (exploit): constant-tweak ±one grid step, window ±one lattice step, same-family
  input-swap (swap a `Field` terminal for another price field). Mutate-then-`repair`.
- **Explore** (cell-changing): subtree crossover between two parents / subtree-replace / grow / prune.
- **FreshRandom:** ramped-half-and-half random trees to `D=4`.

All randomness via `DetRng` from `task_rng(master, index)` — thread-count-independent (QE-006). Node
selection is a uniform index over a **deterministic pre-order traversal**. The `operator.rs`
credit-firewall is reused *as a pattern* (no OOS reward reaches the bandit). Golden stream pinned by
`golden_mutation_stream_is_pinned`.

---

## 6. `Elite<ExprTree>` archive (§4.5) — separate, structural, dedup'd

Three **pure-structural** descriptor axes (window-invariant ⇒ `cell_reassignment_rate = 0`):

1. **Family** — a **structural classifier** on the tree's dominant `Field`/op mapped to the existing five
   `IndicatorFamily` variants (Volume-dominant → `Volume`; `Std`/`MeanAbsDev` present → `Volatility`;
   `Delta`/`Lag` present → `Momentum`; else smoothing/levels → `Trend`; flow → **never** in 1a). Replaces
   `family_of`'s id-prefix match, which breaks on auto-named formulas.
2. **Timescale** — `TimescaleBand::from_lookback(structural_lookback)` (reused verbatim).
3. **Complexity** — node-count band `{≤2 / 3–4 / ≥5}`, the parsimony-illuminating axis.

Grid `5×3×3 = 45` cells; Deep-Grid subpop **8**; **uniform-non-empty-cell** parent sampling (sparse
niches reproduce as often as crowded ones). **In-sample behavioural dedup:** an offspring whose quantised
series Pearson-correlates `> 0.95` with an existing elite in its **target cell** is rejected (firewall-
safe, in-sample only). This is a *separate* archive from the strategy `MapElitesArchive` — only the
descriptor-band math and the bandit pattern are reused, not the storage.

---

## 7. Illumination under a trivial fixed decision head

Per §11 open-question 1 (*lean: trivial head + IC screen for 1a; pooled backtest only for screened
survivors in 1b*), the Phase-1a fitness is a **threshold-cross** head, **not** a pooled backtest:

- Compute the tree's raw series (streaming `eval_stream`), quantise with the root-appropriate `Quantiser`.
- `signal_t = +1` if `state_t ≥ mid` else `−1` (threshold cross at the mid state).
- `fitness = mean_t( signal_t · r_{t+1} )` over warm bars, `r_{t+1} = close_{t+1}/close_t − 1` — an
  IC-like scalar. Cheap, deterministic, enough to *illuminate* the archive. No costs, no deflation — 1b's
  job.

The scalar `fitness` is `f64` (it only orders elites, never feeds a hash), computed from exact `Decimal`
returns. All hashing (canonical trial count, archive equality) is `rust_decimal`-only text.

---

## 8. Distinct-canonical trial count → lineage

Every **evaluated** tree (including dedup/…-rejected offspring) is canonicalised and content-hashed; the
number of **distinct** hashes is the trial count. Canonicalisation:

- **Constant-fold** `Unary`/`Binary` over `Const` children into a single snapped `Const`.
- **Normalise commutative operand order** — sort `Add`/`Mul` operands by a canonical key.
- **Collapse rank-monotone outer wrappers** — under a `Rank` root, strip strictly-monotone-**increasing**
  affine wrappers (`add(_, c)`, `sub(_, c)`, `mul(_, c>0)`, `div(_, c>0)`); these do not change the rank
  order so the trees are equivalent. (`Neg` is monotone-**decreasing** ⇒ not collapsed.)

The canonical `Expr` is serialised to a canonical S-expression string (constants via `rust_decimal`'s
exact `Display`) and SHA-256'd. The count is carried in `IlluminationReport { lineage: Lineage,
distinct_evaluations: u64, total_evaluations: u64, … }`. **The production `Lineage` struct is not
modified** (that would move every vintage id — a golden move); the count rides a dedicated Phase-1a
report that *contains* a `Lineage`. Test `canonical_count_collapses_equivalent_trees` proves equivalent
trees share a hash and the count is exact.

---

## 9. Invariants preserved (QE-450 §6)

| Invariant | How |
|---|---|
| FIR closure / exact `max_lookback ≤ 200` | roots are FIR windows; `repair` caps lookback; the Phase-0 recursion is exact; ac2 generalises to trees |
| Batch = streaming parity | trees still compile to the `Kernel` single-`update` path (Phase-0 seam) |
| No dataset-wide/adaptive fit | normalisation is a causal FIR **root node**; the point-wise `Quantiser` is unchanged (`quant.rs` +0 lines) |
| `rust_decimal`-only where it feeds a hash | canonical S-expression uses exact `Decimal` text; `f64` appears only in the ordering-only fitness/correlation |
| Determinism | all RNG via `DetRng` `task_rng`; golden mutation-stream + canonical eval vector pinned; same-seed ⇒ identical archive |
| Search ⟂ portfolio firewall | GP lives in `qe-wfo`+`qe-signal`; no new crate edge; `check_firewall` green |
| No golden moved | new stage is opt-in; catalogue/schema/`CATALOGUE_VERSION`/`CatalogueIdentity` unchanged |

---

## 10. Test plan (TDD)

- **Grammar:** `Rank`/`Zscore` bounds + FIR lookback; `is_normalising`.
- **Repair:** idempotent; forces normalising root; every cap (lookback ≤ 200, depth ≤ 4, nodes ≤ 16)
  enforced; periods snap to lattice; consts snap to grid; deterministic pruning; protected-div.
- **Operators:** repair-to-validity; `DetRng`-deterministic (same seed ⇒ same offspring); node selection
  uniform over pre-order; golden mutation stream pinned.
- **Archive:** niches by the three structural axes; subpop bounded/keeps-fittest; uniform-cell sampling
  reaches sparse cells; behavioural dedup rejects a >0.95-correlated offspring.
- **Illumination:** same-seed ⇒ byte-identical archive; reproducible across thread counts.
- **Canonical count:** equivalent trees collapse to one hash; count includes rejects; count is exact.
- **Default-off:** catalogue size/version/identity unchanged; firewall green.

---

*Phase 1a only. No deflation, no gates, no freeze, no flow terminals, default-off — the production
catalogue and vintage are byte-unchanged.*
