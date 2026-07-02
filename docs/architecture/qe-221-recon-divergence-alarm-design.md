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

### D1 — `ReconciliationGuard` — the fast divergence detector (with debounce)

```rust
pub enum AlarmAction { AlarmOnly, HaltAfter { consecutive: u32 } }  // beyond-tolerance policy (debounced)

pub struct Divergence { pub expected: Decimal, pub venue: Decimal, pub delta: Decimal,
                        pub consecutive: u32, pub halted: bool }

pub enum ReconOutcome { Reconciled, Diverged(Divergence) }

pub struct ReconciliationGuard { tolerance: Decimal, action: AlarmAction, kill: KillHandle,
                                 alarms: u64, streak: u32 }
```

- **`new(tolerance, action, kill)`** — `tolerance` is an **absolute contracts** bound, clamped to `≥ 0` (a
  negative tolerance would make every check diverge). Holds a **clone** of the QE-216 `KillHandle`.
- **`check(&mut self, expected: Decimal, venue: &PositionReport) -> ReconOutcome`** — the periodic check:
  1. `venue_qty = signed(venue)` (`Long → +qty`, `Short → −qty`, `None → 0`).
  2. `delta = |expected − venue_qty|`.
  3. `delta ≤ tolerance` → `Reconciled`, and the **streak resets to 0** (any reconciled check clears the
     debounce).
  4. `delta > tolerance` → **alarm**: `streak += 1`, `alarms += 1`; **trip the kill** only when
     `action == HaltAfter { consecutive }` **and** `streak ≥ consecutive.max(1)` (the debounce), with a reason
     recording the streak length; return `Diverged { expected, venue: venue_qty, delta, consecutive, halted }`.
- `AlarmAction::halt_immediately()` = `HaltAfter { consecutive: 1 }` (single-check halt — quiescent-point mode).
- Accessors: `alarms()`, `consecutive_breaches()`, `kill()`, `tolerance()`, `action()`. A
  `check_qty(expected, venue_qty)` core is exposed too (report-free unit checks).

### D2 — why this satisfies "fast, out-of-band, can-halt"

- **Fast:** a single signed-decimal subtraction + compare per period — no journal replay, no attribution. It is
  the *detector*; QE-302 does the post-hoc *explanation*.
- **Out-of-band halt:** the guard trips the **same** latching `KillHandle` the venue adapter (QE-216) honours,
  so a divergence flattens-and-halts independently of the cockpit/planner. `HaltAfter` vs `AlarmOnly` lets the
  operator choose auto-halt or alarm-then-manual.
- **Debounced halt (avoids false-halting on in-flight orders).** Venue `PositionReport`s are
  eventually-consistent: a timer-driven periodic check will routinely fire while an order is *in flight* —
  `expected` already reflects an order the venue has not yet reported filled, so `delta` briefly equals the
  in-flight quantity and exceeds tolerance. Halting the whole book on that benign one-period skew would make
  the control unusable (and get it disabled in practice). A genuine desync **persists** across periods, whereas
  a propagation blip clears on the next check (which resets the streak). `HaltAfter { consecutive }` therefore
  trips only on a *sustained* divergence; a wider tolerance is the wrong fix (it blinds the detector), and
  `AlarmOnly` abandons the auto-halt AC — the streak threshold preserves fail-safe auto-halt for a real desync
  while ignoring a transient one.
- **Sign-aware:** because `venue_qty` carries the report's direction, a **sign flip** (expected long, venue
  short) yields a `delta` of the *sum* of magnitudes — a severe desync that trips well before a same-side
  drift of the same magnitude, which is the correct safety ordering.
- **Latching:** the `KillHandle` latches (QE-009), so once a divergence halts, it stays halted; repeated
  checks still count alarms but do not "un-halt".

## Test plan (deterministic, TDD)

1. `sustained_divergence_alarms_and_halts_after_threshold` (**AC**) — `HaltAfter { consecutive: 2 }`, tol
   `0.1`; first breach (expected `0.5` vs `Long 0.3`) → `Diverged { consecutive: 1, halted: false }`, kill
   untripped; second consecutive breach → `Diverged { consecutive: 2, halted: true }`, `kill().is_tripped()`,
   `alarms() == 2`.
2. `transient_single_period_skew_does_not_halt` (**F1 regression**) — `HaltAfter { 2 }`: breach (streak 1) →
   reconcile (streak resets) → breach (streak 1) ⇒ kill **never** tripped, `alarms() == 2` (both still alarm).
3. `immediate_halt_trips_on_first_breach` — `halt_immediately()` (`HaltAfter { 1 }`) halts on the first breach.
4. `within_tolerance_reconciles_without_alarm` — `Reconciled`, kill untripped, `alarms() == 0`, streak 0.
5. `exactly_at_tolerance_reconciles` — `delta == tolerance` → `Reconciled` (inclusive bound).
6. `alarm_only_never_halts` — three sustained breaches with `AlarmOnly` → never halts, `alarms() == 3`.
7. `sign_flip_is_a_divergence` — expected `+0.2`, venue `Short 0.2` → `delta 0.4` → diverged.
8. `flat_venue_report_vs_expected_position_diverges` — expected `0.3`, flat report → `venue 0`, `delta 0.3`.
9. `halt_latches_first_reason` — first triggering reason preserved (records the streak length).
10. `negative_tolerance_clamped_to_zero` — a negative tolerance clamps to `0` (fails safe).

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Single-check halt would false-halt on in-flight orders (QE-221 review F1) — fixed by the debounce.** A
  timer-driven check during active hedging routinely sees `expected` ahead of the venue's not-yet-reported
  fill; a single-check auto-halt would trip on that benign skew. `HaltAfter { consecutive }` requires the
  divergence to persist across checks (a reconcile resets the streak), so a sustained desync still halts but a
  one-period propagation blip does not. `halt_immediately()` restores single-check halt and is documented as
  safe **only** at quiescent points with no in-flight orders.
- **Tolerance is absolute contracts.** Simple and unambiguous for the AC; a fractional (of expected magnitude)
  tolerance is a later refinement if positions span very different scales. Documented; the constructor clamps a
  negative tolerance to zero so a mis-config fails safe (alarms more, never less). Note (review F2): do **not**
  widen the tolerance to absorb in-flight-order skew — that blinds the detector; the streak threshold is the
  right lever for the transient.
- **The guard needs an independent `expected`.** It compares the runtime's *belief* against the venue report;
  the caller supplies `expected` from its own accounting (e.g. order-derived), **not** from the same keeper the
  report already updated — otherwise the check is circular. Documented; the API takes `expected` explicitly so
  the caller cannot accidentally compare venue-truth against itself.
- **Detector only.** It deliberately does not attribute the cause (QE-302) or reconcile balances — it raises a
  fast alarm and can halt. Documented scope boundary.
- **Firewall / deps.** No new crate edge; `qe-runtime` already depends on `qe-venue`/`qe-risk`/`qe-domain`.
  QE-132 guard stays green.
