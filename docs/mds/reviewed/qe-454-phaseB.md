# QE-454 Phase B — server-authoritative seal predicate + structural barriers + deflation fold-in + supervision — review record

*QE-454 epic (P1 ops-safety), Phase B of 2 — **the final phase of the final ticket; COMPLETES QE-454 AND the entire QE-430..454 program.***

- **PR**: https://github.com/aoimasu/quant-engine/pull/159 (squash-merged)
- **Branch**: qe-454-pb/seal-predicate-barriers
- **Implementation commit**: `0f69792`
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.5 (8 hard-blocks), §13.6 (3 barriers), §13.7 (seal predicate), §13.10 (supervision)
- **Evidence note**: `docs/architecture/qe-454-phaseB-seal-predicate-barriers-supervision-design.md`
- **Builds on (merged)**: QE-454 Phase A (audit/RBAC/GovernanceRecord), QE-451 Phase 1b (the deflation code this enforces), QE-452 A+B, QE-453

## Acceptance criteria (Phase B — completes QE-454 + the program) — all met
- [x] **`DEFLATION_BASIS_VERSION` const (barrier 1)**: 5-bit prereq bitset (QE-430/432/434/436/439) = `0b11111` == `REQUIRED`; `validate_evolve_basis` rejects `mode:production` 400 when `const<REQUIRED` (names the missing prereqs); source const, feeds NO hashed field.
- [x] **Barriers 2 + 3**: (2) research pools under `<artifacts>/research/pools`, a separate subtree the production `VintageRepository` never scans; (3) fail-closed `assert_production_eligible` keys on the HASHED `mode` — a sandbox pool copied into prod verifies its hash but 409s (mode can't be flipped without breaking the hash); wired into `load_production`.
- [x] **Server-authoritative `seal_allowed`**: replaces the QE-452 blanket 409; runs in `spawn_blocking`; reads ONLY {hash-verified pool, audit replay, const} + server-derived {launcher, revoked} — NO request field; requires all 8 hard-blocks + `mode==production` + const + not-revoked + two distinct approver sigs ≠ launcher; failure ⇒ 409 + named blocker list + appended `Reject` audit entry with `evidence_hash`; approval re-derived from `pool_hash`-bound events (no `review.json`).
- [x] **The eight §13.5 HARD-BLOCKS** — each unit-tested to fire individually with its named blocker; every ABSENT stat blocks. HB1 `gp_aware && distinct_evaluations > analytic_floor` (`N==floor`⇒block — the Phase-1b carry-forward, never seals on the floor); HB3 uncensored-PBO PRIMARY (`variance_trials<distinct`⇒censored⇒block); HB4 DSR≥0.95; HB5–8 per-formula `gate_evidence` (absent⇒block). `gate_evidence` hashed WHEN PRESENT (tamper-evident), skipped when absent (golden-safe).
- [x] **Displayed=enforced=evidenced**: `uncensored_pbo`/`variance_trials`/`distinct_evaluations` on `GateSnapshot`/`ProgressLine::Gate` (evolve emits real values; train emits `None`); the seal `evidence_hash` is SHA over the same enforced stat set.
- [x] **Carry-forward #1 (SECURITY)**: `resolve_launcher` (pool-bound then run-bound via `find_run_id_by_pool`→`launcher_for_run`); `None` is a hard BLOCK (`launcher_unresolved`), never passed to `derive_signoff`; the live `/approve` SoD 403 fires on the run-bound path; self-approve / single-sig / `pool_hash`-mismatch all 409.
- [x] **Run supervision**: `tokio::time::timeout(≈24h, drain)` → `start_kill`+`kill_on_drop`+`finish_failed` (reuses QE-407, no new kill path); separate `QE_SERVER_MAX_EVOLVE_CONCURRENCY` semaphore (default 1) acquired only for evolve — serialises campaigns without starving backtests; authz'd Halt preserved.
- [x] **Golden-safety**: NORMAL train path byte-identical (new fields `#[serde(default, skip_serializing_if)]` absent-by-default; `agreement.rs` frozen Gate wire UNCHANGED; qe-gate untouched); reviewer independently re-ran `regenerate_fixtures` → empty; `POOL_FORMAT_VERSION=1`/`PROTOCOL_VERSION=2`/`VINTAGE_FORMAT_VERSION=7`/`CATALOGUE_VERSION=1` unchanged; NO vintage minted by sealing.

## Review verdict — [Approved] (0 blocking, 4 non-blocking), reviewer on `0f69792` (read `pool_seal.rs`/`basis.rs`/`formula-pool/lib.rs`/supervision directly; full cargo gate re-run; regenerate + versions + no-mint + firewall independently confirmed; a subagent corroborated `/seal` wiring, live `/approve` SoD, supervision, integration tests, AC5 soundness)
1. **`seal_allowed` a genuine fail-closed AND** over ONLY {hash-verified pool, audit replay, const} + server-derived {launcher, revoked} — no request field; all 8 hard-blocks fire individually (unit-tested); every absent stat blocks; failure → 409 + named blockers + `Reject` audit entry.
2. **Honest fold-in**: HB1 requires `distinct > analytic_floor` (never seals on the floor); HB3 uncensored-PBO primary + censored-population block; `gate_evidence` hashed-when-present; approval from `pool_hash`-bound events, no `review.json`.
3. **Carry-forward #1 fixed live**: `resolve_launcher` (pool→run-bound), `None`⇒block; live `/approve` SoD 403 fires; self-approve/single-sig/hash-mismatch all 409.
4. **3 barriers each fail-closed**: const (launch 400), research root off the vintage list, `assert_production_eligible` on the hashed `mode`.
5. **Golden crux holds**: regenerate empty, versions unchanged, train wire byte-identical, no vintage minted.
6. **Supervision** reuses the QE-407 kill path + evolve semaphore serialises without starving backtests; firewall unchanged (qe-validation a clean leaf, no `qe-wfo→qe-server` inversion); AC5 deferral sound (runtime genuinely never loads a pool).

Green gate on `0f69792`: fmt/clippy(both)/**1046 passed, 2 ignored** (audit_governance 13 / runs 17 / pools 11 / pool_seal 6 / basis 2)/deny/firewall all green; regenerate empty.

### Non-blocking findings (accepted; all fail-closed-safe)
1. **[Deferred follow-up — the last mile]** The evolve pipeline currently emits `gate_evidence: None`, so a live-evolve-produced production pool **cannot yet seal** (HB5–8 correctly blocks absent per-formula evidence). The "unlock" is a launch capability + a fully-armed predicate; the **end-to-end live evolve → production-seal path stays BLOCKED until per-formula `gate_evidence` is wired into the evolve pipeline** (a follow-up ticket). This is the safe direction (fail-closed), not a defect.
2. Barrier-2 integration test is thin (asserts the research pool is absent from the vintage list; a deeper test would drive a full research→prod copy attempt end-to-end).
3. `/api/audit` remains session- not admin-gated (from Phase A) — a future hardening choice.
4. Minor: the defense-in-depth QE-452 blanket-409 guard is now redundant with `seal_allowed` but harmless (kept as belt-and-suspenders).

## Phase status — QE-454 COMPLETE; PROGRAM COMPLETE
- Phase A (RBAC + audit + GovernanceRecord) — delivered ([`qe-454-phaseA.md`](./qe-454-phaseA.md)).
- Phase B (seal predicate + barriers + deflation fold-in + supervision) — **delivered** (this record). **QE-454 is complete, and with it the entire QE-430..454 (24-ticket) program.**
- Deferred follow-up: wire per-formula `gate_evidence` into the evolve pipeline to complete the live evolve→production-seal path (currently fail-closed by design).
