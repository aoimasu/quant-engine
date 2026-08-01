# QE-499 — GP evolve → G1 vintage bridge: design

> Design doc for [QE-499](../mds/tickets/QE-499.md) — **design-first, review-before-implementation.** Grounded in
> the current code; distinguishes what already exists from what must be built. Evidence context: the 2026-08-01
> edge experiment (112 runs, 0 G1 passes; richer *factors* did not help) established **strategy representation** as
> the binding constraint, justifying this — the lever that lets evolved `Expr` formulas (not just the fixed k-of-4
> threshold grammar) reach the unmodified G1 gate.

## 1. Problem

`qe train` evolves a fixed **genome grammar** (k-of-4 threshold clauses over a frozen ~26-indicator catalogue) via
MAP-Elites and seals a G1-gated **vintage**. `qe evolve` evolves richer **`Expr` formula trees** and seals a
**formula pool** — but a pool **never mints a vintage**: there is no path from an evolved formula into a G1-evaluated
candidate strategy. The representation the search can express is therefore capped by the fixed catalogue, and the edge
experiment shows that cap is binding. QE-499 connects the two so evolved formulas widen what the search can express,
**without** weakening the false-discovery control that makes the engine trustworthy.

## 2. Current state — what already exists vs what is missing

**Already built (the hooks):**
- **`Expr` → indicator compilation:** `compile(id, &Expr, Quantiser) -> Box<dyn Indicator>` (`crates/signal/src/indicator/expr.rs:403`) turns a formula tree into a catalogue-equivalent indicator; `ExprIndicator`/`eval_stream` evaluate it causally with the catalogue's own quantiser and a `lookback` for purge/embargo.
- **Formula-pool artefact:** `FormulaPoolContent` (`crates/formula-pool/src/lib.rs`) carries `K ≤ 16` canonical S-expression `PoolFormula`s (`sexpr` + SHA-256 `formula_hash`), a **deflation-summary** block, and an **optional, absent-by-default `gate_evidence`** slot (the QE-454 fail-closed governance hook).
- **Vintage identity slot:** `CatalogueIdentity.formula_pool: Vec<String>` (`crates/signal/src/feature.rs:132`, QE-451 Phase-1b) — the sorted `formula_hash`es of the evolved trees **sealed into a vintage**. Empty ⇒ omitted (no golden moves); **non-empty changes the vintage identity**, and the load boundary (`assert_schema`) asserts an **exact** match — so a vintage is bound to the exact evolved formulas it used.
- **Honest deflation basis:** `gp_trial_basis(distinct_evaluations, cells, generations, windows)` and `assess_gp_champion` (`crates/wfo/src/gp/deflation.rs`) — the conservative max of the distinct-canonical-formula count and the analytic floor; train already computes a features-aware basis via `effective_trials_with_features` (QE-434).

**Missing (the bridge to build):**
1. **Catalogue injection** — `catalogue(cfg)` (`crates/signal/src/indicator/mod.rs:166`) appends only `price::` + `flow::` kernels; nothing appends a pool's compiled `Expr` formulas. `CatalogueConfig` has no pool field, and `seed_catalogue_subset` is intentionally never called by `catalogue`.
2. **Train intake** — `qe train` has no way to accept a sealed pool and thread it into the run.
3. **Deflation accounting** — the vintage's `n_trials` does not fold in the evolved-formula search's trial basis, so an unaccounted pool would **under-deflate** (inflate DSR) — the one unacceptable failure mode.
4. **Identity population** — `CatalogueIdentity.formula_pool` is defined but never populated by a train run.

## 3. Proposed design

**Formulas are features, not strategies.** An evolved `Expr` is an *indicator*; it becomes a candidate strategy only
when the MAP-Elites genome search addresses it in a clause. So the bridge injects a sealed pool's formulas **into the
catalogue** for a train run; the existing search then builds — and the existing G1 gate then judges — genome strategies
over the *widened* feature set. No new strategy representation, no gate change.

Flow (extends the QE train pipeline; new/changed parts in **bold**):
```
qe evolve  → sealed FormulaPool (K≤16 Expr, deflation-summary, [gate_evidence])
qe train --pool <pool-id>:
   load pool ──▶ **compile each PoolFormula.sexpr → Expr → compile(id, expr, q) → Box<dyn Indicator>**
             ──▶ **CatalogueConfig { …, formula_pool: [compiled indicators] }**  (catalogue() appends them)
   features (decision bars over the WIDENED catalogue)
   MAP-Elites search  (unchanged — can now select clauses on the evolved features)
   ensemble (unchanged)
   validation:  **n_trials = fold(effective_trials_with_features(base catalogue),
                                   pool.deflation_summary.effective_trials)**  ← the honesty crux
   G1 gate (UNCHANGED thresholds/criteria)
   seal:  **CatalogueIdentity.formula_pool = sorted(pool.formula_hashes)** ⇒ vintage id binds the exact pool
```

### 3.1 The deflation crux (non-negotiable)
The evolved formulas were themselves selected from many GP trials; ignoring that inflates DSR. The vintage's deflation
`n_trials` **must** be the honest composition of *both* searches' trial bases — the genome/MAP-Elites basis
(`effective_trials_with_features`) **and** the pool's own basis carried in its `deflation_summary`. Compose
conservatively (the design's existing rule: never under-deflate; prefer over-deflation). A test **must** assert that
adding a pool with a larger evolved-trial count **raises** the vintage's `n_trials` (and thus can only make DSR
*harder*), never lowers it. This is the single load-bearing invariant of the whole ticket.

### 3.2 Identity, determinism, provenance
- The compiled formula indicators enter `CatalogueIdentity.formula_pool` as their sorted `formula_hash`es → the vintage
  content hash changes iff a pool is used → determinism (QE-006) preserved; the load boundary rejects a vintage whose
  build catalogue does not carry the exact pool (fail-closed on drift).
- Compilation is deterministic (`rust_decimal`, causal, no wall-clock); the pool id is folded into the run-params
  fingerprint (QE-496) so a train-with-pool and train-without-pool never share a vintage id.

### 3.3 Firewall & governance (unchanged, must stay green)
- The evolve → pool → train path is **offline** (research); no live edge is introduced. The QE-132 firewall test must
  stay green — `qe-cli` may depend on `qe-formula-pool` (already does), but no `search→live`/`portfolio→live` edge.
- **Production sealing stays fail-closed (QE-454):** a *production*-mode pool still requires per-formula `gate_evidence`
  and the server-authoritative `seal_allowed` + audit chain. QE-499 does **not** relax this; it only lets a pool feed a
  *train*/*research* vintage. Sealing that vintage to production remains governed exactly as today.

## 4. Phased implementation (proposed; each phase independently green + reviewable)

- **Phase A — catalogue injection (pure, offline, testable without a train):** add `CatalogueConfig.formula_pool:
  Vec<CompiledFormula>`; `catalogue()` appends them; `CatalogueIdentity::with_pool` populates the hashes. Default empty
  ⇒ byte-identical to today (no golden moves). Tests: injected formulas produce non-constant, causal features; identity
  changes iff pool non-empty; exact-match load boundary.
- **Phase B — train intake + deflation accounting:** `qe train --pool <id>` loads the pool, compiles + injects, and
  **composes n_trials** from both bases. The deflation test (§3.1) is the acceptance gate. G1 unchanged.
- **Phase C — end-to-end + honesty proof:** a train over a sealed pool produces a vintage the unmodified G1 evaluates;
  a test proves a pool with N evolved trials raises `n_trials` by ≥ its basis; determinism (two same-seed runs → same
  vintage id); firewall + `cargo deny` green.

## 5. Validation / acceptance

- **Deflation honesty (the gate on merge):** adding a pool never lowers the vintage `n_trials`; a larger evolved-trial
  count strictly raises it. No path lets an evolved candidate reach the gate uncounted.
- **Gate unmodified:** no threshold / criterion / `funding_coverage` / `cv_folds` diff; the G1 code is untouched.
- **Determinism:** same (config, window, seed, budget, **pool**) ⇒ byte-identical vintage id; different pool ⇒ different
  id (QE-496 fingerprint folds the pool id).
- **Firewall + governance:** `qe-architecture` firewall green; production-seal path still fail-closed (QE-454) — a
  research/sandbox pool cannot be flipped to a production vintage without the existing governance.
- **No golden movement** on the default (no-pool) path; a sealed-schema change (if any) ⇒ single `VINTAGE_FORMAT_VERSION`
  bump with goldens regenerated intentionally.
- **Efficacy is a SEPARATE question:** this ticket delivers the *mechanism*. Whether evolved formulas actually clear G1
  is then measured by an honest re-hunt (an evolve campaign → pool → train) — and may still be "no edge." QE-499 does not
  promise a pass; it removes the representation cap so the question can be asked honestly.

## 6. Risks

- **Under-deflation (critical):** the one way to make the engine dishonest. Mitigated by §3.1 + its mandatory test;
  when in doubt, over-deflate.
- **Catalogue-width blow-up:** injecting K≤16 formulas widens the feature space the genome addresses; MAP-Elites cost
  and the `effective_trials_with_features` basis both rise — acceptable and self-correcting (a wider search deflates
  harder, per the engine's design).
- **Identity/schema drift:** the exact-match load boundary already guards this; Phase A must keep the default path
  byte-identical.
- **Scope creep toward production sealing:** explicitly out of scope — QE-499 stops at a research/train vintage;
  production sealing stays under QE-454.

## 7. Open questions (for the human / review)

1. **Compose vs replace the trial basis:** fold the pool basis into `effective_trials_with_features` additively, or take
   a max? (Design leans: the conservative composition that never under-deflates — likely additive on the multiple-testing
   count, but the exact formula needs the deflation author's sign-off.)
2. **Which pool modes may feed a train vintage** — research/sandbox only, or also production (with its `gate_evidence`)?
   Recommendation: research/sandbox for QE-499; production stays behind QE-454.
3. **Quantiser choice for injected formulas** — reuse the catalogue's uniform quantiser (as `compile` already takes one),
   or per-formula? Reuse for determinism/simplicity unless evidence says otherwise.
4. **Phase A alone** (catalogue injection, no train intake) is independently useful and fully golden-safe — ship it first
   for review, then B/C?

`Spec ref: QE-499 ticket; grounds — expr.rs:403 (compile), formula-pool/src/lib.rs (FormulaPoolContent/gate_evidence), feature.rs:132 (CatalogueIdentity.formula_pool), wfo/src/gp/deflation.rs (gp_trial_basis/assess_gp_champion), indicator/mod.rs:166 (catalogue). Preserves QE-006/QE-132/QE-454/QE-476; gate unchanged.`
