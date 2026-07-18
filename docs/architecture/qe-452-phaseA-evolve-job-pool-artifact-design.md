# QE-452 Phase A — `evolve` run-spec + `qe-formula-pool` artifact + evolve CLI job

**Status:** implementation evidence note (written before coding, per work-on-tickets step 1).
**Scope:** the **job → artifact backbone** only — §13.2 first four bullets + the firewall bullet of
`docs/architecture/qe-450-gp-indicator-evolution-design.md`. **Not** the server HTTP routes / governance
(Phase B), and **not** RBAC / server-authoritative seal / audit / `DEFLATION_BASIS_VERSION` const gate
(QE-454). QE-451 (the GP engine: `illuminate`/`Elite<ExprTree>`/`deflation`/`gates`/`freeze`) is already
merged and is **reused, not rebuilt**.

---

## 1. Current-state evidence (what already exists and is reused verbatim)

| Surface | File | Reuse |
|---|---|---|
| GP engine | `crates/wfo/src/gp/mod.rs` — `illuminate`, `IlluminationParams`, `IlluminationReport`, `eval_tree` | the evolve job drives this exactly |
| Archive | `crates/wfo/src/gp/archive.rs` — `ExprArchive`, `ExprElite{tree,fitness,hash,series}`, `occupied_cells`, `best_in` | cell-champion selection |
| Deflation | `crates/wfo/src/gp/deflation.rs` — `assess_gp_champion` → `GpDeflationReport`, `formula_returns`, `gp_trial_basis` | deflation-summary block source |
| Freeze | `crates/wfo/src/gp/freeze.rs` — `FrozenPool::freeze` (K≤16, sorted+dedup by `formula_hash`), `FrozenFormula{sexpr,formula_hash}`, `MAX_POOL_SIZE` | the K formulas |
| Canonical form | `crates/signal/src/indicator/expr.rs` — `ExprTree::canonical_hash`/`canonical_sexpr` (rust_decimal-only) | formula content-address |
| Vintage discipline | `crates/vintage/src/lib.rs` — `Vintage::seal/verify/load` + `VintageRepository` (SHA-256 over canonical JSON; `load` never yields an unverified artifact; tamper-load rejected) | **the exact pattern `qe-formula-pool` copies** |
| Protocol leaf | `crates/run-protocol/src/lib.rs` — `ProgressLine`, `PROTOCOL_VERSION`, `emit_*`, `BacktestParams`/`TrainParams` wire DTOs (all `#[serde(default)]`) | `EvolveParams` + `PROTOCOL_VERSION 1→2` + `pool` on `Done` |
| CLI job | `crates/cli/src/jobs/train.rs` + `main.rs` + `lib.rs` | the evolve job mirrors the train arm |
| Spawn seam | `crates/server/src/runs/spawn.rs` — `train_args` (QE-419 config pin `QE_CONFIG` + `kill_on_drop`) | the evolve arm reuses both exactly |
| Manager | `crates/server/src/runs/manager.rs` — `build_spec` + `validate_train`, `supervise`/`drain_stdout` folding the terminal `Done` | evolve arm + `validate_evolve` + Done-routes-`pool`-never-`vintage` |
| Firewall | `crates/architecture/src/lib.rs` + `tests/firewall.rs` — `FirewallRule`, `check_firewall` | add a `qe-formula-pool` rule |

Baseline versions (must stay unchanged by Phase A): `VINTAGE_FORMAT_VERSION = 7`, `CATALOGUE_VERSION = 1`.

## 2. The two lifecycles (§13.3) — why a separate crate + separate root

A **run** stays the 4-variant `RunStatus`, terminating at `succeeded` when its artifact is written. A
**pool** is a *separate resource* whose content shape (K canonical S-expr strings + a deflation-summary
block + review lineage) and lifecycle (human-paced governance, later phases) differ from a vintage — and
**runtime never loads a pool**. So the pool artifact is a **dedicated `qe-formula-pool` leaf crate** and
lives under a **separate directory root** from vintages. Phase A produces the sealed POOL artifact only;
it never mints, registers, or touches a vintage / the catalogue.

## 3. Protocol change (`qe-run-protocol`, pure leaf — serde only)

1. `PROTOCOL_VERSION: 1 → 2`.
2. `ProgressLine::Done` gains `pool: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
   Back-compat: a v1 `done` line (no `pool`) still deserializes (`pool = None`); the vintage/backtest
   forms are byte-unchanged except the version integer.
3. `emit_evolve_done(w, result, pool)` writes `{... "protocol_version":2, "pool":"<id>"}` with
   **`vintage: None`**; `emit_done`/`emit_train_done` set `pool: None` (never both).
4. `EvolveMode { Sandbox, Production }` (`#[serde(rename_all="snake_case")]`, default `Sandbox`); an
   unknown mode string is a serde reject → a clear `400`.
5. `EvolveParams`: **`seed: u64` REQUIRED** (no `#[serde(default)]` — a missing seed is a serde reject);
   **every other field `#[serde(default)]`** (`mode`, `start`/`end`/`resolution`, and the optional
   `generations/offspring/states/depth/nodes/lookback/windows/k/config/profile`).
6. Shared cap constants live here so the CLI and the server agree: `EVOLVE_MAX_DEPTH=4`,
   `EVOLVE_MAX_NODES=16`, `EVOLVE_MAX_LOOKBACK=200`, `EVOLVE_MAX_POOL=16` (K), `EVOLVE_WINDOW_LATTICE=[5,10,20,50,100]`.

`PROTOCOL_VERSION` is a protocol-crate bump; it feeds **no** hashed vintage field (`VintageContent` hashes
config/snapshot/commit/seeds/format_version — never the protocol version). Golden-safe.

## 4. `qe-formula-pool` crate shape — reuses Vintage seal/verify/load verbatim

Pure leaf: deps `serde`, `serde_json`, `sha2`, `thiserror`, `rust_decimal` — **no `qe-*` crate**, so it
trivially cannot reach `qe-runtime`/`qe-venue`.

```
POOL_FORMAT_VERSION: u16 = 1
MAX_POOL_SIZE: usize = 16                       // K ≤ 16 (mirrors freeze::MAX_POOL_SIZE)

PoolMode { Sandbox, Production }
PoolFormula      { sexpr: String, formula_hash: String }            // mirrors FrozenFormula
DeflationSummary { gp_aware: bool, distinct_evaluations: u64, n_trials: u64, analytic_floor: u64,
                   variance_trials: u64, trial_variance: Decimal, expected_max_sharpe: Decimal,
                   champion_dsr: Decimal, uncensored_pbo: Option<Decimal> }
PoolLineage      { campaign_id, seed: u64, mode: PoolMode, code_commit, input_snapshot_id, config_hash,
                   pool_hash: String }
FormulaPoolContent { format_version, pool_id, mode, formulas: Vec<PoolFormula>,
                     deflation: DeflationSummary, lineage: PoolLineage }
  ::validate()      -> K≤16, formulas strictly-ascending by formula_hash (sorted+dedup), each hash 64-hex
  ::content_hash()  -> lowercase-hex SHA-256 over canonical serde_json (same discipline as VintageContent)
FormulaPool { content, content_hash }
  ::seal(content)   -> validate + hash + pin              (mirror Vintage::seal)
  ::verify()        -> recompute == stored else HashMismatch
  ::write(w) / ::load(r)                                   // load VERIFIES before returning
FormulaPoolRepository { root }                             // SEPARATE root; write/load/list, load verifies
```

**Every hashed numeric field is `rust_decimal::Decimal`** (deflation stats), not `f64` — sidestepping the
`serde_json` float re-parse instability the vintage works around with `hash_stable`, so seal→load is
byte-stable by construction. `load` reuses the Vintage rule: a tampered artifact fails `verify` at load
(the QE-451 Phase-1b tamper-load pattern).

## 5. The `evolve` CLI job (`crates/cli/src/jobs/evolve.rs`) — mirrors `train`

`run_evolve_job(params, emit)`:
1. scan bars over `[start,end)@resolution` from the config store → `Sample::from_bar` (reuses the QE-419
   config-pinned store dir); empty ⇒ `RunError::NoBars`.
2. `illuminate(IlluminationParams{ master_seed: seed, generations, offspring_per_generation, states }, …)`
   — determinism rides the merged QE-451 `DetRng`/`task_rng` seeding (`seed` required).
3. cell champions (`best_in` per occupied cell), sorted by fitness desc, take ≤K distinct by canonical
   hash; empty ⇒ `RunError::NoElites`.
4. `FrozenPool::freeze(&champions)` (K≤16, sorted+dedup) → the K `FrozenFormula`.
5. `assess_gp_champion(population = formula_returns(champions), champion=0, distinct, cells, gens, windows=1,
   cscv_blocks=2)` → `GpDeflationReport`; convert `f64 → Decimal` (round_dp 12, non-finite→0) into
   `DeflationSummary`.
6. seal a `FormulaPool` (mode, campaign_id = `lineage.id()`, pool_hash = `frozen.pool_hash()`), write to
   the **mode-specific pool root** (sandbox → `<artifacts>/research/pools`, production → `<artifacts>/pools`;
   both **separate from `<artifacts>/vintages`**), write `result.json`.
7. terminal `emit_evolve_done(result, pool_id)` → `pool: Some(id)`, `vintage: None`. The job **never**
   constructs a `VintageRepository` or writes a vintage.

`spawn.rs` gains `evolve_args` building `qe evolve --config … --profile … --start … --end … --resolution …
--seed … [--generations/--offspring/--states/--k/--mode …] --run-dir <dir> --json`, reusing the QE-419
`QE_CONFIG` pin + `kill_on_drop(true)` exactly like `train_args`.

## 6. `create_run` evolve arm + `validate_evolve` (manager)

`build_spec` gains an `"evolve"` arm: deserialize `params` into `EvolveParams` (lenient except the
serde-required `seed`), then `validate_evolve`. Rejections (each a uniform `CreateError::Validation` → `400`,
one test per):
- `start`/`end`/`resolution` required (window is needed to scan);
- `depth > 4`, `nodes > 16`, `lookback > 200` (when supplied);
- any `windows` entry ∉ `{5,10,20,50,100}`;
- `k > 16`;
- **missing `seed`** (serde reject, wrapped as `invalid evolve params: missing field 'seed'`);
- **bad `mode`** (serde reject on an unknown mode string).

## 7. The `Done`-never-writes-vintage invariant (§13.3, load-bearing)

- `RunSpec::Evolve` reports `run_type = "evolve"`; `TrainProgress` gains `pool: Option<String>`
  (`skip_serializing_if none` — train/backtest meta unchanged).
- In `drain_stdout`, a terminal `Done` is routed by spec: **Train** folds `vintage`; **Evolve** folds
  `pool` and **asserts `vintage.is_none()`** (a `debug_assert!` + a defensive branch that logs and ignores
  any stray vintage) — an evolve run can never record a vintage.
- Test: drive the manager with a fake evolve job that emits `emit_evolve_done`; assert `meta.train.pool ==
  Some(id)`, `meta.train.vintage == None`, and the **vintage repo directory is never created/written**.
- End-to-end (CLI): `run_evolve_job` over the committed fixture store writes a sealed pool under the pool
  root and **no file under the vintage root**; the emitted `Done` carries `pool: Some`, `vintage: None`.

## 8. Firewall extension

Add `FirewallRule { upstream: "qe-formula-pool", forbidden: ["qe-runtime","qe-venue","qe-runtime-core",
"qe-hedger","qe-edge"] }` and, in `firewall.rs`, a presence + non-vacuity assertion (crate parsed; a known
real edge `qe-cli → qe-formula-pool` seen) so the rule can't pass vacuously. Because the pool crate is a
true leaf its reachable set is empty, so the rule holds; the assertion guards a future dep that would breach.

## 9. Golden-safety — no golden moved

- The pool is a **separate artifact under its own root**; the vintage repo, `CatalogueIdentity` default,
  `CATALOGUE_VERSION (1)`, and `VINTAGE_FORMAT_VERSION (7)` are **untouched** (no pool is registered into
  the catalogue in Phase A — that registration + a subsequent `train` run is a later phase).
- `PROTOCOL_VERSION 1→2` is a protocol bump only; it feeds no hashed vintage field (verified by reading
  `VintageContent`). If it ever did, STOP + return.
- Gate: `regenerate_fixtures` → empty diff; both versions unchanged; `cargo deny` adds no external crate
  (the pool crate's deps are all already in the tree).

## 10. Test plan (non-vacuous)

1. `qe-formula-pool`: seal→write→load round-trip + stable hash; **tampered pool fails `verify` at `load`**;
   K≤16 cap; unsorted/dup hashes rejected; repository round-trip under a separate root.
2. `qe-run-protocol`: `PROTOCOL_VERSION == 2`; evolve `Done` wire (`pool` present, no `vintage`); a v1
   `done` (no `pool`) still deserializes; `emit_evolve_done` byte-exact; agreement wires updated to `:2`.
3. manager `validate_evolve`: one rejecting test per cap (depth/nodes/lookback/off-lattice window/K/missing
   seed/bad mode); a valid spec accepted; evolve `Done` routes `pool` and never `vintage`.
4. CLI end-to-end evolve job: sealed pool under the pool root, none under the vintage root, `Done` carries
   `pool: Some`, `vintage: None`; same seed ⇒ same `pool_id`/`content_hash` (determinism).
5. firewall: no `qe-runtime`/`qe-venue` → `qe-formula-pool` edge.

## 11. Deferred

- **Phase B:** server read routes (`GET /api/formula-pools[/{id}]`, `/api/runs/{id}/archive`) + governance
  routes (`approve/seal/reject/revoke/halt`) + the SPA `evolve` area.
- **QE-454:** RBAC (`require_role`), server-authoritative `seal_allowed`, `DEFLATION_BASIS_VERSION` const
  gate, tamper-evident audit, `GovernanceRecord`, run supervision/deadline. **Sealing a pool must NOT mint a
  vintage** — registration of a sealed pool's K formulas into `CatalogueIdentity` + a subsequent `train`
  run is the vintage path, and is **not** Phase A.
