# QE-452 Phase B — formula-pool server routes + pool lifecycle (production-seal fail-closed) — review record

*QE-452 epic, Phase B of 2 — **the phase that COMPLETES the QE-452 epic** (server surface + durable pool lifecycle on top of the Phase-A artifact/job).*

- **PR**: https://github.com/aoimasu/quant-engine/pull/156 (squash-merged)
- **Branch**: qe-452-pb/server-routes-pool-lifecycle
- **Implementation commit**: `34e73f7`
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.2 (Server bullet), §13.3 (two lifecycles: run vs pool), §13.6 (sandbox↔production barriers — the fail-closed default)
- **Evidence note**: `docs/architecture/qe-452-phaseB-server-routes-pool-lifecycle-design.md`
- **Builds on (merged)**: QE-452 Phase A ([`qe-452-phaseA.md`](./qe-452-phaseA.md)), QE-451 GP engine

## Acceptance criteria (Phase B — completes QE-452) — all met
- [x] **Read routes** (in `protected_routes`): `GET /api/formula-pools` (summaries), `GET /api/formula-pools/{id}` (K S-exprs + deflation + lineage + lifecycle, from verified `FormulaPool::load`, 404 on miss), `GET /api/runs/{id}/archive` (heatmap cells + trial-count-vs-bar for the QE-453 CampaignMonitor).
- [x] **Pool lifecycle state machine** (§13.3, durable pool lifecycle distinct from the ephemeral run lifecycle): `Draft→Approved→Sealed` + `Rejected`/`Revoked`, guarded transitions rejecting every illegal edge; persisted alongside the pool artifact, NOT in the run.
- [x] **Governance route handlers** (in `protected_routes`, each behind a `require_role` seam): `POST /api/formula-pools/{id}/{approve,reject,revoke,seal}`, `POST /api/runs/{id}/halt`.
- [x] **Production seal FAIL-CLOSED** until QE-454: production-mode `/seal` refused with a structured 409 before any state change; a sealed pool NEVER auto-mints a vintage.
- [x] **`require_role` seam** on governance routes (placeholder; authoritative RBAC + `seal_allowed` + audit + `GovernanceRecord` deferred to QE-454, documented at the seam).
- [x] **Firewall**: route/lifecycle code stays server + pool + vintage/validation; no `qe-runtime`/`qe-venue` regression; non-vacuous.
- [x] **Golden-safety**: no vintage minted / no catalogue registration; `regenerate_fixtures` → empty; `CATALOGUE_VERSION`/`VINTAGE_FORMAT_VERSION`/`PROTOCOL_VERSION` unchanged.

## Implementation
- **Read routes** (session-gated): summaries over both roots (hash-verified on load, carrying lifecycle); one-pool detail from verified `FormulaPool::load` (404 on miss); `runs/{id}/archive` served from a run-dir `archive.json` the evolve job now writes via a new non-hashed `EvolveArchive` sidecar DTO in the `qe-run-protocol` leaf.
- **Pool lifecycle** (`qe-formula-pool` `lifecycle.rs`): 5 legal edges, everything else `Illegal` with no partial mutation; missing record → `Draft` (fail-closed); persisted via `PoolGovernanceStore` at `<data_dir>/governance` (with the pool, not the run).
- **Fail-closed production seal** (`server/src/pools.rs::apply_transition`): `Seal + Production → 409` returns BEFORE any governance read/write; `mode` comes from the hash-verified `pool.content`; NO vintage-mint/catalogue-register anywhere in the diff. Load-bearing test approves the production pool first, then asserts 409 + pool stays `approved` (never `sealed`).
- **`/halt`**: reuses the QE-407 `registry`→`abort()`→`kill_on_drop` drain verbatim (no new kill path); `RunStatus` stays 4-state → halt = terminal `Failed` + "run halted by operator request"; test drives a running evolve run to halted and asserts the reason.
- **`require_role` seam**: fail-closed boot-resolved `RoleConfig` allowlists (`QE_ROLE_OPERATORS`/`QE_ROLE_APPROVERS`), nested inside `require_session`; all 8 new routes 401 unauth, governance routes 403 role-less, reads pass. Positioned so QE-454 hardens in place without moving routes.
- **Firewall**: `qe-server → qe-formula-pool` non-vacuous parsed edge; no runtime/venue regression; pool crate leaf-pure.

## Review verdict — [Approved] (0 blocking, 4 non-blocking), reviewer on `34e73f7` (independently traced to code + non-vacuous tests; full local gate re-run)
1. **Fail-closed production seal (the BLOCKING criterion) is genuinely safe.** The `Seal + Production → 409` guard returns before any governance read or write; `mode` from the hash-verified `pool.content`; no vintage-mint/catalogue-register in the diff; load-bearing test confirms the pool stays `approved`.
2. **Lifecycle** — 5 legal edges, all else `Illegal` with no partial mutation; missing record → `Draft` (fail-closed); persisted under `<data_dir>/governance` (not the run).
3. **`require_role` seam** — fail-closed, nested inside `require_session`; 8 new routes 401 unauth, governance 403 role-less, reads pass; hardenable in place for QE-454.
4. **`/halt`** — reuses QE-407 drain verbatim (no new kill path); `RunStatus` 4-state; pool `load` still verifies (tampered pool → 404).
5. **Golden-safe** — `regenerate_fixtures` → empty diff; `CATALOGUE_VERSION`=1/`VINTAGE_FORMAT_VERSION`=7/`PROTOCOL_VERSION`=2 unchanged; `EvolveArchive` is a non-hashed sidecar written after the seal; firewall non-vacuous.

Green gate on `34e73f7`: fmt/clippy(both)/**1007 passed, 2 ignored**/deny/firewall all green; regenerate empty.

### Non-blocking notes (accepted; all QE-454 carry-forwards)
1. Test-hardening: add an explicit empty-vintage-root assertion to the seal test (the "no vintage minted" property is currently implied by the absence of a mint path).
2. `runs/{id}/archive` is session-gated but not role-gated (a read; acceptable — QE-454 may choose to role-gate archive reads).
3. The seal guard covers only the `Seal` transition (correct today; QE-454's authoritative `seal_allowed` should be the single choke point).
4. `PoolGovernanceStore` keys by `pool_id` only (QE-454's tamper-evident audit + `GovernanceRecord` will extend this).

## Epic status — QE-452 COMPLETE
- Phase A (protocol + `qe-formula-pool` artifact + evolve CLI job) — delivered ([`qe-452-phaseA.md`](./qe-452-phaseA.md)).
- Phase B (server routes + durable pool lifecycle) — **delivered** (this record). **The QE-452 server-integration epic is complete.**
- Deferred: QE-453 (the `evolve/` SPA consuming these endpoints), QE-454 (authoritative RBAC + server-authoritative `seal_allowed` + `DEFLATION_BASIS_VERSION` gate + tamper-evident audit + `GovernanceRecord` + run supervision + folding the QE-451-Phase-1b `N*` deflation carry-forward into the production seal).
