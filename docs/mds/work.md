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
- **Latest commit:** `d2ea50b85984370f22446ee5e29b8e1c05b8a3e2`
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
