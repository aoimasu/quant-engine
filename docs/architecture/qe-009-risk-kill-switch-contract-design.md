# QE-009 — Risk-limit & kill-switch contract (shared types) — design / evidence

## Ticket

`Phase: P0` · `Area: domain / risk` · `Depends on: QE-004, QE-007`

**Goal.** The order path must be *born* with hard caps and an out-of-band halt, not have them
retrofitted at runtime. Defining the contract in P0 forces every downstream component to honour it.

**Scope / requirements.**
- First-class types for limit kinds: max notional, max leverage, max gross/net exposure,
  liquidation-distance floor, margin-utilisation ceiling, per-vintage drawdown caps.
- A kill-switch contract: out-of-band (independent of cockpit and Hedge Planner), acts at the
  order-submission layer, deterministically flattens-and-halts, independently testable.
- Limit-violation outcomes: clamp vs reject vs halt, defined per limit kind.

**Out of scope.** Enforcement implementation (QE-215, QE-216) — this ticket is the *contract*.

**Acceptance criteria.**
- The contract compiles and is referenced by the runtime crates' interfaces.
- A conformance test asserts any order-submitting component must accept a kill handle.

## Current-state evidence

- **QE-004 (`qe-error`)** gives the halt channel: a tripped kill / a `Halt`-outcome limit is expressed
  as `QeError::fatal` → `disposition == Disposition::Halt`. Same channel QE-008 used — consistent.
- **QE-007 (`qe-domain`)** gives `Notional`, `Qty`, `Price`, `InstrumentId`, `Direction` for the limit
  caps and the order intent, and the validated-newtype + serde-`try_from` discipline this crate
  reuses for `Leverage`/`Fraction`.
- **`qe-runtime`** is a scaffold today; AC #1 requires it to *reference* this contract. We add a
  `qe-risk` dependency and define `qe_runtime::OrderPort: qe_risk::OrderGate`, so the runtime's order
  port is, by its type, an order gate that holds a kill handle.

## Design decisions

New crate `qe-risk` (`crates/risk`) — types + traits + a reusable conformance check; **no enforcement
logic** (that is QE-215/216).

### `limit.rs` — limit kinds, caps, outcomes
- `LimitKind` enum (the seven kinds). `LimitOutcome { Clamp, Reject, Halt }` and
  `LimitKind::default_outcome()` encoding the per-kind policy with documented rationale:
  - `MaxNotional`, `MaxLeverage` → **Clamp** (shrink the order to fit).
  - `MaxGrossExposure`, `MaxNetExposure`, `LiquidationDistanceFloor`, `MarginUtilisationCeiling` →
    **Reject** (portfolio/margin-level breaches: refuse the order, keep trading).
  - `DrawdownCap` → **Halt** (a per-vintage drawdown breach kills the vintage).
- First-class value newtypes: `Leverage` and `Fraction` (∈ [0,1], for distance/utilisation/drawdown),
  both validated on construction **and** at the serde boundary (manual `Serialize` as exact string +
  validating `Deserialize`), per the QE-007 lesson. Notional caps use `qe_domain::Notional`.
- `RiskLimits { max_notional, max_leverage, max_gross_exposure, max_net_exposure,
  liquidation_distance_floor, margin_utilisation_ceiling, drawdown_cap }` — all `Option`, the
  configured cap set. `LimitBreach { kind, outcome, detail }` names a violation + its outcome.

### `kill.rs` — out-of-band kill switch
- `KillSwitch` trait (`is_tripped`/`reason`/`trip`) + concrete `KillHandle` (a cloneable
  `Arc<AtomicBool + Mutex<reason>>`). **Out-of-band:** the handle is shared by `clone()` and trippable
  from anywhere (a watchdog, the QE-008 skew guard, the cockpit) independently of any one component.
  **Latching:** once tripped it stays tripped (no reset mid-run) → deterministic halt.

### `gate.rs` — order-submission contract
- `OrderIntent { instrument, direction, qty, price }` (qe-domain types). `Admission { Admit,
  Clamp(Qty), Reject(String), FlattenAndHalt(String) }` — the kill path and a `Halt`-outcome limit
  both map to `FlattenAndHalt`.
- `OrderGate` trait: `kill_handle(&self) -> &KillHandle` (so **every** order-submitting component must
  hold a kill handle — the contract) and `admit(&self, &OrderIntent) -> Admission`. Provided methods
  `kill_precheck()` (→ `Some(FlattenAndHalt)` when tripped) and `ensure_live()` (→ `Err(Fatal/Halt)`
  when tripped) give every gate the flatten-and-halt behaviour for free, wired to QE-004.
- `assert_honours_kill_switch<G: OrderGate>(&G)` — the reusable **conformance check**: an untripped
  gate admits; after `trip`, `kill_precheck` is `FlattenAndHalt` and `ensure_live` is a `Halt`
  disposition. AC #2.

### `qe-runtime` wiring (AC #1)
Add `qe-risk` dep; define `pub trait OrderPort: qe_risk::OrderGate {}` and re-export `KillHandle`, with
a runtime-level test that a sample `OrderPort` passes `assert_honours_kill_switch`. The runtime's
order interface now references the contract by type.

### Dependencies / topology
`qe-risk`: `qe-domain`, `qe-error`, `rust_decimal`, `serde`, `thiserror`. `qe-runtime` gains
`qe-risk`. Neither pulls `qe-wfo`/`qe-ensemble`, so the QE-001 topology guard stays green.

## Test plan (proves both ACs)

- **AC #1 (compiles + referenced by runtime interface):** `qe-runtime` depends on `qe-risk` and
  `OrderPort: qe_risk::OrderGate`; a runtime test builds a sample port and runs the conformance check —
  the workspace build + that test prove the reference compiles and is honoured.
- **AC #2 (conformance: must accept a kill handle):** `assert_honours_kill_switch` is exercised in
  both `qe-risk` (sample gate) and `qe-runtime` (sample port). The trait makes holding a `KillHandle`
  a compile-time requirement; the conformance fn asserts the kill semantics (untripped admits; tripped
  → `FlattenAndHalt` + `Halt` disposition).
- Unit: `default_outcome` per kind; `Leverage`/`Fraction` construction + **serde-rejection** (negative
  leverage, fraction > 1, both rejected on deserialize); `RiskLimits` serde round-trip; `KillHandle`
  latches and shares state across clones; `ensure_live` disposition is `Halt`.

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **Contract vs enforcement:** this ticket deliberately ships types + the conformance check, not the
  limit-checking maths (QE-215/216). The `OrderGate`/`KillHandle` shapes are what those tickets build
  on; getting them right now is the point.
- **Serde bypass (QE-007 class):** pre-empted — `Leverage`/`Fraction` validate on deserialize, with
  rejection tests.
- **Kill-handle concurrency:** `Arc<AtomicBool>` + `Mutex<reason>`, latching with `SeqCst`, so trips
  are visible across threads/clones deterministically.
