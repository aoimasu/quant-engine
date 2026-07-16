# QE-421 — Adopt the `qe-error` recoverability taxonomy on the runtime order path

`Phase: cross-cutting` · `Area: architecture / error-strategy` · `Depends on: QE-268` · `Effort: L`

## Goal / AC

- Every error reachable on the order-emission path maps to a `Disposition`.
- A synthetic **fatal** error drives to `Halt` (test), and the mapping is **non-vacuous** (a non-fatal
  error drives to `Retry`/`Continue`).
- Complementary to QE-268 (panic-freedom on the live order path) — routing must stay panic-free.

## Current state (evidence)

- `crates/error/src/lib.rs` already defines the taxonomy: `ErrorClass { Transient, Data, Fatal }`,
  `Disposition { Continue, Retry, Halt }`, and a free `disposition(&QeError) -> Disposition`
  (`Transient→Retry`, `Data→Continue`, `Fatal→Halt`). There is **no** trait abstracting `class()` over
  crate-specific error enums.
- Only `qe-clock` (`skew.rs`) and `qe-risk` (`gate.rs`) consume it, and they do so by **producing a
  `QeError`** and calling the free `disposition`. `qe-risk` already depends on `qe-error`.
- `qe-runtime` has **zero** `ErrorClass`/`Disposition` usage. The order-emission path
  (`transport.rs::PlannerAdapterLink`, `kill_gate.rs::VenueKillGate`, `pretrade.rs`) rolls its own
  `thiserror`/plain enums with no recoverability dimension. Concretely, the live dispatch loop
  (`PlannerAdapterLink::apply_revision`) **silently swallows** the kill-halt with `if let Ok(fill) = …`
  — there is no uniform halt-vs-retry-vs-skip decision.
- `qe-venue` has no `qe-error` dependency; its `RestError`/`WsError`/`UserDataError` carry an implicit
  retry/fatal split only in prose.

## Design

### 1. `qe-error`: a `Classified` trait + `ErrorClass::disposition`

Add a small trait so any crate-local error enum can report its class, and the runtime can disposition it
without knowing the concrete type:

```rust
pub trait Classified {
    fn class(&self) -> ErrorClass;
    fn disposition(&self) -> Disposition { self.class().disposition() } // default
}

impl ErrorClass {
    pub fn disposition(self) -> Disposition { /* Transient→Retry, Data→Continue, Fatal→Halt */ }
}
```

- The existing free `disposition(&QeError)` is retained (clock/risk call it) and refactored to delegate to
  `err.class().disposition()` — single source of truth, no behaviour change.
- `impl Classified for QeError` for uniformity (its inherent `class()` still resolves first in direct
  calls; identical value).
- `Fatal` **always** maps to `Halt` — the halt-not-panic guarantee, unchanged.

### 2. Per-variant class mapping (rationale)

Coherence: each crate implements `Classified` for its **own** local types (orphan-rule clean).

**Runtime (`crates/runtime/src/classify.rs`)** — order-emission + bootstrap/handoff path:

| Type | Variant | Class | Disposition | Rationale |
|------|---------|-------|-------------|-----------|
| `KillHalt` | (tripped kill) | Fatal | **Halt** | A tripped kill is the deterministic halt path; submission must stop. **The natural fatal on the emission path.** |
| `TransportError` | `Disconnected` | Transient | Retry | Link down; planner awaits reconnect — recoverable. |
| `AppendError` | `(_)` | Data | Continue | Journal append is **non-gating** (QE-301 AC): skip and keep dispatching. |
| `BootstrapError` | `Recon` | Fatal | Halt | Reconstruction invariant broken — cannot safely start. |
| `BootstrapError` | `Rest(e)` | delegates to `RestError` | Retry/Halt | Inherit the REST retry/fatal split. |
| `BootstrapError` | `Decode` | Fatal | Halt | Undecodable history → cannot reconstruct state → halt, don't trade on partial state. |
| `CutoverError` | `EmptyReplay` | Fatal | Halt | No boundary to continue from. |
| `CutoverError` | `Gap` | Fatal | Halt | Skipped/misaligned seam bar — reject rather than trade through a gap. |
| `BootStateError` | `MismatchedEquityPaths` | Fatal | Halt | Reconstructed-state invariant broken; breaker anchor unsafe. |

**Venue (`crates/venue/src/classify.rs`)** — connectivity feeding the live loop:

| Type | Variant | Class | Rationale |
|------|---------|-------|-----------|
| `RestError` | `RateLimited`, `Transient` | Transient (Retry) | Back off + retry (never dropped). |
| `RestError` | `Fatal`, `Exhausted` | Fatal (Halt) | Non-retryable / retry budget spent. |
| `WsError` | `Connect`, `Subscribe`, `Closed` | Transient (Retry) | Reconnect + resubscribe. |
| `UserDataError` | `ListenKey`, `Connect`, `Snapshot` | Transient (Retry) | Renew key + re-snapshot on reconnect (per module contract). |

**Risk (`crates/risk/src/lib.rs`)**:

| Type | Variant | Class | Rationale |
|------|---------|-------|-----------|
| `RiskError` | `NegativeLeverage`, `FractionOutOfRange` | Fatal (Halt) | Misconfigured cap (invariant/config) — construction fails, cannot proceed. |

### 3. Live-loop routing point

The single concrete live dispatch loop is `PlannerAdapterLink` (`transport.rs`). It now routes **every**
order-path error through `disposition()` instead of ad-hoc handling, recording the last disposition
(`last_disposition()` accessor) for observability/testing:

- `apply_revision`: the `gate.submit(...)` result is `match`ed; on `Err(KillHalt)` it routes through
  `disposition()` → `Halt` and submits nothing (behaviour-equivalent to the prior silent swallow, but now
  a uniform, observable decision). Health still reports `Down` from the kill directly.
- `submit_target`: a `Disconnected` send routes to `Retry`.
- `pump`'s journal append: an `AppendError` routes to `Continue` (counted, non-gating — unchanged).

No error text changes; no `unwrap`/`expect`/`panic` introduced.

### 4. "Every reachable error maps to a Disposition" — guarantee

- **Exhaustive per-variant tests** in `classify.rs` (runtime + venue) and `risk/lib.rs`: construct each
  variant, assert its `Disposition`. Adding a variant without a mapping fails to compile (the impl's
  `match` is exhaustive) or fails the test.
- **Trait-bound assertions** (`fn _assert_classified<T: Classified>()`) over every order-path error type
  (runtime + venue types, referenced from runtime; `RiskError` from risk) — a compile-time proof the
  types implement `Classified`.

### 5. AC test (synthetic fatal → Halt, non-vacuous)

- Fatal: a tripped kill drives `PlannerAdapterLink::pump` to route the `KillHalt` to `Halt`
  (`last_disposition() == Halt`), with no fill and `orders_submitted() == 0`.
- Non-vacuous: a `FailingAppendSink` drives the same loop to `Continue` (dispatch unaffected), and a
  disconnected `submit_target` routes to `Retry` — proving the mapping discriminates.
- Plus a standalone `KillHalt.disposition() == Halt` vs `AppendError.disposition() == Continue` /
  `TransportError::Disconnected.disposition() == Retry`.

## Firewall / goldens / panic-freedom

- **Firewall**: new edges `qe-runtime → qe-error` and `qe-venue → qe-error`. `qe-error` is a foundational
  **leaf** (only external `thiserror`; already depended on by `qe-clock`/`qe-risk`) and appears in **no**
  `forbidden` list of `firewall_rules()` (which guard wfo/ensemble/server ⟂ runtime/venue). The edges
  cannot create forbidden reachability. Verified by `firewall.rs`.
- **Goldens / vintage hash**: this is error-classification plumbing only — no error **text**, no
  serialization, no content changes. The determinism/vintage golden tests must stay byte-identical
  (verified by `cargo test --all`); if any golden moves, STOP (classification leaked into content).
- **Panic-freedom (QE-268/267)**: routing uses only exhaustive `match`; no `unwrap`/`expect`/`panic` in
  non-test code (`clippy::unwrap_used = deny` workspace-wide holds).

## Risks

- Over-classifying a genuinely-transient error as `Fatal` would halt the book unnecessarily; mappings are
  conservative and match each type's documented recovery semantics (venue connectivity → Retry; kill /
  invariant / config → Halt).
- The routing is intentionally behaviour-preserving: the kill already halted submission; QE-421 makes the
  decision **uniform and observable**, not different.
