# QE-499 — GP evolve → G1 vintage bridge: design (v2, post design-review)

> Design doc for [QE-499](../mds/tickets/QE-499.md). **v2** folds in the 2026-08-02 design-review (4 reviewers + CTO):
> verdict **PROCEED-WITH-DESIGN-CHANGES — build Phase A only; Phase B is held** until the deflation-composition rule
> (§4) and B1–B6 are signed off by the deflation author. This version corrects 7 factual claims v1 got wrong about the
> code and pins the mandatory honesty rule. Evidence context: the 2026-08-01 edge experiment (112 runs, 0 G1 passes;
> richer *factors* did not help) established **strategy representation** as the binding constraint, justifying this lever.

## 1. Problem

`qe train` evolves a fixed **genome grammar** (k-of-4 threshold clauses over a frozen ~26-indicator catalogue) via
MAP-Elites and seals a G1-gated **vintage**. `qe evolve` evolves richer **`Expr` formula trees** and seals a **formula
pool** — but a pool **never mints a vintage**. QE-499 lets evolved formulas widen what the search can express, **without**
weakening the false-discovery control (DSR/PBO/CPCV) that makes the engine trustworthy.

## 2. Current state — verified against the code (v1 corrections in **bold**)

**Hooks that exist:**
- **`Expr` → indicator:** `compile(id, &Expr, Quantiser) -> Box<dyn Indicator>` (`crates/signal/src/indicator/expr.rs:403`).
- **Formula-pool artefact:** `FormulaPoolContent` (`crates/formula-pool/src/lib.rs:178`): `K ≤ 16` `PoolFormula`s
  (`sexpr:String` + `formula_hash`), a `DeflationSummary` **`.deflation`** block whose fields are **`n_trials`,
  `distinct_evaluations`, `analytic_floor`, `gp_aware`** (`lib.rs:72–97`) — **there is no `deflation_summary.effective_trials`
  field (v1 was wrong)** — and an optional, absent-by-default `gate_evidence` slot.
- **Vintage identity slot:** `CatalogueIdentity.formula_pool: Vec<String>` (`feature.rs:132`); the population method is
  **`with_formula_pool` (not `with_pool`)** (`feature.rs:152`). Empty ⇒ omitted from JSON (no golden move).
- **Honest deflation:** `gp_trial_basis` (`crates/wfo/src/gp/deflation.rs:30`) and `assess_gp_champion` (`:125`); a
  direct-champion `GpDeflationGate` (uncensored PBO + DSR floor) also exists (`deflation.rs:96–203`).

**What the design review REFUTED about v1 (now corrected here):**
1. **"train already computes a features-aware basis."** Only under `is_steered`; the default path uses plain
   `effective_trials(occupied_niches, gens, windows)` with `evolved_count` hardcoded `0` (`train.rs:829–838`), and a
   `--pool`-only run is **not** steered (`train.rs:564–565`). → **B1** below.
2. Crux field name (`deflation_summary.effective_trials` → **`pool.deflation.n_trials`**).
3. `with_pool` → **`with_formula_pool`**.
4. The exact-match load boundary gives tamper-*rejection*, **not acceptance**: `assert_schema` compares against
   `CatalogueIdentity::current()` built from the **empty-pool default** (`schema.rs:52–59`, `feature.rs:168–170`), so
   every pool-injected vintage is rejected as `SchemaMismatch`. → **B4**.
5. `num_states` uniformity does **not** hold — `from_specs` is last-writer-wins (`feature.rs:33–35`). → major, §5.
6. A sealed `sexpr` **cannot** be reconstructed — only `write_sexpr`/`canonical_sexpr` (Expr→text) exist; **no parser**. → **B6**.
7. This path is **not** automatically fail-closed for production (`seal_allowed` is bypassed). → **B5**.

## 3. Design — formulas are *features*, not strategies

An evolved `Expr` is an *indicator*; it becomes a candidate strategy only when the MAP-Elites genome search addresses it
in a clause. The bridge injects a sealed pool's compiled formulas into the catalogue for a train run; the **unchanged**
search builds and the **unmodified** G1 gate judges strategies over the widened feature set. Flow:
```
qe evolve → sealed FormulaPool (K≤16 Expr sexpr + .deflation{n_trials,…,gp_aware} + [gate_evidence])
qe train --pool <id>  (Phase B):
  parse each sexpr → Expr (round-trip-hash-checked, B6) → compile(formula_hash, expr, quantiser_for_root(root, states))
  CatalogueConfig.formula_pool = Vec<(id=formula_hash, Expr, Quantiser)>  (compiled INSIDE catalogue(), fixed position after price/flow)
  CatalogueIdentity.with_formula_pool(sorted formula_hashes)  ⇒ vintage id binds the exact pool
  features → MAP-Elites (unchanged) → ensemble (unchanged)
  n_trials + trial_variance = COMPOSED per §4 (the honesty crux)  ← MUST hold on the un-steered path
  G1 gate (UNCHANGED) → seal (write-but-mark non-production per B5)
```

## 4. The deflation-honesty rule (MANDATORY — CTO ruling, no exceptions)

> Whenever `formula_pool` is non-empty, the vintage trial basis is computed **unconditionally** (independent of
> `is_steered`) as the **additive sum across the two independent selection stages**:
>
> `n_trials = effective_trials_with_features(cells, gens, windows, feature_space = catalogue_width_incl_K) + pool.deflation.n_trials`
>
> - Keep the conservative `max(distinct_evaluations, analytic_floor)` **within** each stage; **SUM across** the two.
>   `max()` across stages is **forbidden** (it discards a whole stage's multiple-testing penalty when one dominates).
> - **Single count definition:** formulas are catalogue indicators → they ride `catalogue_width`; `evolved_count` stays
>   `0` (do **not** also set `evolved_count = K` — that double-counts / mis-counts).
> - **Require `pool.deflation.gp_aware == true`**; a `gp_aware=false` pool is a hard error at intake.
> - **The DSR bar is `(trial_variance, n_trials)` jointly:** `trial_variance = max(genome-population variance,
>   pool.deflation.trial_variance)` and floor `E[maxSR]` with the pool's sealed `expected_max_sharpe`.
> - **Acceptance test** runs on the **un-steered default pool path** and asserts the quantitative lower bound
>   `composed_n_trials ≥ genome_basis + pool_basis` (monotonicity is insufficient).

## 5. Design changes to fold in (from review §5)

- **Data model:** `CatalogueConfig` is `#[derive(Copy)]` with one `u16` (`mod.rs:142–146`); a `Vec<CompiledFormula>`
  breaks `Copy` and `Box<dyn Indicator>` is not `Clone`. Carry `Vec<(id, Expr, Quantiser)>` and compile **inside**
  `catalogue()`. Removing `Copy` requires auditing by-value call sites — do it deliberately.
- **`CompiledFormula` stays qe-signal-native** (id + `Expr` + `Quantiser`); the `FormulaPool → CompiledFormula` mapping
  lives in **qe-cli** (already deps both) — keeps qe-signal free of a qe-formula-pool edge.
- **Firewall (new rule):** QE-132 only forbids the reverse direction, so a `qe-runtime → qe-signal → qe-formula-pool`
  edge would pass green. **Add a rule forbidding `qe-runtime`/`qe-edge`/`qe-hedger`/`qe-venue` → `qe-formula-pool`**
  (`architecture/src/lib.rs:501–510`) so "the live path never loads a pool" is enforced.
- **Deterministic ids:** each injected indicator's id = its 64-hex `formula_hash`, appended at a fixed position after
  price/flow, so `id_hash` (`feature.rs:120–122`) is a pure function of the sorted pool.
- **Determinism (QE-496):** "pool id folded into the run-params fingerprint" is a build item with its own test (two
  configs differing only in `--pool` ⇒ distinct vintage ids).
- **`num_states` uniformity:** compile each formula with `quantiser_for_root(root_op, cfg.states)` (not last-writer-wins).
- **Version guard:** state whether the pool-composed basis bumps `DEFLATION_BASIS_VERSION` (only the new pool path is
  affected — likely no bump; call it out explicitly).

## 6. The six blockers (must be written into the design + signed off before Phase B)

- **B1 — un-steered under-deflation:** non-empty `formula_pool` ⇒ features-aware + pool-folded basis **unconditionally**
  (not gated on `is_steered`); the §4 test runs on the un-steered path.
- **B2 — composition:** pin **additive** across stages (§4); test the quantitative lower bound, not monotonicity.
- **B3 — dispersion, not just count:** compose `trial_variance` and floor `E[maxSR]` with the pool's sealed values (§4).
- **B4 — load boundary:** persist `pool_id` in the vintage/run-params; provide `current_with_pool(pool)` (or rebuild the
  widened identity from the resolved sealed pool in `assert_schema`) so a correctly-sealed vintage passes exact-match and
  a drifted/absent/tampered pool **fails closed** (a missing pool artefact ⇒ hard error).
- **B5 — production back-door:** `seal_allowed`'s per-formula hard-blocks (IC/FDR, cost-stress, turnover, capacity,
  random-entry null, `pool_seal.rs:61–165`) never run on pool→vintage→G1. Resolution (chosen): **(a)** reject any
  non-Sandbox pool at train intake **and** require `gate_evidence` present-and-passing before formulas enter a vintage,
  **and (b)** mark the pool-carrying vintage **non-production via QE-476 write-but-mark** so the live load path refuses it.
  A pool-carrying vintage is never production-sealable without re-deriving per-formula `gate_evidence` under QE-454.
- **B6 — S-expression parser:** the `sexpr → Expr` parser must round-trip exactly
  (`SHA-256(canonical_sexpr(parse(s))) == sealed formula_hash`), tested as a hash-stability property — a **Phase B**
  first-class component (or seal the `Expr` structurally with a called-out `POOL_FORMAT_VERSION` bump).
- **Majors:** require `gp_aware == true`; data-window provenance — an evolve-fit formula scored by G1 over overlapping
  bars is in-sample-contaminated regardless of counting, and `PoolLineage.input_snapshot_id` is empty (`lib.rs:167–168`);
  add a disjoint-window / embargo invariant between the evolve window and the train/holdout window.

## 7. Phase plan

- **Phase A — catalogue-injection seam (APPROVED to build now; golden-safe, inert):**
  1. Default no-pool path **byte-identical** — goldens unchanged; empty `formula_pool` omitted from JSON (`feature.rs:131`).
  2. **No train intake** — no `--pool` flag, no caller populates the field, no code path constructs a non-empty pool; a
     populated pool is **structurally unreachable** until Phase B lands the §4 accounting (closes the A→B under-deflation window).
  3. Data model per §5 (no `Copy` break shipped as churn, no `Box<dyn Indicator>` in a `Clone` struct); `CompiledFormula`
     qe-signal-native; the pool→formula mapping deferred to qe-cli (Phase B).
  4. Firewall rule forbidding live-graph → `qe-formula-pool` added and tested green.
  5. Deterministic injected-id scheme (`formula_hash`, fixed position) landed and asserted; identity changes iff pool
     non-empty (tested via a synthetic non-empty pool in a unit test only — never a real train intake).
- **Phase B — train intake + §4 deflation + B4/B6 (HELD)** — authorize only after §4 and B1–B6 are signed off by the
  deflation author.
- **Phase C — end-to-end honesty proof + re-hunt** — an evolve campaign → pool → train, with the §4 test on the
  un-steered path, determinism, firewall, governance all green. Efficacy (does an evolved vintage clear G1?) is a
  **separate** measured question; QE-499 delivers the honest mechanism, not a promised pass.

`Spec ref: QE-499 ticket + the 2026-08-02 design review (verdict PROCEED-WITH-DESIGN-CHANGES, Phase A only). Grounds: expr.rs:403, formula-pool/src/lib.rs:72–97/178, feature.rs:132/152/168, deflation.rs:30/96–203, architecture/src/lib.rs:501–510, train.rs:829–838/564. Preserves QE-006/QE-132/QE-454/QE-476; G1 gate unchanged.`
