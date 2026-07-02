# QE-222 — GATE G2: Live shadow / dry-run — design note

`Phase: P2` · `Area: gate` · `Depends on: QE-218, QE-221` · **Blocks: Phase 3 live capital** ·
`Branch: qe-222/g2-live-shadow`

## Goal (from backlog)

*(Reviewer-added.)* Before any capital, run the full loop against live data computing **would-be** orders with
**no submission**, reconciled vs the simulator — catching wss-stitch, mark-EMA, netting, and cutover bugs.

- **Scope.** Shadow mode: full pipeline on live data; the Edge gateway **logs would-be orders without
  submitting**; reconcile against simulator expectations.
- **Out of scope.** Go/no-go sign-off (QE-308) — QE-222 produces the *evidence*; QE-308 is the decision.

**Acceptance criteria.**
- [ ] A shadow run over a defined live period produces would-be orders that **reconcile with the simulator
  within tolerance**; **no orders are submitted**.

`Spec ref: reviewer: live shadow / dry-run gate.`

## Current-state evidence & placement

- QE-217 (`crates/runtime/src/edge.rs`): `plan_delta(target, current_qty, mark)` (target → venue-native
  delta), `VenueKeeper`, `VenueSimulator` (paper fills).
- QE-218 (`crates/runtime/src/transport.rs`): `TargetRevision` (the planner's absolute target over the wire)
  and `PlannerAdapterLink` — the production dispatch that runs a target through the kill gate into the
  simulator and returns fills/position. This is the **simulator-expectation** reference.
- QE-221 (`crates/runtime/src/reconciliation.rs`): `ReconciliationGuard` — compares two positions within
  tolerance. QE-222 uses it to reconcile the **shadow** (would-be) position against the **simulator**
  position; a gate *reports*, so it runs in `AlarmOnly` (no halt during a dry-run).
- **Placement: new `crates/runtime/src/shadow.rs`.** The shadow/dry-run edge + the gate run are runtime
  capabilities that compose QE-217/218/221 — all already `qe-runtime`. No new dependency; firewall unaffected.

## Design

### D1 — `ShadowGateway` — the dry-run edge (logs would-be orders, submits nothing)

```rust
pub struct WouldBeOrder { pub seq: u64, pub side: Side, pub qty: Qty, pub event_time_ms: i64 }

pub struct ShadowGateway { mark: Price, shadow_qty: Decimal, would_be: Vec<WouldBeOrder> }
```

- `observe(&mut self, rev: &TargetRevision) -> Option<&WouldBeOrder>` — the same computation the live edge
  does (`plan_delta(rev.target.notional, shadow_qty, mark)`), but instead of submitting it **logs** the
  resulting order and advances a **shadow position** as-if-filled. Returns the logged order (`None` when
  already at target). It **never** submits — `orders_submitted()` is a `const 0`.
- `observe_mark(Price)`, `shadow_position() -> Decimal`, `would_be_orders() -> &[WouldBeOrder]`.

### D2 — `ShadowRun` — the gate: drive both, reconcile, report

```rust
pub struct ShadowReport { pub would_be_orders: usize, pub shadow_position: Decimal, pub sim_position: Decimal,
                          pub orders_submitted: u64, pub reconciled: bool, pub max_divergence: Decimal }

pub struct ShadowRun { shadow: ShadowGateway, reference: PlannerAdapterLink,
                       guard: ReconciliationGuard, reconciled: bool, max_divergence: Decimal }
```

- `new(mark, tolerance)` — a flat shadow gateway + a fresh submitting `PlannerAdapterLink` (the sim
  expectation) + a `ReconciliationGuard(tolerance, AlarmOnly, …)`.
- `observe_mark(mark)` — feeds **both** the shadow and the reference keeper the same mark (venue truth).
- `observe(&mut self, rev)` — drives the revision through **both** paths: the shadow logs a would-be order and
  advances its shadow position; the reference `submit_target(rev)` + `pump()` fills the sim and advances the
  keeper. Then it **reconciles** the shadow position against the sim position via the guard, tracking whether
  every step stayed within tolerance and the max divergence seen.
- `report()` — `orders_submitted` is the **shadow's** count (must be `0`); `reconciled` is true iff every step
  was within tolerance; `sim_position` proves the reference actually traded (so a vacuous "both flat" pass is
  ruled out).

Because both paths derive the order from the same `plan_delta` over the same targets and mark, they agree
exactly in the happy path (divergence `0`); the value of the gate is that **any** pipeline discrepancy
(mark-EMA drift, netting error, a stitched/duplicated bar, a cutover gap) makes the shadow position diverge
from the simulator and the guard reports it — which is exactly what the gate must catch before live capital.

## Test plan (deterministic, TDD)

`crates/runtime/src/shadow.rs`:
1. `shadow_run_reconciles_with_simulator_and_submits_nothing` (**AC**) — a defined period of `TargetRevision`s
   (flat → long → larger → smaller → flat); after the run `report.reconciled == true`,
   `report.orders_submitted == 0` (the shadow submitted nothing), `report.would_be_orders > 0`, the shadow and
   sim positions are equal, and the **reference sim actually submitted** (`> 0`) — the dry-run really is a
   no-submit shadow of a trading reference.
2. `would_be_orders_match_simulator_fills` — per revision, the shadow's would-be order side/qty equals the
   simulator's fill side/qty (the logged orders are exactly what would have been sent).
3. `at_target_revision_logs_no_would_be_order` — a revision already at the shadow's position produces no
   would-be order and no sim order.
4. `reconciliation_catches_a_pipeline_divergence` — feed the shadow a **stale mark** while the reference sees
   the fresh one (a mark-EMA/stitch bug proxy) for the same target: the shadow and sim positions diverge and
   the `ReconciliationGuard` flags it (`reconciled == false`, `max_divergence > tolerance`, `alarms ≥ 1`) —
   proving the gate genuinely bites, not just that the happy path agrees.

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **No real submission — structural.** `ShadowGateway` has no submit path at all (`orders_submitted()` is a
  literal `0`); the gate asserts it, so "no orders are submitted" is guaranteed by construction, not by a flag
  that could be forgotten.
- **Happy-path agreement is exact, so the bite must be proven separately.** Both paths share `plan_delta`, so a
  passing reconcile alone could be vacuous; test 4 injects a real divergence (stale mark) to prove the
  reconciliation catches a pipeline bug, and test 1 asserts the reference actually traded.
- **`AlarmOnly` for the gate.** A dry-run *reports* divergences; it must not halt (there is nothing live to
  halt). The QE-221 auto-halt (`HaltAfter`) belongs to the live path, not the shadow gate. Documented.
- **Determinism.** Single-threaded, pull-based; synthetic target/mark stream, no clocks/sockets/RNG. The real
  live-data feed is the runtime wiring (out of scope, like QE-202's real socket); the gate's *logic* is what is
  proven here.
- **Firewall / deps.** No new crate edge; composes existing `qe-runtime` modules. QE-132 guard stays green.
