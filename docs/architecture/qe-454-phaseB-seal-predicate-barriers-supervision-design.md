# QE-454 Phase B — server-authoritative seal predicate + structural barriers + deflation fold-in + supervision

*Final phase of the final ticket of the QE-430..454 program. Builds on QE-451 (GP engine + Phase-1b deflation),
QE-452 A+B (routes + pool lifecycle + fail-closed seal), QE-453 (SPA), QE-454 Phase A (RBAC + tamper-evident audit
+ `GovernanceRecord`). Spec of record: `qe-450-gp-indicator-evolution-design.md` §13.5–§13.7, §13.10.*

## 0. The one-line invariant

A production pool is sealable **only** as a server-recomputed AND over three inputs — the hash-verified
`pool.json`, the audit-log replay, and the compiled `DEFLATION_BASIS_VERSION` — with **no request field feeding
the decision**. Any failure is a `409` carrying a **named blocker list** and an **appended rejected-attempt audit
entry**. Sealing marks the pool sealed + records a `GovernanceRecord`; it **never mints a vintage**.

## 1. `DEFLATION_BASIS_VERSION` compiled const (§13.6 barrier 1)

`qe_validation::DEFLATION_BASIS_VERSION` is a **5-bit prerequisite bitset** (design §13.13 open-q1 "lean: bitset"),
one bit per landed prerequisite ticket — `QE-430` (ensemble-correlation deflation), `QE-432` (slow reference
oracle), `QE-434` (IC screening), `QE-436` (parsimony/MDL), `QE-439` (DSR trial basis). `REQUIRED_DEFLATION_BASIS`
is all five bits set. The prereqs are **ALL MERGED** (provable from `Lineage.code_commit` — each landed on `main`
before this phase; see recent `git log` QE-430..449), so the const is set to the fully-satisfied value and
`const >= REQUIRED` holds. It is a **source const**, gating the seal route only — it feeds **no hashed vintage
field** (golden-safety §8).

`validate_evolve` rejects `mode: production` with **`400`** when `!deflation_basis_satisfied()` (`const < REQUIRED`)
— a tampered client cannot even **launch** a production campaign. The artifact's own `deflation.gp_aware` (from the
real trial-counter path) is the **primary** guard inside `seal_allowed`; production requires **both**
`gp_aware && const`.

## 2. The three structural barriers (§13.6) — each independently fail-closed

1. **The const** (above) — no production *launch* without the compiled prerequisite bitset.
2. **Physically separate research artifacts root** `data/artifacts/research/pools`. `GET /api/vintages`
   (`read.rs`) lists the **vintage** repository (`<artifacts>/vintages`) only — a different directory subtree — so
   a sandbox/research pool is off the production load path by directory boundary no flag can flip. Test:
   `GET /api/vintages` never surfaces a research pool even when one exists under `research/`.
3. **Fail-closed `assert_production_eligible`** (`qe_formula_pool`) reusing the exact-match-on-a-hashed-identity-
   field discipline of `qe_vintage::schema::assert_schema`: a pool's `mode` is a **hashed** content field, so a
   sandbox-identity pool copied into the production dir still verifies its content hash (it is a real pool) but
   **fails `assert_production_eligible`** because its sealed `mode == Sandbox` — and the mode cannot be flipped
   without breaking `content_hash`. The production repository load path calls it, so a sandbox pool is
   **structurally unloadable** in production.

## 3. `seal_allowed` — the server-authoritative predicate (§13.7)

`POST /api/formula-pools/{id}/seal` runs `seal_allowed(pool[hash-verified], audit_replay, DEFLATION_BASIS_VERSION)`
in `spawn_blocking` (under the QE-425 deadline). It requires **all** of §13.5's eight hard-blocks **plus**
`mode == production`, the const satisfied, and **two distinct valid approver signatures (neither == launcher)**.
Approval is **re-derived from `pool_hash`-bound `approve` audit events** via `AuditLog::derive_signoff` — the stored
`review.json`/governance-cache status is **never** trusted; a mismatched `pool_hash` invalidates every signature.

Any failure ⇒ `409` + a **named blocker list** + an **appended rejected-attempt audit entry** (`AuditAction::Reject`
bound to `pool_hash`, `evidence_hash` = the evidence digest). Success ⇒ mark the governance cache `Sealed`, append
an approve/seal-evidence entry, and record a `GovernanceRecord` — **no vintage is minted**.

### The eight hard-blocks (every ABSENT stat is a BLOCK — never a vacuous pass)

Blocks 1–4 read the pool's `DeflationSummary` (the QE-451-Phase-1b `assess_gp_champion` output); blocks 5–8 read an
**optional per-formula `FormulaGateEvidence` block** (absent-by-default; **absent ⇒ block**):

1. `gp_aware == true` AND `distinct_evaluations > cells·gens·windows` floor. `N == analytic_floor` **exactly** ⇒
   block ("QE-439 not wired — blind floor"). Enforced from `gp_aware`, `distinct_evaluations`, `analytic_floor`.
2. **Finite `E[maxSharpe]`** via the log-N path — guards the `dsr.rs` `+∞` bug at `n ≳ 4.5e15`. Enforced:
   `expected_max_sharpe` finite and `> 0` (the QE-439 log-N path self-caps near `√(2 ln N)`).
3. **Uncensored PBO ≤ threshold (0.5)** — PRIMARY. Estimated over `variance_trials ≥ distinct_evaluations`; a
   **censored** (top-N) population (`variance_trials < distinct_evaluations`) ⇒ block; **absent** PBO ⇒ block.
4. **DSR ≥ 0.95** (necessary-not-sufficient floor).
5. Every formula: **IC two-fold same-sign + BH-FDR pass** (QE-434).
6. Every formula: **cost-stress `min{1×,2×}` net log-growth finite & > 0**, realised turnover ≤ `0.25·n_bars`,
   capacity ≥ `$250k`.
7. Every formula: **within MDL / node / depth / lookback caps** (QE-436), deflated vs its **own node-count stratum**.
8. Every formula: **passes its turnover-matched random-entry null** (`nulls.rs`) — no "SCRAPES NOISE".

### The N* / uncensored-PBO fold-in (the QE-451-Phase-1b carry-forward)

The DSR axis is honest **only** with the calibrated basis. Blocks 1+3 are enforced exactly as the deflation code
computes them: the pool's `DeflationSummary` is produced by `assess_gp_champion` (uncensored PBO over the full
evaluated population via `pbo_cscv`, DSR deflated against `N = max(distinct, floor)` — and, per `calibrate_null_basis`,
the conservative `N*`). `seal_allowed` **does not seal on the analytic floor alone**: hard-block 1 requires
`distinct_evaluations > floor` and hard-block 3 makes uncensored PBO (not the DSR floor) the primary gate.

## 4. Carry-forward #1 (SECURITY, load-bearing) — bind `run_id → pool_id`, always resolve the launcher

Phase A wrote the live evolve-launch audit entry `subject_hash="" , run_id=<uuid>` (run-bound), but
`launcher_for_pool` matched on `pool_id`, so the `/approve` SoD 403 was **inert on the live path**. Phase B resolves
the launcher at seal (and approve) time by the chain **pool_id → run → launch entry → launcher**:
`RunStore::find_run_id_by_pool(pool_id)` finds the run whose `meta.train.pool == pool_id` (`campaign_id == pool_id`),
then `AuditLog::launcher_for_run(entries, run_id)` reads that launch entry's actor. `seal_allowed` **always passes a
resolved launcher** to `derive_signoff` — never `launcher = None` (which would exclude NOBODY and let the launcher
self-approve, defeating SoD). An **unresolved launcher is a BLOCK** (fail-closed). The `/approve` handler resolves
the launcher the same way, so the SoD `403` now **fires on the live path**, not only in tests.

AC5 runtime-side revocation filtering reuses the Phase-A `Revocations` leaf on the server read paths
(`/api/formula-pools` already marks `revoked`); a revoked pool cannot seal (`is_revoked(pool_hash)` ⇒ block).

## 5. `Displayed = enforced = evidenced` (§13.5) — GateSnapshot additions, absent-by-default

`uncensored_pbo` + `variance_trials` + `distinct_evaluations` are added to the server `GateSnapshot` and the
`ProgressLine::Gate` wire line as `Option`/`#[serde(default, skip_serializing_if = "Option::is_none")]` (the QE-444
pattern). The evolve/GP path populates them; the **normal train path passes `None`**, so the emitted `gate` line and
the stored `GateSnapshot`/`TrainProgress`/`meta.json` serialise **byte-identically** to today. The PoolReview shows
the same numbers from the pool's `DeflationSummary`, `seal_allowed` enforces them, and the audit `evidence_hash`
captures them — one set. `evaluate_g1`'s verdict for the non-evolve path is **unchanged** (its `RobustnessReport`
already carries `pbo` + `variance_trials`; no criterion logic changes).

## 6. Run supervision & blast-radius (§13.10)

- **Per-run wall-clock deadline** (~24h hard ceiling, `QE_SERVER_MAX_RUN_SECS`) wraps the stdout drain in
  `tokio::time::timeout`, reusing the existing `abort → kill_on_drop → terminally-mark` pattern: a run past the
  ceiling is terminally `failed` ("run exceeded the wall-clock ceiling"), the child killed on `Child` drop.
- **Separate `QE_SERVER_MAX_EVOLVE_CONCURRENCY` semaphore (default 1)** so a multi-hour evolve campaign never
  starves interactive backtests: evolve supervisors acquire an evolve permit **in addition to** the shared
  worker-pool permit; a second evolve run stays `queued` behind the first.
- **Authz'd Halt** — the existing `POST /api/runs/{id}/halt` is already `require_role(Operator)`-gated (§13.11).
- **Reproducibility** — `campaign_id` = canonical-JSON SHA-256 over `EvolveParams` (existing lineage id);
  `Lineage.input_snapshot_id` pins the snapshot so `reproduce_from` byte-matches; the evolve run-spec's `seed` is
  REQUIRED (QE-452).

## 7. Firewall

The deflation stats cross into the seal predicate as **DATA** (the pool's `DeflationSummary` + `FormulaGateEvidence`,
plain serde leaves), not as a `qe-wfo → qe-server` code edge. The predicate lives in `qe-server` +
`qe-formula-pool` (+ `qe-validation` for the const). No new `qe-server → qe-runtime/qe-venue` edge. `cargo test -p
qe-architecture --test firewall` stays green.

## 8. Golden-safety argument (the biggest risk)

- The new `GateSnapshot` / `ProgressLine::Gate` fields are `Option` + `skip_serializing_if` ⇒ a non-evolve train run
  emits and stores **byte-identical** bytes; `evaluate_g1` verdict is **unchanged** for the normal path.
- The new `FormulaPoolContent.gate_evidence` field is `Option` + `skip_serializing_if` ⇒ a pool without it (every
  existing/format-v1 pool) serialises byte-identically; `POOL_FORMAT_VERSION` is **unchanged**.
- `DEFLATION_BASIS_VERSION` is a **source const** feeding **no hashed field**; it does not touch the catalogue.
- **No production pool is sealed into the default catalogue and NO vintage is minted by Phase B** — sealing marks
  the pool sealed + records a `GovernanceRecord`; the catalogue-registration → train → vintage remains the existing
  train-G1 path.
- `regenerate_fixtures` → empty diff; `CATALOGUE_VERSION` / `VINTAGE_FORMAT_VERSION` / `PROTOCOL_VERSION` unchanged.
- If a hashed struct genuinely had to change, we STOP and surface it rather than moving a golden.
