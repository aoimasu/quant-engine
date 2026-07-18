# QE-454 Phase A — RBAC + tamper-evident audit + GovernanceRecord (identity/audit substrate) — design & evidence

*QE-454 epic, **Phase A of 2**. Phase A delivers the **identity + audit + governance-record substrate**
(§13.8 RBAC/separation-of-duties, §13.9 tamper-evident audit + governance↔lineage binding). Phase B builds
the server-authoritative `seal_allowed` predicate (§13.7), the §13.5 eight hard-blocks, the
`DEFLATION_BASIS_VERSION` const + the §13.6 structural barriers, the `GateSnapshot` PBO/variance fields, and
run supervision — all of which **consume** the substrate this phase provides. **Production sealing stays
FAIL-CLOSED (the Phase-B `409`) — Phase A must not open it.***

- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §13.8, §13.9 (primary);
  §13.3 (lifecycle), §13.6/§13.7 (context — Phase B).
- **Builds on (merged)**: QE-452 Phase B ([`qe-452-phaseB.md`](../mds/reviewed/qe-452-phaseB.md)) — the
  `require_role` seam, `RoleConfig`, `PoolGovernanceStore`, the governance routes, the fail-closed
  production seal.

## Current-state evidence (what Phase B left)

- `crates/server/src/auth/mod.rs` — `require_session` sets an `AuthedEmail` request extension; a **placeholder**
  `require_operator`/`require_approver` seam resolves `RoleConfig` (`QE_ROLE_OPERATORS`/`QE_ROLE_APPROVERS`)
  per-request from the `AuthedEmail` (already **not** from the cookie) and `403`s a role-less caller. Doc
  comments explicitly call it a placeholder to be hardened by QE-454. `/api/me` returns only `{email}`.
- `crates/server/src/pools.rs` — governance routes `approve/reject/revoke/seal` + `/halt`, each behind the
  seam. `apply_transition` returns `ProductionSealGated` (`409`) for `Seal + Production` **before any state
  change**; sandbox transitions function. Governance persists to `<data_dir>/governance/<pool_id>.json`
  (`PoolGovernanceStore`) — a plain record, **not** yet tamper-evident.
- `crates/formula-pool/src/lib.rs` — `FormulaPoolContent` (hashed) carries `lineage.pool_hash` (content
  address over the sorted `formula_hash` list) + `formulas[].formula_hash`; `FormulaPool::{seal,verify,load}`
  is the SHA-256 discipline (`load` never yields an unverified pool). `lifecycle.rs` is the pure 5-state
  machine + `PoolGovernanceStore`.
- `crates/server/src/runs/store.rs` — `atomic_write` (temp + `rename`); `runs/manager.rs` serialises
  `index.json` under a `tokio::sync::Mutex` (`index_lock`). `check_session_secret_policy`
  (`auth/mod.rs`) is the fail-closed "ephemeral secret must not guard a network bind" precedent to mirror.

## Control-by-control design

### 1. Authoritative `require_role(Role)` (§13.8)
The Phase-B seam is **promoted to authoritative** (doc comments de-placeholdered). It already resolves roles
**per-request, server-side, from env allowlists** via the fail-closed `parse_allowlist`, reading the
`AuthedEmail` extension — never the cookie/body. Phase A adds the third role `QE_ROLE_ADMINS`
(`RoleConfig.admins` + `is_admin`) and keeps `require_operator`/`require_approver` on the governance routes
(approve/reject/revoke → approver; launch/halt → operator) **without moving the routes**.

**Why roles are never in the cookie:** the session cookie (`mint_session_cookie(secret, email, exp)`) carries
**only the email**; roles are looked up fresh on every request from `RoleConfig` (env). So revoking an
approver (removing them from `QE_ROLE_APPROVERS`) takes effect on their **next request**, and no cookie/body
field can forge a role. *Test:* a request carrying a forged `X-Role: approver` header **and** a `{"role":
"approver"}` body is still `403` when the email is not in the approver allowlist — proving request-supplied
role claims are ignored (roles come only from env).

### 2. Dual sign-off / separation of duties (§13.8, §13.3)
Modeled as a **pure predicate over the audit log**, keeping the existing 5-state `PoolLifecycleState`
machine untouched (so every merged Phase-B lifecycle test still holds). §13.3 makes the append-only signed
audit log the **authoritative** source of approvals; `governance/<pool>.json` is a rebuildable cache the seal
gate never reads.

`SignoffState ∈ {NoSignoff, AwaitingSecondSignoff, TwoDistinctSignoffs}` is derived by
`AuditLog::derive_signoff(pool_hash, launcher)`:
- count **distinct** approver emails from `approve` entries whose `subject_hash == pool_hash`, **excluding**
  the launcher;
- `0 → NoSignoff`, `1 → AwaitingSecondSignoff`, `≥2 → TwoDistinctSignoffs` (the two-signature clause is
  satisfiable — Phase B's `seal_allowed` consumes exactly this).

Properties (unit-tested): same approver twice → set size 1 → **still `AwaitingSecondSignoff`**; the launcher
as approver → excluded → not counted; a **`pool_hash` mismatch invalidates every prior signature** (entries
bound to the old `pool_hash` no longer match the current one → `NoSignoff`). Approval authority is thus
re-derived from `pool_hash`-bound signature **events**, never the stored `review.json` status.

**Separation of duties at `/approve`:** the launcher of a pool is the actor of a `launch` audit entry bound
to the pool (`subject_hash == pool_id`, the campaign identity that is stable from launch to approval). If the
approving actor **is** that launcher, `/approve` returns `403` (SoD violation) *before* recording a
signature. The `approve` audit entry is bound to `pool_hash` (the frozen-artifact content address), so a
formula change invalidates it. **Lifecycle-cache handling:** the first approve advances the cache
`Draft→Approved`; a *second* (distinct-approver) approve appends the authoritative signature entry but leaves
the cache `Approved` (it does **not** re-drive the pure `apply(Approve)`, which correctly rejects a
re-approve on the cache) — the audit log, not the cache, carries the two signatures.

**Phase-A boundary (documented, not a gap):** the `launch` entry is committed at run-create (the first audit
entry, run-bound). Binding it to the frozen `pool_id` end-to-end happens when the evolve run terminates (the
run's `meta.train.pool` already records the `pool_id` — no new hashed state) — Phase B's `seal_allowed`
replay does that join. Because production sealing is **fail-closed** in Phase A (the `409` stays), no live
seal consumes the dual-signoff predicate yet, so there is no exposure: the substrate + enforcement + tests
are complete; only the automatic terminal launch↔pool binding is Phase B. The `/approve` SoD enforcement is
real and tested against a pool-bound launch entry (exactly what the terminal binding will populate).

### 3. Tamper-evident audit log (§13.9)
`<data_dir>/audit/log.jsonl` (sibling of `runs/`). Each line an `AuditEntry`:
`{seq, ts_ms, actor_email, action(launch|approve|reject|revoke|role_change), subject_hash, run_id,
vintage_id, evidence_hash, prev_hash, entry_hash, hmac}`.
- **Preimage** = canonical JSON over the content fields `(seq, ts_ms, actor_email, action, subject_hash,
  run_id, vintage_id, evidence_hash)` concatenated with `prev_hash`.
- `entry_hash = SHA256(preimage)`; `hmac = HMAC-SHA256(QE_AUDIT_SIGNING_KEY, preimage)` (constant-time
  verify via `hmac::Mac`, the same primitive as the session cookie). Genesis `prev_hash` is a fixed
  constant; each entry's `prev_hash` = the prior entry's `entry_hash` → a hash chain.
- **Appends serialised** under an `index_lock`-style `tokio::sync::Mutex`, persisted with `atomic_write`
  (read-all → push → atomic rewrite of the JSONL — matches the run store's `index.json` discipline).
- `GET /api/audit` is paginated (`?limit`/`?cursor`) and returns a **per-page chain-verification status**
  (`ok` or `broken_at: <seq>`).
- **Tamper-evidence (tested):** mutating any content field of an entry makes its recomputed `entry_hash`
  differ from the stored one → `verify_chain` fails **at that seq**; corrupting the `hmac` fails HMAC verify
  at that seq.
- **FAIL-CLOSED signing-key policy:** `AuditConfig::from_env` reads `QE_AUDIT_SIGNING_KEY`; unset/blank ⇒ a
  random **ephemeral** key + `signing_key_is_ephemeral = true`. `production_seal_capability_allowed()` returns
  `false` while the key is ephemeral (mirrors `check_session_secret_policy`). Chain+HMAC is tamper-*evidence*
  (sufficient for v1; external WORM checkpointing is a follow-up).

### 4. `GovernanceRecord` — OUTSIDE `VintageContent` (§13.9, AC4)
A **separate content-addressed** record (new pure type in the `qe-formula-pool` leaf):
`GovernanceRecord{vintage_content_hash, pool_formula_hashes, launch_entry_hash, approval_entry_hashes[2],
evidence_hash}` with its own `content_hash()` (SHA-256 over canonical JSON). It **references** a vintage's
`content_hash` but is **not** a member of `VintageContent`.

**Why (the determinism constraint):** `VintageContent::content_hash` covers the whole struct incl. lineage,
and `vintage_id` is that hash. Embedding post-hoc approver identity into `VintageContent` would change
`vintage_id` and break QE-450 **AC4** byte-identity (reproduce-from must byte-match). Keeping governance in a
*separate* content-addressed record joins governance→lineage (anyone recomputes the pool from `Lineage`,
recomputes its content hash, and confirms two valid approvals against that exact hash) **without touching the
hashed vintage struct**. *Golden test:* seal a `Vintage`, capture `content_hash`/`vintage_id`; build a
`GovernanceRecord` referencing it; re-verify the vintage — `content_hash`/`vintage_id` **unchanged**; the
record's own hash is independent.

### 5. Revocation — forward-only, no history rewrite (§13.9)
`/revoke` (a) appends an **append-only** `revoke` audit entry referencing the approval's `entry_hash` (via
`evidence_hash`), and (b) inserts into `<data_dir>/governance/revocations.json` (`Revocations`, a new pure
`qe-formula-pool` type keyed by `pool_hash`). `Revocations::is_revoked(pool_hash)` is the filter that **both**
the Phase-B G1/promotion path **and** the production evolved-catalogue read path consult — so a revoked pool
becomes **inert on the live/read path even if previously sealed**, **without rewriting history** (the prior
approve/seal entries stay in the chain; already-sealed vintages keep their immutable `formula_hash` pin).
Phase A wires the filter into the pool read/summaries path (a revoked pool is marked/excluded) and exposes
`is_revoked` for Phase B. *Test:* revoke a sealed sandbox pool → `is_revoked` true + the read path treats it
inert, while the audit chain still contains the earlier seal entry (history intact).

### 6. `/api/me` capabilities — UX-only (§13.8)
`/api/me` gains `capabilities:{canLaunch, canApprove}`, computed **server-side** from `RoleConfig` for the
authed email. They are **hints that can only remove affordances** — the server enforces on every governance
route regardless of what the client renders. *Test:* capabilities mirror the allowlists, and a caller whose
`canApprove` is false is still `403` on `/approve` (capabilities are never authoritative); a forged
capability cannot grant access because the server never reads client-supplied capabilities.

## Golden-safety (critical)
- **No golden move:** no vintage minted, no catalogue registration, **`VintageContent` UNCHANGED**. The
  `GovernanceRecord` and `Revocations` are separate content-addressed / operational types **outside** the
  hashed vintage struct — they do **not** touch `content_hash`/`vintage_id`.
- Audit log (`audit/`), governance store + `revocations.json` (`governance/`) live under `<data_dir>`, **not**
  in hashed artifacts.
- Confirm `regenerate_fixtures` → empty diff and `CATALOGUE_VERSION`/`VINTAGE_FORMAT_VERSION`/
  `PROTOCOL_VERSION` unchanged.
- **Firewall:** new pure types live in `qe-formula-pool` (a leaf, no new deps); audit/role code stays in
  `qe-server`. No new crate edges → no `qe-runtime`/`qe-venue` regression.
- **Production seal stays fail-closed:** the Phase-B `Seal + Production → 409` guard is untouched; Phase A
  adds no code path that mints a production vintage.

## Test plan (non-vacuous, prove-it)
1. `require_role`: role-less governance request `403`; allowlisted passes; a **forged cookie/body/header role
   claim is ignored** (still `403`) — roles resolved per-request from env.
2. Audit chain tamper-evident: mutate one entry → `verify_chain` fails **at that seq**; corrupt an `hmac` →
   fails at that seq; a clean chain verifies.
3. Dual sign-off: two **distinct** approvers ≠ launcher → `TwoDistinctSignoffs`; **same approver twice → still
   `AwaitingSecondSignoff`**; **launcher-as-approver → excluded/`/approve` 403**; **`pool_hash` mismatch →
   signatures invalidated** (`NoSignoff`).
4. Fail-closed key: `production_seal_capability_allowed()` is **false** on an unset/ephemeral
   `QE_AUDIT_SIGNING_KEY`, true on a persistent key — and production `/seal` **still `409`** regardless.
5. Revocation inert-on-read without history rewrite.
6. `GovernanceRecord` does **not** change `vintage_id`/`content_hash` (byte-identity golden test).
7. `/api/me` capabilities mirror roles but never authorize (server enforces regardless).
8. Green gate + `regenerate_fixtures` empty + versions unchanged + firewall green.

## Risks
- **Scope creep into Phase B** — mitigated by *not* implementing `seal_allowed`, the hard-blocks, the const,
  the barriers, `GateSnapshot` fields, or run supervision; production seal stays `409`.
- **Breaking merged Phase-B tests** — mitigated by leaving the `PoolLifecycleState` machine + the
  fail-closed seal guard untouched and modeling dual-signoff as an audit-derived predicate.
- **Golden drift** — mitigated by keeping `VintageContent`/`qe-vintage` untouched and asserting
  `regenerate_fixtures` empty + versions unchanged.
