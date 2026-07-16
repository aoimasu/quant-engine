# QE-426 — Split the `qe-runtime` god-crate along the spec's process seams

- **Ticket:** QE-426 (Phase 3 prep · architecture / crate-boundaries · Effort L)
- **Spec of record:** `### QE-426` in `docs/reviews/2026-07-15-team-improvement-review.md:659-681`
- **Nature:** PURE MOVE. No behaviour change, no wire-format change, existing runtime tests pass unchanged.

## 1. Current state — inventory & concern classification

`qe-runtime` is ~6.6k LOC / 18 flat modules re-exporting ~50 types (`crates/runtime/src/lib.rs`). The
modules classify into the four spec concerns + a shared contract:

| module | LOC | concern | internal deps (`crate::`) | order-path panic-free attr |
|---|---|---|---|---|
| `boot_state` | 415 | Bootstrap ③ | `evaluator` | no |
| `bootstrap` | 725 | Bootstrap ③ | `evaluator`, `live_kline` | no |
| `cutover` | 400 | Bootstrap ③ | `bootstrap`, `evaluator` | no |
| `evaluator` | 354 | Live ④ | `factor_join` | **yes** |
| `factor_join` | 278 | Live ④ | — | no |
| `live_kline` | 282 | Live ④ | — | no |
| `live_mark` | 323 | Live ④ | — | no |
| `live_netter` | 307 | Live ④ | — | **yes** |
| `live_breakers` | 487 | Live ④ | `boot_state`, `evaluator` | **yes** |
| `hedger` | 259 | Hedge ⑤ | `live_netter` | **yes** |
| `pretrade` | 502 | Hedge ⑤ | `hedger` | **yes** |
| `vintage_rollover` | 324 | Hedge ⑤ (lifecycle) | — | no |
| `edge` | 610 | Edge ⑥ (order submit) | `hedger` | **yes** |
| `kill_gate` | 345 | Edge ⑥ (kill on submit) | `edge` | **yes** |
| `transport` | 634 | Edge ⑥ (gRPC adapter seam) | `edge`, `hedger`, `kill_gate` | no |
| `reconciliation` | 380 | Edge ⑥ | — | no |
| `shadow` | 377 | Edge ⑥ (G2 dry-run) | `edge`, `kill_gate`, `reconciliation`, `transport` | no |
| `classify` | 162 | cross-cutting (QE-421 error taxonomy) | `boot_state`, `bootstrap`, `cutover`, `kill_gate`, `transport` | no |
| `lib.rs` | 124 | facade / `OrderPort` trait / `crate_name` | — | n/a |

**The fusion problem.** `edge` (⑥) `impl`s `crate::hedger::PositionKeeper` and uses `crate::hedger::CapitalView`;
`transport` (⑥) carries `crate::hedger::TargetPosition` inside `TargetRevision`. These three types are the
**wire/seam contract** the planner (⑤) produces and the adapter (⑥) consumes — but today they live inside the
`hedger` module, so the "separate colocated processes over gRPC" boundary (QE-218) is not a compile boundary.

## 2. Target topology

Three new crates + `qe-runtime` retained as a **thin facade** (lowest consumer churn):

```
qe-runtime-core   (contract: TargetPosition, CapitalView, PositionKeeper + &K blanket impl)
        ▲                        ▲
        │                        │
   qe-hedger  ───(no edge dep)   qe-edge   (order submission: venue adapter / keeper / kill gate / transport)
   (Bootstrap③+Live④+Hedge⑤)        │
        ▲                        ▲
        └──────── qe-runtime ────┘   (facade: re-exports the full public API)
                       ▲
                    qe-cli  (unchanged: `use qe_runtime::{HistoricalSource, BootstrapError, HistoricalWindow, BreakerLayer}`)
```

The **gRPC seam becomes a crate boundary**: `qe-hedger` produces the absolute `TargetPosition` (in
`qe-runtime-core`); `qe-edge` owns `PlannerAdapterLink` (the adapter-side server), `TargetRevision`,
`AdapterReport`, backpressure/reconnect. `qe-hedger` has **no** dependency on `qe-edge` (prod), and `qe-edge`
has **no** dependency on `qe-hedger` (prod) — they meet only through the shared contract in `qe-runtime-core`,
exactly as two processes over the wire would.

### Module → crate map

- **`qe-runtime-core`** (`crates/runtime-core`): `TargetPosition`, `CapitalView`, `PositionKeeper` (extracted
  from `hedger.rs`) + the `impl PositionKeeper for &K` blanket (moved from `edge.rs`, orphan-legal only where
  the trait is defined). Dep: `qe-domain`.
- **`qe-hedger`** (`crates/hedger`): `boot_state`, `bootstrap`, `cutover`, `evaluator`, `factor_join`,
  `live_kline`, `live_mark`, `live_netter`, `live_breakers`, `hedger` (now `HedgePlanner` only), `pretrade`,
  `vintage_rollover`, and `classify` (the hedger-side `Classified` impls: `BootstrapError`, `CutoverError`,
  `BootStateError`). Deps: `qe-domain`, `qe-error`, `qe-risk`, `qe-signal`, `qe-vintage`, `qe-venue`,
  `qe-determinism`, `qe-runtime-core`, `rust_decimal`, `thiserror`.
- **`qe-edge`** (`crates/edge`): `edge`, `kill_gate`, `transport`, `reconciliation`, `shadow`, `OrderPort`
  trait, and `classify` (the edge-side `Classified` impls: `KillHalt`, `TransportError`, `AppendError`). Deps:
  `qe-domain`, `qe-error`, `qe-risk`, `qe-venue`, `qe-runtime-core`, `rust_decimal`. Dev-dep: `qe-hedger`
  (the `edge.rs` end-to-end test drives `HedgePlanner`+`NetTarget` through the real `VenueKeeper` — dev-only,
  firewall-excluded, no prod edge→hedger edge).
- **`qe-runtime`** (facade, `crates/runtime`): re-exports the full public surface (types **and** module paths
  `boot_state`/`evaluator`/… so `qe_runtime::boot_state::X` keeps resolving for the integration tests), keeps
  `crate_name()`, hosts the cross-cutting classify taxonomy test (needs both sides + venue). Deps:
  `qe-runtime-core`, `qe-hedger`, `qe-edge`, `qe-risk`. Dev-deps for the retained integration tests:
  `qe-domain`, `qe-signal`, `qe-vintage`, `qe-determinism`, `qe-error`, `qe-venue`, `rust_decimal`.

**`qe-storage` is dropped** — grep shows zero `qe_storage` usage anywhere in `crates/runtime/`; it was a dead
dependency. Dropping an unused dep is behaviour-neutral.

## 3. gRPC / transport crate-boundary plan

`transport.rs` splits conceptually — but only the *contract* it names crosses the seam:
- `TargetRevision` embeds `TargetPosition` → `TargetPosition` moves to `qe-runtime-core`; `transport` (in
  `qe-edge`) imports it from core.
- `AdapterReport` embeds `SimFill` (an edge type) and `PlannerAdapterLink` **owns** `VenueKeeper` +
  `VenueKillGate` (edge state) → the whole `PlannerAdapterLink` server is intrinsically adapter-side and stays
  in `qe-edge`. In the current codebase no `hedger`-side module constructs `TargetRevision` (only `transport`
  + `shadow`, both edge-side, + tests), so `qe-hedger` never needs the transport types — the seam is clean.

## 4. Firewall extensions (QE-405)

Extend `crates/architecture/src/lib.rs::firewall_rules()` and `crates/architecture/tests/firewall.rs`:

- **Existing live-side edges strengthened** — the "live" side is now five crates, not two. Add
  `qe-runtime-core`, `qe-hedger`, `qe-edge` to the `forbidden` list of each existing rule:
  - `qe-wfo ⊬ { qe-ensemble, qe-runtime, qe-venue, qe-runtime-core, qe-hedger, qe-edge }`
  - `qe-ensemble ⊬ { qe-wfo, qe-runtime, qe-venue, qe-runtime-core, qe-hedger, qe-edge }`
  - `qe-server ⊬ { qe-runtime, qe-venue, qe-runtime-core, qe-hedger, qe-edge }`
- **New: the order-submitting crate is the security boundary — its deps stay tight:**
  - `qe-edge ⊬ { qe-wfo, qe-ensemble, qe-vintage, qe-signal, qe-hedger }` — the order path links neither
    search/portfolio, nor the genome/vintage eval, nor the planner. (edge reaches only core/domain/error/risk/venue.)
- **New: the planner does not link the adapter (the seam is a compile boundary):**
  - `qe-hedger ⊬ { qe-edge, qe-wfo, qe-ensemble }` — realises "runtime side ⊬ training" for the split (QE-405
    intent) and asserts the process seam.
- **New: the shared contract is a pure leaf:**
  - `qe-runtime-core ⊬ { qe-edge, qe-hedger, qe-venue, qe-risk, qe-signal }` — a contract crate depends only
    on `qe-domain`.
- **Non-vacuity sanity edges** added to the test: `reachable(qe-edge) ∋ qe-venue`,
  `reachable(qe-hedger) ∋ qe-vintage`, `reachable(qe-runtime-core) ∋ qe-domain`,
  `reachable(qe-runtime) ∋ qe-edge` — plus the three new crates added to the "graph really parsed" presence check.

All rules verified true against the leaf dep graph (`core→domain`; `edge→{core,domain,error,risk,venue}`;
`hedger→{core,domain,error,risk,signal,vintage,venue,determinism}`).

## 5. Panic-freedom lint carry-over (QE-268)

QE-268 scoped panic-freedom **per module** via `#![deny(clippy::unwrap_used, clippy::expect_used,
clippy::panic)]` (seven modules; `clippy.toml` allows unwrap/expect/panic in tests). This is preserved
**verbatim** as modules move:
- Order-submission modules `edge.rs` + `kill_gate.rs` carry their deny attribute into **`qe-edge`** — the
  order path is panic-free-scoped inside its own crate, which is the point (independent lint scoping).
- `evaluator`, `live_netter`, `live_breakers`, `hedger`, `pretrade` carry theirs into `qe-hedger`.
- `qe-runtime-core` gets the same crate-level attribute on the extracted contract (clean; it came from the
  `hedger.rs` order-path module). `shadow`/`transport`/`reconciliation` never had the attr (shadow has a
  production `.expect`) — not added, preserving pure-move.

## 6. Consumer-churn plan

Only real consumer of `qe-runtime` types is **`qe-cli`** (`HistoricalSource`, `BootstrapError`,
`HistoricalWindow`, `BreakerLayer` — all hedger-side). `qe-server`/`qe-signal`/`qe-storage` mention
`qe-runtime` only in comments. The facade re-exports every type + module path, so **zero consumer edits**.

Integration tests under `crates/runtime/tests/` (`breaker_seed`, `restart_parity`, `order_port_conformance`)
reference `qe_runtime::boot_state::X`, `qe_runtime::evaluator::X`, `qe_runtime::{BreakerLayer, OrderPort}` —
all satisfied by facade re-exports (module + type). They stay **byte-identical in place**.

## 7. How behaviour / wire / goldens stay identical

- No logic edits: modules move via `git mv`; only `use` paths change (`crate::hedger::{TargetPosition,
  CapitalView, PositionKeeper}` → `qe_runtime_core::…`; cross-crate `crate::X` → `qe_hedger::X`). Same code,
  same arithmetic, same types.
- No wire/serde types change; `TargetPosition`/`AdapterReport`/`TargetRevision` fields untouched. No vintage
  or golden fixture is touched — goldens verified byte-identical post-move.
- Unit tests move **with** their module (they are `#[cfg(test)] mod tests` inside each file). The one
  cross-cutting `classify` test (spans edge + hedger + venue error types) moves to the facade as
  `crates/runtime/tests/classify_taxonomy.rs`, assertions verbatim, imports re-prefixed.

## 8. Tests: move-but-stay-green

- `edge.rs`, `hedger.rs`, `transport.rs`, `shadow.rs`, `pretrade.rs` unit-test modules: kept verbatim except
  mechanical import re-prefix for the moved contract types.
- `crates/runtime/tests/*` (the three integration tests): unchanged, in place, green via the facade.
- `classify` impls split by orphan rule (a foreign `Classified` impl must live in the type's crate); its test
  module relocates to the facade.

## 9. Risks & rollback

- **Risk:** orphan-rule violations if the `PositionKeeper for &K` blanket or a `Classified` impl lands in the
  wrong crate → mitigated by placing the blanket in `qe-runtime-core` (trait home) and each `Classified` impl
  in the crate owning the error type; `cargo build` is the check.
- **Risk:** a facade re-export gap breaks `qe-cli` or an integration test → mitigated by re-exporting the full
  prior surface (types + module paths) and running `cargo test --all`.
- **Risk:** a golden shifts → would mean the move was not pure; the plan touches no vintage/serde/logic, and
  goldens are verified byte-identical in the green gate. If one moves: STOP.
- **Rollback:** the change is additive crate scaffolding + `git mv`s + import edits on one branch; revert the
  branch. No data migration, no wire change.
