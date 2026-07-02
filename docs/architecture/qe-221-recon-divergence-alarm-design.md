# QE-221 — Real-time reconciliation divergence alarm — design note

`Phase: P2` · `Area: ⑨ + risk` · `Depends on: QE-217` · `Branch: qe-221/recon-divergence-alarm`

## Goal (from backlog)

*(Reviewer-added.)* Reconciliation should not be post-hoc only; a live journal-vs-venue mismatch beyond
tolerance should be a **fast safety check** that can trip the kill-switch.

- **Scope.** Periodically compare the runtime's **expected** position against **venue truth**; on divergence
  beyond tolerance, **alarm** and optionally **trip QE-216**.
- **Out of scope.** Cold-path attribution (QE-302) — the *why* of a divergence; QE-221 is the fast *detector*.

**Acceptance criteria.**
- [ ] An injected position desync **beyond tolerance raises an alarm and can halt**.

`Spec ref: Runtime — position reports authoritative; reviewer: real-time divergence guard.`

## Current-state evidence & placement

- QE-217 (`crates/runtime/src/edge.rs`): `VenueKeeper` is fed the venue's **authoritative** position reports
  and **never infers** position. Venue truth arrives as `qe_venue::userdata::PositionReport { direction:
  Option<Direction>, qty, entry_price, event_time_ms }` (sign lives in `direction`, `qty` is non-negative).
- QE-216 (`crates/runtime/src/kill_gate.rs`) + QE-009 (`crates/risk/src/kill.rs`): the latching, cloneable
  `KillHandle` — `trip(reason)` (out-of-band, first-reason-wins), `is_tripped()`. A guard that holds a clone
  can trip the same halt the venue adapter honours, so a divergence can flatten-and-halt without the cockpit.
- **Placement: new `crates/runtime/src/reconciliation.rs`.** It compares an expected signed position against a
  venue `PositionReport` and holds a `KillHandle` — needs `qe_venue` + `qe_risk` + `qe_domain`, all already
  `qe-runtime` deps. No new dependency; firewall unaffected.

## Design

### D1 — `ReconciliationGuard` — the fast divergence detector

```rust
pub enum AlarmAction { AlarmOnly, Halt }               // beyond-tolerance policy

pub struct Divergence { pub expected: Decimal, pub venue: Decimal, pub delta: Decimal, pub halted: bool }

pub enum ReconOutcome { Reconciled, Diverged(Divergence) }

pub struct ReconciliationGuard { tolerance: Decimal, action: AlarmAction, kill: KillHandle, alarms: u64 }
```

- **`new(tolerance, action, kill)`** — `tolerance` is an **absolute contracts** bound, clamped to `≥ 0` (a
  negative tolerance would make every check diverge). Holds a **clone** of the QE-216 `KillHandle`.
- **`check(&mut self, expected: Decimal, venue: &PositionReport) -> ReconOutcome`** — the periodic check:
  1. `venue_qty = signed(venue)` (`Long → +qty`, `Short → −qty`, `None → 0`).
  2. `delta = |expected − venue_qty|`.
  3. `delta ≤ tolerance` → `Reconciled` (no alarm, nothing tripped).
  4. `delta > tolerance` → **alarm**: `alarms += 1`; if `action == Halt`, **trip the kill** with a descriptive
     reason (`"reconciliation divergence: expected … venue … |Δ| … > tol …"`); return
     `Diverged { expected, venue: venue_qty, delta, halted }`.
- Accessors: `alarms()`, `kill()`, `tolerance()`, `action()`. A `check_qty(expected, venue_qty)` core is
  exposed too (report-free unit checks).

### D2 — why this satisfies "fast, out-of-band, can-halt"

- **Fast:** a single signed-decimal subtraction + compare per period — no journal replay, no attribution. It is
  the *detector*; QE-302 does the post-hoc *explanation*.
- **Out-of-band halt:** the guard trips the **same** latching `KillHandle` the venue adapter (QE-216) honours,
  so a divergence flattens-and-halts independently of the cockpit/planner. `Halt` vs `AlarmOnly` lets the
  operator choose auto-halt or alarm-then-manual.
- **Sign-aware:** because `venue_qty` carries the report's direction, a **sign flip** (expected long, venue
  short) yields a `delta` of the *sum* of magnitudes — a severe desync that trips well before a same-side
  drift of the same magnitude, which is the correct safety ordering.
- **Latching:** the `KillHandle` latches (QE-009), so once a divergence halts, it stays halted; repeated
  checks still count alarms but do not "un-halt".

## Test plan (deterministic, TDD)

1. `divergence_beyond_tolerance_alarms_and_halts` (**AC**) — expected `0.5`, venue report `Long 0.3`, tol
   `0.1`, `Halt` → `Diverged { delta 0.2, halted: true }`, `kill().is_tripped()`, `alarms() == 1`.
2. `within_tolerance_reconciles_without_alarm` — expected `0.5`, venue `Long 0.45`, tol `0.1` → `Reconciled`,
   kill untripped, `alarms() == 0`.
3. `exactly_at_tolerance_reconciles` — `delta == tolerance` → `Reconciled` (the bound is inclusive).
4. `alarm_only_mode_alarms_without_halting` — beyond tolerance with `AlarmOnly` → `Diverged { halted: false }`,
   kill **untripped**, `alarms() == 1`.
5. `sign_flip_is_a_divergence` — expected `+0.2` (long), venue report `Short 0.2` → `delta 0.4 > tol` →
   diverged (a flip trips even when magnitudes match).
6. `flat_venue_report_vs_expected_position_diverges` — expected `0.3`, venue report flat (`direction None`) →
   `venue_qty 0`, `delta 0.3` → diverged (a phantom position the venue does not confirm).
7. `alarms_accumulate_and_kill_latches` — two beyond-tolerance checks → `alarms() == 2`, kill tripped once and
   latched (first reason preserved).

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Tolerance is absolute contracts.** Simple and unambiguous for the AC; a fractional (of expected magnitude)
  tolerance is a later refinement if positions span very different scales. Documented; the constructor clamps a
  negative tolerance to zero so a mis-config fails safe (alarms more, never less).
- **The guard needs an independent `expected`.** It compares the runtime's *belief* against the venue report;
  the caller supplies `expected` from its own accounting (e.g. order-derived), **not** from the same keeper the
  report already updated — otherwise the check is circular. Documented; the API takes `expected` explicitly so
  the caller cannot accidentally compare venue-truth against itself.
- **Detector only.** It deliberately does not attribute the cause (QE-302) or reconcile balances — it raises a
  fast alarm and can halt. Documented scope boundary.
- **Firewall / deps.** No new crate edge; `qe-runtime` already depends on `qe-venue`/`qe-risk`/`qe-domain`.
  QE-132 guard stays green.
