# QE-452 Phase A — evolve run-spec + qe-formula-pool artifact + evolve CLI job (default-off) — review record

*QE-452 epic, Phase A of 2 (protocol + frozen-pool-artifact + `evolve` CLI job backbone). Phase B = server routes + pool lifecycle.*

- **PR**: https://github.com/aoimasu/quant-engine/pull/155 (squash-merged)
- **Branch**: qe-452-pa/evolve-job-pool-artifact
- **Implementation commit**: `6028e57`
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.2 (integration surface), §13.3 (two lifecycles: run vs pool)
- **Evidence note**: `docs/architecture/qe-452-phaseA-evolve-job-pool-artifact-design.md`
- **Builds on (merged)**: QE-451 GP engine (Phases 0/1a/1b)

## Acceptance criteria (Phase A) — all met
- [x] **Protocol**: `RunSpec::Evolve(EvolveParams)` in the `qe-run-protocol` LEAF crate; `PROTOCOL_VERSION 1→2`; `EvolveParams` `seed` REQUIRED, rest `#[serde(default)]`; `EvolveMode ∈ {sandbox,production}`; v1 spec/`done` still deserializes (serde-default back-compat).
- [x] **`evolve` CLI job**: `spawn.rs` `qe evolve … --run-dir --json` arm runs the QE-451 engine (illuminate → deflation/gates → freeze K≤16), reusing QE-419 config pin + `kill_on_drop`; `Done` emits `pool: Option<String>`, NEVER `vintage`.
- [x] **`create_run` evolve arm + `validate_evolve`**: caps depth≤4/nodes≤16/lookback≤200/windows∈{5,10,20,50,100}/K≤16/seed-present/mode∈{sandbox,production}; each violation → clear 400.
- [x] **`qe-formula-pool` LEAF crate**: separate from `qe-vintage`; content = K canonical S-expr strings + deflation-summary + review lineage; reuses `Vintage` seal/verify/load SHA-256 discipline verbatim (load never yields an unverified pool; tamper fails at load); separate directory root.
- [x] **Done-never-writes-vintage invariant**: manager-level assertion + e2e test.
- [x] **Firewall extension**: pool code has no `qe-runtime`/`qe-venue` edge (asserted non-vacuously).
- [x] **Golden-safety**: `regenerate_fixtures` → empty diff; `CATALOGUE_VERSION`=1 / `VINTAGE_FORMAT_VERSION`=7 unchanged; `PROTOCOL_VERSION` feeds no hashed vintage field.

## Implementation
- **Protocol** (`qe-run-protocol` leaf): `PROTOCOL_VERSION 1→2`; `ProgressLine::Done` gains `pool: Option<String>` (skip-if-none); v1 `done` still deserializes (`pool=None`); `emit_evolve_done`; `EvolveMode{sandbox,production}`; `EvolveParams` (`seed` required, rest `#[serde(default)]`) + shared cap consts; agreement wires updated to `:2` + evolve/back-compat.
- **`qe-formula-pool`** (new pure serde leaf, no `qe-*` dep): `FormulaPool::seal/verify/load` + `FormulaPoolRepository` under a separate root, reusing Vintage's SHA-256 discipline verbatim; hashed numeric fields are string-serialised `Decimal` (byte-stable).
- **evolve job** (`jobs/evolve.rs`): illuminate → `assess_gp_champion` → `FrozenPool::freeze` → seal a pool under the mode-specific root; `spawn.rs` evolve arm; `Command::Evolve`; `Done` = `pool: Some(id)`, `vintage: None`.
- **`validate_evolve`**: `cap()` returns `Err → 400` (reject, not clamp) for all 7 violations, one test each; seed serde-required.
- **Done-never-vintage**: manager routes evolve `Done` to `train_mut(meta).pool`, `debug_assert!(vintage.is_none())` + release-mode ignore-warning; `RunSpec::writes_vintage()==false` for Evolve; e2e asserts the vintage root is never created.
- **Firewall**: `qe-formula-pool → {runtime,venue,split-live}` forbidden; non-vacuous via a real parsed `qe-cli → qe-formula-pool` edge.

## Review verdict — [Approved] (0 blocking, 3 non-blocking), reviewer on `6028e57`
1. **Lifecycle separation REAL — CONFIRMED.** `jobs/evolve.rs` constructs no `VintageRepository`; sole write is `FormulaPoolRepository::write` under the separate pool root. `writes_vintage()` false for Evolve; manager `debug_assert!(vintage.is_none())` + release-guard. Load-bearing e2e `evolve_over_fixture_seals_pool_and_never_writes_a_vintage` asserts `!vintage_root.exists()`.
2. **Pool load never unverified — CONFIRMED.** Reuses Vintage seal/verify/load verbatim (load verifies before returning); tampered pool fails at `load` with `HashMismatch` (non-vacuous); hashed fields string-`Decimal` (15-digit round-trip test, no f64 on the hashed path); `validate` rejects K>16/malformed-hash/unsorted.
3. **`validate_evolve` caps REJECT (not clamp) — CONFIRMED.** All 7 violations → 400 (one test each); seed serde-required; the CLI `k.clamp` is defense-in-depth after the server hard-reject.
4. **Protocol back-compat + leaf purity — CONFIRMED.** `PROTOCOL_VERSION 1→2`; v1 spec AND v1 `done` still deserialize (`pool=None`, tested); `qe-run-protocol` a pure serde leaf.
5. **Golden-safe + firewall — CONFIRMED.** Reviewer independently re-ran `regenerate_fixtures` → empty diff; versions unchanged; `PROTOCOL_VERSION` not a hashed vintage field; firewall non-vacuous.

Green gate on `6028e57`: fmt/clippy(both)/**985 passed, 2 ignored**/deny/firewall all green; regenerate empty.

### Non-blocking nits (accepted; recorded — not worth a fix cycle)
1. **AC wording:** `writes_vintage()==false` for Backtest too (correct — backtest *reads* a vintage); the AC should read "`writes_vintage()` true ONLY for Train". Doc wording only; code is correct.
2. **Pool `content_hash` cross-platform determinism** relies on the 12dp rounding masking f64 ULP drift in the deflation-summary stats (same QE-440/QE-443 precedent). **Carry-forward:** when Phase B / QE-454 pins a pool-hash literal into a test/fixture, pin it on rounded `Decimal` fields only (never a raw f64-derived value) so the hash is cross-platform stable.
3. Phase B (server read/governance routes + pool lifecycle) and QE-454 (RBAC, server-authoritative `seal_allowed`, `DEFLATION_BASIS_VERSION` const gate, tamper-evident audit, `GovernanceRecord`, run supervision, AND folding the QE-451-Phase-1b `N*` deflation carry-forward into the seal) are explicitly deferred — not faulted here.

## Phase status
- Phase A (protocol + `qe-formula-pool` + evolve job) — **delivered** (this record).
- Phase B (server read + governance routes + pool lifecycle) — pending.
