# QE-454 Phase A — RBAC + tamper-evident audit + GovernanceRecord (production seal stays fail-closed) — review record

*QE-454 epic (P1 ops-safety), Phase A of 2 — the identity/audit/governance-record substrate. Phase B = the seal predicate + barriers + N* fold-in + supervision.*

- **PR**: https://github.com/aoimasu/quant-engine/pull/158 (squash-merged)
- **Branch**: qe-454-pa/rbac-audit-governance
- **Implementation commit**: `7bb9df3`
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.8 (RBAC & separation of duties), §13.9 (tamper-evident audit + governance↔lineage binding), §13.3 (lifecycle)
- **Evidence note**: `docs/architecture/qe-454-phaseA-rbac-audit-governance-design.md`
- **Builds on (merged)**: QE-452 Phase B (the `require_role` seam + fail-closed seal it hardens), QE-451/453

## Acceptance criteria (Phase A) — all met
- [x] **Authoritative `require_role(Role)`**: per-request from `QE_ROLE_OPERATORS/APPROVERS/ADMINS` allowlists (via `parse_allowlist`), keyed by session-derived `AuthedEmail` — NEVER cookie/body (revocation effective next request).
- [x] **Dual sign-off / SoD**: two DISTINCT approvers ≠ launcher; re-derived from `pool_hash`-bound events (NOT `review.json`); `pool_hash` mismatch invalidates signatures. *(See finding 1 — live launcher anchor is run-bound; Phase B must bind it.)*
- [x] **Tamper-evident audit log** `<data_dir>/audit/log.jsonl`: `entry_hash=SHA256(canonical‖prev_hash)` + HMAC under `QE_AUDIT_SIGNING_KEY`; mutex + `atomic_write` append; `GET /api/audit` paginated + chain-verification; FAIL-CLOSED (production-seal capability refused if the key is unset/ephemeral).
- [x] **`GovernanceRecord`** — separate content-addressed leaf OUTSIDE `VintageContent` (AC4 byte-identity preserved).
- [x] **Revocation** — append-only `revoke` entry + `governance/revocations.json`, forward-only; server read-path marks `revoked:true`. *(Runtime-side live-path filtering is Phase B — finding 2.)*
- [x] **`/api/me` capabilities** — `{canLaunch,canApprove}` UX-only (server enforces regardless).
- [x] **Golden-safety + fail-closed** — `VintageContent`/`vintage_id` byte-identical; `regenerate_fixtures` → empty; versions unchanged; production sealing STILL fail-closed.

## Implementation
- **require_role** (`server/src/auth/mod.rs`): authoritative, per-request from env allowlists via `parse_allowlist`, read from `AuthedEmail`; added `admins`+`is_admin`.
- **Dual sign-off** (`server/src/audit.rs`): pure `AuditLog::derive_signoff(entries, pool_hash, launcher) → SignoffState`; `/approve` enforces launcher≠approver (403); existing 5-state `PoolLifecycleState` untouched (modeled as an audit predicate).
- **Audit chain+HMAC** (`server/src/audit.rs`): `<data_dir>/audit/log.jsonl`, `entry_hash=SHA256(canonical‖prev_hash)` + HMAC, mutex+`atomic_write`; `GET /api/audit` paginated + `ChainStatus`.
- **GovernanceRecord** (`formula-pool/src/governance_record.rs`): separate content-addressed pure leaf, OUTSIDE `VintageContent`.
- **Revocation** (`pools.rs` + `Revocations`): append-only `revoke` + `governance/revocations.json`; `is_revoked` filter on the read path.
- **/api/me** (`auth/mod.rs`): UX-only capabilities computed server-side.

## Review verdict — [Approved] (0 blocking, 3 non-blocking Phase-B carry-forwards), reviewer on `7bb9df3` (security-critical files read directly; full cargo gate re-run; byte-identity + regenerate independently confirmed; a subagent swept pools.rs wiring + the 8-test HTTP suite + firewall)
1. **Roles non-spoofable** — resolved per-request from env allowlists keyed by session-derived `AuthedEmail`; zero request-supplied role/capability trust in the diff; forged `x-role` header + `{"role":...}` body still → 403.
2. **Audit chain tamper-evident** — `verify_chain` returns `BrokenAt{seq}` on any field/HMAC mutation; fail-closed capability under an ephemeral key.
3. **Dual sign-off SoD** — distinct approvers excluding the launcher, re-derived from `pool_hash`-bound events; `pool_hash` mismatch invalidates; `/approve` 403s launcher-as-approver.
4. **Vintage byte-identity (AC4)** — GovernanceRecord a separate leaf outside `VintageContent` (untouched); golden test proves `content_hash`/`vintage_id` byte-identical; regenerate → empty; versions unchanged.
5. **Revocation forward-only** — append-only revoke entry + `revocations.json`; read-path marks `revoked:true`; no history rewrite.
6. **Phase A did NOT open sealing** — production `/seal` 409 byte-unchanged + re-asserted; `/api/me` capabilities UX-only (NOBODY still 403s).

Green gate on `7bb9df3`: fmt/clippy(both)/**1025 passed, 2 ignored**/deny/firewall all green; regenerate empty.

## Phase-B carry-forwards (non-blocking here; MUST be addressed in Phase B)
1. **[SECURITY — LOAD-BEARING] Bind `run_id→pool_id` and always resolve the launcher.** The live evolve-launch audit entry is written `subject_hash=""` (run-bound), but `launcher_for_pool` matches on `pool_id`, so today the `/approve` SoD 403 is inert on the LIVE path (fires only in tests). Harmless in Phase A (no seal opens), but **Phase B's `seal_allowed` MUST bind `run_id→pool_id` and always pass a resolved launcher to `derive_signoff`** — note `derive_signoff(..., launcher=None)` excludes NOBODY, so an unresolved launcher would let the launcher self-approve and defeat SoD. This is the #1 Phase-B requirement.
2. **AC5 runtime-side filtering is Phase B.** The shared `Revocations` leaf + `is_revoked` ship here (drop-in ready); the G1/promotion + evolved-catalogue live-path filtering lives in the runtime/wfo crates (correctly out of this diff per the firewall) and must be wired in Phase B.
3. **`/api/audit` is session- not admin-gated** despite the `admins` role existing — Phase B should decide whether audit reads require `QE_ROLE_ADMINS`.

## Phase status
- Phase A (RBAC + audit + GovernanceRecord substrate) — **delivered** (this record).
- Phase B (the server-authoritative `seal_allowed` predicate + §13.5 eight hard-blocks + `DEFLATION_BASIS_VERSION` const + §13.6 three structural barriers + PBO/`variance_trials`/`distinct_evaluations` on `GateSnapshot`/`evaluate_g1` + the QE-451-Phase-1b `N*` deflation fold-in + run supervision + the run→pool launcher binding above) — pending.
