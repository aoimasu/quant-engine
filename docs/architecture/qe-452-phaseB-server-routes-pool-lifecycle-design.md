# QE-452 Phase B — formula-pool server routes + pool lifecycle (production-seal fail-closed) — design/evidence note

*QE-452 epic, Phase B of 2 (COMPLETES the epic). Phase A (protocol + `qe-formula-pool` artefact + `evolve`
CLI job) is merged. This phase adds the **server HTTP surface** for pools + the **durable pool governance
lifecycle**, with production sealing **fail-closed** until QE-454.*

- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` — §13.2 (integration
  surface, "Server" bullet), §13.3 (two lifecycles: run vs pool), §13.4/§13.5 (screens/endpoints the
  QE-453 SPA consumes), §13.6 (three structural sandbox↔production barriers → the fail-closed default).
- **Phase A record**: `docs/mds/reviewed/qe-452-phaseA.md` (nit-2 carry-forward honoured: any pinned
  pool-hash literal is over rounded `Decimal` fields only).
- **Branch**: `qe-452-pb/server-routes-pool-lifecycle` (off `main` @ `9ac89e6`).

---

## 1. Scope (Phase B only)

Server read routes + governance routes + the durable pool lifecycle state machine. **No** RBAC/audit/
`GovernanceRecord`/`DEFLATION_BASIS_VERSION`/`seal_allowed` authority (all QE-454). **No** catalogue
registration, **no** vintage minting — a sealed pool NEVER auto-mints a vintage (§13.2 last bullet).

## 2. Read routes (all inside `protected_routes`, session-gated; no role)

| Route | Shape | Source |
|---|---|---|
| `GET /api/formula-pools` | `[PoolSummary]` (id, mode, content_hash, pool_hash, formula count, `gp_aware`, distinct_evaluations, **lifecycle state**) ascending by id | `FormulaPoolRepository::list` over BOTH roots (research + production), each hash-verified on load; lifecycle from the governance store |
| `GET /api/formula-pools/{id}` | `PoolDetail` (the K canonical S-exprs, the full deflation summary, review lineage, lifecycle + transition history) | `FormulaPoolRepository::load` (**verifies** — never serves an unverified pool); 404 on missing id |
| `GET /api/runs/{id}/archive` | `EvolveArchive` (heatmap `cells[{family,timescale,complexity,node_count,best_fitness}]` + `trial_basis{distinct_evaluations,n_trials,analytic_floor,expected_max_sharpe,occupied_cells,total_cells}`) | `<run-dir>/archive.json`, written by the evolve job; 404 on unknown run / absent archive |

`GET /api/runs/{id}/archive` reuses the exact run-route auth (session via `protected_routes`) + error handling
(`spawn_blocking` fs read, 404 shape) of `GET /api/runs/{id}/result`.

**Archive producer (thin, additive, golden-safe).** The Phase-A evolve job wrote only `result.json`. Phase B
extends it to also write `<run-dir>/archive.json` (a new shared `EvolveArchive` DTO in the `qe-run-protocol`
leaf — the CLI↔server↔SPA contract home, alongside `EvolveParams`). It is built deterministically from the
illuminated `report.archive` (sorted-cell order) + the deflation report — no `f64` in any hashed artefact,
`PROTOCOL_VERSION` unchanged (a new artefact struct is back-compatible; it feeds no hashed vintage field).

## 3. Pool lifecycle state machine (§13.3 — the durable "pool" lifecycle, distinct from the run lifecycle)

The **run** stays the 4-variant `RunStatus` (`queued→running→succeeded|failed`) and terminates when the pool
artefact is written. The **pool** is a *separate resource* with its own human-paced, revocable governance
lifecycle, persisted **alongside the pool artefact**, NOT in the run.

Modelled explicitly in the `qe-formula-pool` leaf (`lifecycle.rs`) as a guarded state machine:

```
Draft ──approve──▶ Approved ──seal──▶ Sealed
  │                   │                  │
  └──reject──▶ Rejected │                │
                       └──revoke──▶ Revoked ◀──revoke──┘
```

- `PoolLifecycleState { Draft, Approved, Sealed, Rejected, Revoked }` (default `Draft`).
- `PoolTransition { Approve, Reject, Seal, Revoke }`.
- `PoolLifecycleState::apply(transition) -> Result<Self, LifecycleError>` — the ONLY legal edges are
  `Draft→Approve→Approved`, `Draft→Reject→Rejected`, `Approved→Seal→Sealed`, `Approved→Revoke→Revoked`,
  `Sealed→Revoke→Revoked`. **Every other (from,transition) pair is `LifecycleError::Illegal`** (rejected).
  `Rejected` and `Revoked` are terminal.
- Illegal edges tested one-per-edge, e.g. seal-before-approve (`Draft→Seal`), approve-after-revoke
  (`Revoked→Approve`), seal-after-reject, revoke-from-draft, re-approve-after-approve, etc.
- Persisted as `PoolGovernance { pool_id, state, history: [TransitionRecord{transition,actor,ts_ms,from,to}] }`
  via `PoolGovernanceStore` (`<data_dir>/governance/<pool_id>.json`) — the append-only history is the
  **placeholder** for QE-454's tamper-evident audit log (the authoritative pool state per §13.3). A missing
  file reads as `Draft` (fail-closed default).

Mapping to §13.3's names: `Draft`≈`PendingReview` (post-illumination), `Approved`≈post-first-signoff (the
`AwaitingSecondSignoff` dual-signoff sub-state is QE-454 RBAC territory), `Sealed`/`Rejected`/`Revoked` as named.

## 4. Governance routes (inside `protected_routes`, each carrying a `require_role` SEAM)

| Route | Transition | Role seam |
|---|---|---|
| `POST /api/formula-pools/{id}/approve` | `Approve` | `require_role(Approver)` |
| `POST /api/formula-pools/{id}/reject` | `Reject` | `require_role(Approver)` |
| `POST /api/formula-pools/{id}/revoke` | `Revoke` | `require_role(Approver)` |
| `POST /api/formula-pools/{id}/seal` | `Seal` (**production FAIL-CLOSED**) | `require_role(Approver)` |
| `POST /api/runs/{id}/halt` | cooperative halt (reuses run-cancel machinery) | `require_role(Operator)` |

Each handler: load+verify the pool (404 on miss) → **[seal only] production fail-closed check** → load current
lifecycle → `apply` transition (409 `LifecycleError` on an illegal edge) → persist state + append history
(actor = `AuthedEmail`) → 200 with the new state.

## 5. CRITICAL — production sealing is FAIL-CLOSED until QE-454

`POST /api/formula-pools/{id}/seal` refuses a `production`-mode pool with a structured **HTTP 409** body:
`{"error":"governance not yet enabled — sealing to production is gated on QE-454 (seal_allowed /
DEFLATION_BASIS_VERSION)","pool_id":…,"mode":"production"}`. The refusal happens on `pool.content.mode ==
Production` **before any state mutation** — a production pool can never reach `Sealed` in Phase B. A **sandbox**
pool may seal (`Approved→Sealed`), but that sandbox seal **cannot reach a production vintage**: sealing only
marks the pool `Sealed`; the catalogue-registration→`train`→vintage path is NOT built here (it is the
QE-451-Phase-1b-gated production path, unlocked by QE-454). **A sealed pool NEVER auto-mints a vintage**
(§13.2). Production-seal denial is a **load-bearing test**.

This is barrier-consistent with §13.6: (1) the compiled `DEFLATION_BASIS_VERSION` const gate is QE-454, but
Phase B's blanket production-seal refusal is strictly *stronger* (no production seal at all); (2) research
artefacts live under a physically separate root (`<artifacts>/research/pools`) that `GET /api/vintages` never
lists; (3) `assert_production_eligible` is QE-454 — Phase B does not load pools into any production path.

## 6. `require_role` SEAM (minimal placeholder — full RBAC is QE-454)

A `require_role`-shaped guard mirroring `require_session`: it reads the `AuthedEmail` extension (set by the
already-run `require_session`, since governance routes live *inside* `protected_routes`) and checks it against
a boot-resolved `RoleConfig { operators, approvers }` (env allowlists `QE_ROLE_OPERATORS` /
`QE_ROLE_APPROVERS`, parsed with the existing fail-closed `parse_allowlist`). Not on the list ⇒ **403**. Wired
as `require_operator` (halt) and `require_approver` (pool governance) `from_fn_with_state` layers on the
governance sub-routers only. **Documented at the seam** that the AUTHORITATIVE enforcement — real RBAC,
server-authoritative `seal_allowed`, dual-signoff (two distinct approvers ≠ launcher), tamper-evident audit,
`GovernanceRecord`, `DEFLATION_BASIS_VERSION` — is **QE-454**. Test: an authenticated request WITHOUT the role
is rejected (403) while a session-only read route with the same cookie returns 200, so QE-454 can harden it.

## 7. `/halt` — reuses the existing run-cancel machinery (no new kill path)

`RunManager::halt(id)` reuses the QE-407 shutdown-drain pattern verbatim: remove the supervisor `JoinHandle`
from the in-flight `registry`, `handle.abort()` (dropping its `Child` fires the existing `kill_on_drop(true)`),
`await` the cancellation to settle, then `terminally_mark` the run. No new signalling/kill code. The run
transitions to a **terminal** state carrying a halt reason. Because §13.12 AC5 keeps `RunStatus` **4-state**
(no new wire variant), "halted" is represented as terminal `Failed` with `error = "run halted by operator
request"` (a dedicated `Halted` variant is deliberately deferred to avoid expanding the run wire enum; the halt
reason distinguishes it). Outcomes: 200 (halted, returns new status) / 404 (unknown run) / 409 (already
terminal).

## 8. Firewall + wiring

- New crate edge: `qe-server → qe-formula-pool` (for the pool routes + lifecycle). `qe-formula-pool` stays a
  pure serde leaf (the lifecycle module adds no dep). No `qe-server → qe-runtime/qe-venue` regression; the pool
  crate stays clear of the live side (its existing rule holds).
- `crates/architecture/tests/firewall.rs`: add `("qe-server","qe-formula-pool")` to the non-vacuity
  reachable-edge list so the `qe-server` firewall rule provably covers the real new edge; the existing
  `qe-server ⊬ {runtime,venue,…}` and `qe-formula-pool ⊬ live` assertions catch any regression.
- `crates/cli/tests/dependency_topology.rs` already asserts `qe-server ⊬ runtime/venue` — unchanged, stays green.

## 9. GOLDEN-SAFETY (no golden moves)

Pure server/API + pool-lifecycle-state additions. UNTOUCHED: vintage repo, `CatalogueIdentity` default,
`CATALOGUE_VERSION` (=1), `VINTAGE_FORMAT_VERSION` (=7), the sealed pool content shape + its hash. No pool is
registered into the catalogue; **no vintage is minted** by Phase B. `PROTOCOL_VERSION` stays **2** (halt reuses
existing machinery — no new progress line; the `EvolveArchive` DTO is a new non-hashed artefact struct).
`regenerate_fixtures` → empty diff confirmed at the green gate.

## 10. Tests (non-vacuous, TDD)

Read routes: list/detail return the expected shape from a **verified** pool (research + production roots);
detail 404s a missing id; archive returns `EvolveArchive` from a run-dir `archive.json` and 404s an unknown
run. Lifecycle: legal `Draft→Approved→(sandbox)Sealed` succeeds; one test **per illegal edge** rejected.
Fail-closed: production-mode `/seal` → 409 structured governance-gated error (load-bearing), and the pool stays
un-`Sealed`. `/halt`: a running evolve run is cooperatively stopped (run goes terminal with a halt reason).
`require_role`: an authenticated-but-role-less governance/halt request → 403 while a read route → 200. Auth: an
**unauthenticated** request to every new route → 401 (from `protected_routes`).
