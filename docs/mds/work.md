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

## QE-222 — GATE G2: Live shadow / dry-run — [Ready-for-review]

- **PR:** #73 — https://github.com/aoimasu/quant-engine/pull/73
- **Ticket:** QE-222 (`Phase: P2` · `Area: gate` · `Depends on: QE-218, QE-221` · **Blocks: Phase 3 live capital**)
- **Branch:** `qe-222/g2-live-shadow`
- **Latest commit:** `f76045f218b5f57d25051c7068e9c5d3ed35e8f6`
- **Evidence / design:** `docs/architecture/qe-222-g2-live-shadow-design.md`
- **Changed files:** `crates/runtime/src/shadow.rs` (new), `crates/runtime/src/lib.rs` (module + re-exports +
  crate doc). (Also archives QE-221 → `docs/mds/reviewed/qe-221.md` + clears the prior `work.md` entry.)

### Goal
*(Reviewer-added.)* Before any capital, run the full loop against live data computing **would-be** orders with
**no submission**, reconciled vs the simulator — catching wss-stitch, mark-EMA, netting, cutover bugs.

### Acceptance criteria (from backlog)
- [x] A shadow run over a defined live period produces would-be orders that **reconcile with the simulator
  within tolerance**; **no orders are submitted** — `shadow_run_reconciles_with_simulator_and_submits_nothing`.

### Implementation summary
- New `crates/runtime/src/shadow.rs`. **`ShadowGateway`** = the Edge gateway in dry-run: `observe(&rev)` runs
  the same `plan_delta` the live edge does but **logs** the `WouldBeOrder` and advances a shadow position
  as-if-filled, submitting nothing (`orders_submitted()` is a literal `0` — no submit path exists).
  **`ShadowRun`** drives each `TargetRevision` through both the shadow edge and a submitting reference QE-218
  `PlannerAdapterLink`, reconciling the two positions via the QE-221 `ReconciliationGuard` (`AlarmOnly` — a
  dry-run reports, never halts); `report()` yields `ShadowReport`.
- **Scrutinise:** (1) both paths share `plan_delta`, so the happy-path reconcile is exact — is that vacuous, or
  do test 1's "reference actually traded" assertion + test 4's stale-mark divergence adequately prove the gate
  bites? (2) `AlarmOnly` for the gate (no halt in a dry-run) — right call? (3) the reference sim IS the
  expectation oracle *and* the "no submission" contrast — is comparing shadow-vs-sim (both from `plan_delta`)
  the right reconciliation, or should the shadow reconcile against something more independent? (4) is a
  synthetic target/mark stream a fair stand-in for "live data" at this layer (real feed = runtime wiring, out
  of scope like QE-202's socket)? (5) `orders_submitted()` const `0` — guaranteed-by-construction vs a flag:
  agree?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 577 passed / 1 ignored / 57 suites (+4 shadow tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok

### Feedback

_First review pass, commit `d2ea50b8` (2026-07-02). **What is correct:** "no submission" is genuinely
structural — `ShadowGateway` has no submit path and `orders_submitted()` is a literal `0`, sourced into the
report and asserted (Scrutinise #5: const-`0`-by-construction beats a mutable flag — agreed). `AlarmOnly` for
a dry-run gate is the right call (nothing live to halt; auto-halt belongs to the live path — Scrutinise #2).
`would_be_orders_match_simulator_fills` and `at_target_revision_logs_no_would_be_order` are non-vacuous and
correct. Determinism, encapsulation, no new dep/firewall edge all hold. The synthetic target/mark stream as a
stand-in for the live feed is fair at this layer (real feed = runtime wiring, out of scope like QE-202's
socket — Scrutinise #4). One blocker (F1) and one scope note (F2). Because this is the gate that blocks Phase
3 live capital, I am holding the bar high on whether the gate actually bites._

**F1 — [Blocker] The `ShadowRun` gate is structurally incapable of reporting a divergence, so its
`reconciled == true` result is vacuous as a gate — the red state is unreachable and untested.** Inside
`ShadowRun`, the shadow and the reference are fed **identical inputs** by construction: `observe_mark(m)`
pushes the same `m` to both (`shadow.rs:147-150`), and `observe(rev)` runs the same `rev` through both
(`shadow.rs:155-168`). The shadow advances `shadow_qty` by `plan_delta(...).qty` as-if-filled; the reference
submits the same intent to a **full-fill** paper `VenueSimulator` (verified: `Order::on_fill` reaches `Filled`
at the full qty, keeper applies the whole fill), so its position advances by exactly the same qty. Both sides
start flat, share `plan_delta`, and never receive divergent data — therefore `self.reconciled` /
`self.max_divergence` are **constant `true` / `0` for every possible input**, and `report.reconciled` can
**never** be `false` through the gate's own API. The AC test's `assert!(report.reconciled)` and
`max_divergence == 0` are consequently vacuous: they would pass for *any* target stream. Test 4
(`reconciliation_catches_a_pipeline_divergence`) does prove the `ReconciliationGuard` bites on a stale-mark
divergence — but it **hand-wires a separate `ShadowGateway` + `PlannerAdapterLink` with different marks,
bypassing `ShadowRun` entirely**; it proves the *guard*, not the *gate*. So the answer to "does the gate bite
and would it catch a real pipeline bug?" is: **not through `ShadowRun` as delivered** — the one code path a
go/no-go reviewer would trust (drive the period, read `report.reconciled`) can only ever show green. A gate
whose fail state is unreachable gives false assurance at the most safety-critical checkpoint. **Required
resolution:** make a divergence reachable *through the gate's own API* and test it red — e.g. give `ShadowRun`
a way to feed the shadow edge a pipeline distinct from the reference (separate mark stream, or an injectable
pipeline-fault/mark-skew on the shadow side), then add a test that drives it and asserts
`report.reconciled == false` **and** `report.max_divergence > tolerance` via `ShadowRun`. Then `reconciled`
is a real gate signal, not dead code. (If you contend the divergent feed is strictly out of scope, that is
not acceptable for *this* ticket: the gate's whole purpose per the goal — "catching wss-stitch, mark-EMA,
netting, cutover bugs" — is the bite, and it must be demonstrable through the gate, not only a hand-wired
guard.)

**F2 — [Note, tied to F1] The reconciliation oracle is not independent of the sizing logic.** Both the shadow
and the reference derive the order from the **same** `plan_delta`, so a bug **in** `plan_delta` (or in the
notional→contracts sizing) would corrupt *both* sides identically and still reconcile at delta 0 — this gate
cannot catch it. That is a defensible scope boundary (the gate targets input-pipeline divergences —
mark/stitch/netting/cutover — and `plan_delta` has its own QE-217 tests), but it should be stated explicitly
in the design note as a known blind spot, and it reinforces F1: the gate's value is entirely in detecting when
the shadow's *inputs* diverge from the reference's, which is precisely the path that is currently unreachable
and untested. Not independently blocking; resolve alongside F1.

### Fix applied (commit `f76045f2`)

**F1 — resolved (agreed; the critique is exactly right).** The gate's red state was unreachable through its
own API because `ShadowRun` fed the shadow and reference identical inputs (and the sim does full fills), so
`report.reconciled` was constant-true and the AC assertion vacuous. Added
**`ShadowRun::observe_marks(shadow_mark, reference_mark)`** — the shadow edge (the live pipeline under test)
and the reference keeper (venue truth) now take marks **independently**, so the shadow's mark pipeline can
drift exactly as a mark-EMA / stitch / stale-tick bug would; `observe_mark(m)` is the aligned shorthand
(`observe_marks(m, m)`). The next `observe` then diverges and `report.reconciled` becomes `false` — **reachable
end-to-end through the gate**. Replaced the hand-wired guard test with
`gate_reports_a_mark_pipeline_divergence_through_shadow_run` (drives `ShadowRun` with a skewed shadow mark →
`reconciled == false`, `max_divergence == 0.05 > tolerance`, `orders_submitted == 0` even on the fail path),
and added `a_divergence_latches_the_run_red` (a run-level latch — one diverged step condemns the run, peak
`max_divergence` retained, even after a later re-converging step). `reconciled` is now a real gate signal, not
dead code.

**F2 — acknowledged and documented as a known blind spot.** The oracle shares `plan_delta` with the code under
test, so a bug **inside** `plan_delta` / the notional→contracts sizing corrupts both sides identically and
reconciles at delta 0 — this gate cannot catch it. Stated explicitly in the design-note Risks: the gate's
scope is **input-pipeline** divergences (mark-EMA / stitch / netting / cutover); `plan_delta` has its own
QE-217 tests. Kept as a deliberate scope boundary, now surfaced so it is not mistaken for full E2E coverage.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 578 passed / 1 ignored /
57 suites (5 shadow tests) · `cargo test -p qe-architecture --test firewall` 1 passed · `cargo deny check` ok.
