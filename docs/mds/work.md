# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. A PR entry is added here when it
reaches review, the dedicated review agent writes `[Reviewed]`/`[Approved]` + comments inline, and on
merge the approved block is archived to `docs/mds/reviewed/<ticket>.md` and this file is **cleared back
to empty**. No running "Completed" list is kept here — the traceable history lives solely in
`docs/mds/reviewed/`.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

---

## QE-219 — Vintage load (read-only) + rollover — [Ready-for-review]

- **PR:** #70 — https://github.com/aoimasu/quant-engine/pull/70
- **Ticket:** QE-219 (`Phase: P2` · `Area: ① Vintage inputs` · `Depends on: QE-129, QE-207`)
- **Branch:** `qe-219/vintage-rollover`
- **Latest commit:** `6705c90ae3d459c52c3b47387e4f3a18ca2ae1b4`
- **Evidence / design:** `docs/architecture/qe-219-vintage-rollover-design.md`
- **Changed files:** `crates/runtime/src/vintage_rollover.rs` (new), `crates/runtime/src/lib.rs` (module +
  re-exports), `crates/runtime/Cargo.toml` (promote `qe-determinism` to a direct dep), design note. (Also
  archives QE-218 → `docs/mds/reviewed/qe-218.md` + clears the prior `work.md` entry.)

### Goal
Runtime loads the ensemble repo + calibration profile **read-only** at startup; periodic **rollover** replaces
the vintage in place when training emits a new one, without violating the firewall.

### Acceptance criteria (from backlog)
- [x] Startup loads a vintage read-only; a rollover swaps it atomically with lineage recorded —
  `startup_loads_vintage_read_only`, `rollover_swaps_in_place_with_lineage_recorded`,
  `rollover_rejects_unverified_vintage_keeping_current`.

### Implementation summary
- New `crates/runtime/src/vintage_rollover.rs`: `ActiveVintage` holds the current sealed `Vintage` + a
  `RolloverRecord` history. `load(repo, id)` uses `VintageRepository::load` (open + **hash-verify**, never
  write) — read-only startup. `rollover(next)`/`rollover_from(repo, id)` **verify before** the single
  `current = next` swap (atomic: a bad vintage never becomes active; repo + calibration — calibration lives
  inside `current.content` — never come from two vintages). Every rollover records `from/to vintage_id` +
  `from/to Lineage`.
- Promotes `qe-determinism` (`Lineage`) to a direct dependency — cross-cutting (QE-006), not on either side of
  the QE-132 firewall, already transitive via `qe-vintage`; firewall test green.
- **Scrutinise:** (1) atomicity is verify-before-commit on a single-threaded runtime — is that the right
  reading of "atomically", or is an `Arc`-swap expected now? (2) read-only proven by *using only* `load` (never
  `write`) + a byte-unchanged assertion — sufficient? (3) unbounded `history` growth across many rollovers —
  acceptable at rollover cadence? (4) `qe-determinism` promoted dev→direct dep — right call vs re-exporting
  `Lineage` from `qe-vintage`? (5) rollover accepts *any* verified vintage (no monotonic-lineage / same-id
  guard) — right boundary, or should a no-op/backwards rollover be rejected?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 559 passed / 1 ignored / 56 suites (+6 vintage_rollover tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok

### Feedback

_First review pass, commit `98558aeb` (2026-07-02). **Approved** — the AC is genuinely met and I found no
correctness or design defect. Detail + Scrutinise answers below; two minor observations recorded as
explicitly non-blocking._

**AC fidelity — met.**
- **Read-only load is genuine.** `ActiveVintage::load` uses only `repo.load` (open + hash-verify, never
  write), and `ActiveVintage` exposes **no** `repo.write` path at all — the type cannot mutate the repository.
  `startup_loads_vintage_read_only` snapshots the on-disk bytes before/after and asserts equality, so the test
  is non-vacuous (a write would fail it).
- **Rollover is atomic (verify-before-commit).** `rollover` runs `next.verify()?` **before** the single
  `self.current = next` move; on failure `current` + `history` are left exactly as they were. Because
  `calibration` lives *inside* `current.content`, the swap moves repo + calibration indivisibly — no torn
  repo/calibration state is representable. The record is built from the outgoing `self.current` **before** the
  swap, so `from_*` is correct. `rollover_rejects_unverified_vintage_keeping_current` tampers
  `content.weights[0]` after sealing so the recomputed hash genuinely mismatches, then asserts current is
  still v1 with empty history — a real, non-vacuous exercise of the safety boundary.
- **Lineage recorded** on every transition (both endpoints' `vintage_id` + `Lineage`); the chain test confirms
  ordering.

**Scrutinise answers.**
1. **Atomicity = verify-before-commit on a single-threaded runtime — correct reading; Arc-swap not required
   by the AC.** No concurrent reader can observe a half-swap in this (established) single-threaded runtime, and
   the swap is one move guarded by a prior `verify()`. The design note documents Arc-swap as the future path
   if a concurrent reader is ever introduced — the right place to defer it.
2. **Read-only proof is sufficient.** Byte-unchanged assertion on the real artefact + the structural fact that
   the API has no writer. Good.
3. **Unbounded `history` — acceptable at rollover cadence** (rollovers are rare/periodic); documented, with a
   ring buffer flagged as a later refinement. Fine.
4. **`qe-determinism` dev→direct promotion — right call.** It is genuinely the crate that defines `Lineage`;
   naming it directly is more honest than re-exporting `Lineage` through `qe-vintage` (which would be a facade
   hiding the true origin). It is cross-cutting (QE-006), already transitive via `qe-vintage`, and adds no
   train→live firewall edge — the reasoning holds and the firewall test re-proves it.
5. **No monotonic/same-id guard — the right boundary; I agree with the choice.** Accepting *any verified*
   vintage preserves legitimate **rollback** (reverting to a known-good vintage after a bad one), and there is
   no clean total order on vintage ids/lineage to enforce monotonicity against. `verify()` already blocks
   tampered artefacts. Guarding this would remove a useful capability for a cosmetic gain.

**O1 — [Observation, non-blocking] `rollover_from` verifies twice.** `repo.load(next_id)` already returns a
hash-verified vintage, then `rollover` calls `next.verify()` again — two hash computations per repo-driven
rollover. This is defensible (it makes `rollover` safe for *any* caller, including in-hand `from_vintage`-style
vintages, so the swap point is the single safety boundary), and rollovers are rare, so the cost is negligible.
Noting only so the redundancy is a conscious choice; a one-line comment on `rollover` stating "verifies
unconditionally so the swap is safe regardless of provenance" would make it self-evident.

**O2 — [Observation, non-blocking] a same-id rollover records a spurious transition.** With no same-id guard,
`rollover(v_same_id)` would append a `RolloverRecord` with `from == to`. Harmless (and the audit trail then
honestly reflects that a no-op rollover happened), but if a no-op should be suppressed, a `to_id == from_id`
early-return would do it. Not required for the AC.

### Polish applied (commit `6705c90a`) — O1/O2 addressed (doc-only)

Even though both were explicitly non-blocking, resolved them for clarity (no logic change):

- **O1 — addressed.** `rollover`'s doc comment now states it verifies **unconditionally** — it is the single
  safety boundary, safe for any caller regardless of provenance — so `rollover_from`'s second verify is
  documented as deliberate defence-in-depth on a rare path, not an oversight. (Kept the double-verify: making
  `rollover` self-sufficient is the right invariant.)
- **O2 — addressed (documented, behaviour intentionally kept).** `rollover`'s doc + the design-note Risks now
  explain that a **same-id vintage with changed content is a *real* transition** (its content hash differs) and
  is honestly recorded — there is deliberately no same-id guard, because rollback to a known-good vintage and
  re-emission of a rebuilt vintage under the same id are legitimate. Suppressing same-id would hide a genuine
  content change, so the honest record is correct.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 559 passed / 1 ignored /
56 suites · `cargo test -p qe-architecture --test firewall` 1 passed · `cargo deny check` ok.
