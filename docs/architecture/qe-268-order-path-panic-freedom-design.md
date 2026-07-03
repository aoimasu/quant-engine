# QE-268 — Enforce panic-freedom on the live order-emission path

`Phase: Hardening` · `Area: runtime / safety` · `Depends on: QE-267` · `Priority: P2`
`Spec ref: workspace-review 2026-07-04 (finding 3); crates/error/src/lib.rs §hot_path.`

## Goal

`qe-error` documents that *modules on the order-emission path must reject `unwrap`/`expect`/`panic`*
and ships a `hot_path` demonstrator (`crates/error/src/lib.rs`) plus `tests/hot_path_lint.rs` proving
clippy enforces the `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` attribute. That
attribute today decorates **only** `error::hot_path`. The **actual** live path —
`runtime::{edge, kill_gate, pretrade, hedger, live_netter, live_breakers, evaluator}` — carries none of
it, and a handful reconstruct validated newtypes via `.expect()` on in-range arithmetic. This ticket
puts the deny attribute on all seven modules and converts every production `.expect()` on the path to a
proof-carrying, total alternative — so the single most safety-critical invariant of a live-capital
engine is compiler-enforced, not maintained by convention.

## Acceptance criteria

- Every listed order-emission module carries the deny attribute.
- `cargo clippy --workspace --all-targets` is green.
- Introducing an `unwrap`/`expect`/`panic!` into any of those modules fails clippy (proved below).
- Live behaviour unchanged (`order_port_conformance` + `restart_parity` green).

## Production panic-family inventory (verified by grep, not trusting the ticket's line hints)

The seven modules were scanned for every non-`#[cfg(test)]` `.expect(` / `.unwrap(` / `panic!` /
`unreachable!` / `unimplemented!` / `todo!`. Result — exactly **7 production call sites**, all `.expect()`:

| # | File | Line | Current | Enclosing fn (return) | Invariant |
|---|------|------|---------|-----------------------|-----------|
| 1 | `crates/runtime/src/edge.rs` | 90 | `Qty::new(self.filled.get() + q.get()).expect("cumulative fill is non-negative")` | `Order::on_fill(&mut self, q: Qty)` → `()` | sum of two non-negative `Qty` is non-negative |
| 2 | `crates/runtime/src/edge.rs` | 143 | `Qty::new(mag).expect("delta magnitude is non-negative")` | `plan_delta(..)` → `Option<OrderIntent>` | `mag = delta.abs()` |
| 3 | `crates/runtime/src/edge.rs` | 344 | `Qty::new(-self.signed_qty).expect("magnitude is non-negative")` | `position_report(..)` → `PositionReport` | in the `is_sign_negative` branch, `-signed_qty = signed_qty.abs()` |
| 4 | `crates/runtime/src/edge.rs` | 349 | `Qty::new(self.signed_qty).expect("magnitude is non-negative")` | `position_report(..)` → `PositionReport` | in the positive branch, `signed_qty = signed_qty.abs()` |
| 5 | `crates/runtime/src/kill_gate.rs` | 57 | `Qty::new(mag).expect("flatten magnitude is non-negative")` | `flatten_intent(..)` → `Option<OrderIntent>` | `mag = current_qty.abs()` |
| 6 | `crates/runtime/src/live_breakers.rs` | 28 | `Fraction::new(Decimal::ONE).expect("1.0 is a valid fraction")` | `never_fires()` → `Fraction` | `1.0 ∈ [0,1]` (constant) |
| 7 | `crates/runtime/src/live_breakers.rs` | 35 | `Fraction::new(Decimal::ZERO).expect("0.0 is a valid fraction")` | `fires_immediately()` → `BreakerThresholds` | `0.0 ∈ [0,1]` (constant) |

**Modules with ZERO production panics** (deny attribute alone suffices, no code change):
`pretrade.rs`, `hedger.rs`, `live_netter.rs`, `evaluator.rs`. Notes on constructs that are **not**
flagged by these lints and so are deliberately left untouched:
- `live_netter.rs:133` `assert!(...)` in `net_positions` — `clippy::panic` matches only the `panic!`
  macro, not `assert!`; this is a documented hard-fail on mis-aligned capital-affecting slices and stays.
- `live_netter.rs:31` `debug_assert!(...)` — likewise not matched.
- `live_netter.rs:35` `Decimal::from_f64_retain(weight).unwrap_or(Decimal::ZERO)` — `unwrap_or`, not
  `unwrap`; not matched by `clippy::unwrap_used`.

## Conversions (proof-carrying, behaviour-preserving)

Priority order per ticket: (a) infallible const / total constructor, (c) total arithmetic returning the
newtype. No case needed a `Result` signature change — every enclosing fn keeps its exact signature.

### Supporting domain additions

**`crates/domain/src/money.rs`:**
- `Qty::abs_of(value: Decimal) -> Qty` — infallible constructor returning `Qty(value.abs())`, always
  non-negative by construction. Replaces the "reconstruct a magnitude via fallible `new` + `expect`"
  idiom for sites #2–#5. This is a *total* function: the magnitude of any decimal is non-negative, so
  the `[0, ∞)` invariant holds for every input; no error path exists to `expect` away.
- `impl Add for Qty` — total addition returning `Qty(self.0 + rhs.0)`, mirroring the existing
  `impl Add for Notional`. Sum of two non-negative `Qty` is non-negative, so the newtype invariant is
  preserved without re-validating. Used for site #1 (`self.filled = self.filled + q`). Same overflow
  caveat as `Notional`'s `Add` (panics only on 96-bit decimal overflow — well outside realistic fill
  magnitudes; `checked_add` exists on `Notional` for the overflow-possible case, not needed here).

**`crates/risk/src/limit.rs`** (where `Fraction` actually lives — the ticket text says `money.rs`, but
`Fraction` is a `qe_risk` type; the const belongs beside its definition):
- `Fraction::ZERO` and `Fraction::ONE` associated consts (`const ZERO: Fraction = Fraction(Decimal::ZERO)`
  etc.), mirroring `Price::ZERO` / `Qty::ZERO`. `Decimal::ZERO` / `Decimal::ONE` are `const`, and both
  are trivially in `[0, 1]`, so the consts are total and need no runtime check. Used for sites #6, #7.

Propagating a `Result` out of `never_fires()`/`fires_immediately()` (option b) was rejected: it would
ripple through `BreakerLayer::new` / `from_calibration` (which return `Self`) into every caller — a wide
signature change for two compile-time constants. The associated-const route is priority (a) and minimal.

### Per-site conversion

| # | After |
|---|-------|
| 1 | `self.filled = self.filled + q;` (uses `Add for Qty`) |
| 2 | drop the `mag` binding; `qty: Qty::abs_of(delta)` with `side` derived from `delta.is_sign_negative()` |
| 3 | `Qty::abs_of(self.signed_qty)` |
| 4 | `Qty::abs_of(self.signed_qty)` |
| 5 | drop the `mag` binding; `qty: Qty::abs_of(current_qty)` with `side` derived from sign |
| 6 | `Fraction::ONE` |
| 7 | `Fraction::ZERO` |

For #2 and #5, `-delta` (resp. `-current_qty`) in the negative branch equals `delta.abs()`, and `delta`
in the positive branch equals `delta.abs()`; `abs_of` collapses both branches' magnitude to one total
call while `side` still branches on the sign. Behaviour is bit-identical.

## clippy.toml additions

QE-267 left `clippy.toml` with `allow-unwrap-in-tests = true`. The per-module
`#![deny(clippy::expect_used, clippy::panic)]` is stricter than the workspace lint set, so the colocated
`#[cfg(test)]` modules (which use `.expect(...)` and `panic!(...)` — e.g. `kill_gate.rs` test match arms,
`edge.rs` `.expect("submits while live")`) would now fail. Add alongside the existing allow:

```
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests  = true
```

## Deny attribute placement

Each of the seven files gets, immediately after its `//!` module docs and before the first `use`:

```rust
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
```

(inner attributes must precede all items including `use`), exactly as `error::hot_path` applies it.

## Prove-it

With the attributes in place and conversions done, temporarily insert a bare `.expect("x")` and (separately)
a `panic!("x")` into one production module, run
`cargo clippy --workspace --all-targets --locked -- -D warnings`, confirm each is rejected naming its
lint, then revert. Result pasted below.

**Result (2026-07-04).** Injected `_prove.expect("PROVE-IT expect")` and (behind `if false`)
`panic!("PROVE-IT panic")` into `live_breakers::never_fires()`, then
`cargo clippy -p qe-runtime --all-targets --locked -- -D warnings`:

```
error: used `expect()` on an `Option` value
  --> crates/runtime/src/live_breakers.rs:32:13
   = note: if this value is `None`, it will panic
   = help: ...index.html#expect_used
  --> crates/runtime/src/live_breakers.rs:19:30
19 | #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

error: `panic` should not be present in production code
  --> crates/runtime/src/live_breakers.rs:34:9
34 |         panic!("PROVE-IT panic");
   = help: ...index.html#panic
  --> crates/runtime/src/live_breakers.rs:19:51
19 | #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

error: could not compile `qe-runtime` (lib) due to 3 previous errors
```

Both lints fire, each anchored to the module's deny attribute (line 19). Reverted immediately after.

## Second lint fixture?

The existing `crates/error/tests/hot_path_lint.rs` compile-fail fixture already proves clippy rejects
`unwrap`/`expect`/`panic` in a `deny`-guarded module generically — it is not specific to `error::hot_path`.
The green gate (`cargo clippy --workspace --all-targets --locked -- -D warnings`) then transitively proves
the seven runtime modules compile clean under the attribute, and the prove-it step above demonstrates a
violation is caught in one of *these* modules. Duplicating the fixture per module would add maintenance
cost without additional coverage, so **no second fixture is added** — the green gate + the existing
generic proof + the manual prove-it suffice.

## Test plan

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked` — must include `order_port_conformance`
  (`crates/runtime/tests/order_port_conformance.rs`) and `restart_parity`
  (`crates/runtime/tests/restart_parity.rs`) passing; plus new unit tests for `Qty::abs_of`,
  `Add for Qty`, and `Fraction::ZERO`/`ONE`.
- `cargo deny check` — no dependency change, expected to stay green.

## Risks

- **Behaviour drift on a conversion** (a hot-path order-emission function → a wrong conversion is a
  live-trading risk). Mitigated: every conversion is body-only with no signature change; `abs_of` and
  `Add for Qty` reproduce the exact arithmetic the `expect` wrapped; `order_port_conformance` and
  `restart_parity` exercise the full plan→delta→fill→keeper loop and the flatten path.
- **`abs_of` masking a genuinely-negative input.** By design `abs_of` takes the magnitude, so it can
  never produce a negative `Qty`; at all five sites the value was already a magnitude (the code branched
  on sign and negated), so `abs_of` is exactly what was meant — no behaviour change, and no silent
  sign loss versus the prior `-delta` / `delta` selection.
- **New public surface** (`Qty::abs_of`, `Add for Qty`, `Fraction::ZERO`/`ONE`) — additive only, no
  existing signature changes, so no caller ripple.
</content>
</invoke>
