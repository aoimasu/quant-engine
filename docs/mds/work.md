# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.

---

## QE-009 — Risk-limit & kill-switch contract (shared types) — PR #9 — [Reviewed]

- **Branch:** `qe-009/risk-kill-switch-contract`
- **PR:** https://github.com/aoimasu/quant-engine/pull/9
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-009-risk-kill-switch-contract-design.md`
- **Changed surface:** new crate `crates/risk` (`src/{lib,limit,kill,gate}.rs`, `Cargo.toml`);
  `qe-runtime` now depends on `qe-risk` and defines `OrderPort: qe_risk::OrderGate`
  (`crates/runtime/src/lib.rs`, `tests/order_port_conformance.rs`, `Cargo.toml`); root `Cargo.toml`
  (+`qe-risk` path dep). Also bundles the QE-008 archive (`docs/mds/reviewed/qe-008.md`) — branch
  protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] The contract compiles and is referenced by the runtime crates' interfaces.
      _(Genuine: `OrderPort: qe_risk::OrderGate` is a real supertrait bound — being an `OrderPort`
      compile-time-requires holding a `KillHandle`; the runtime conformance test exercises a
      runtime-layer port.)_
- [x] A conformance test asserts any order-submitting component must accept a kill handle.
      _(Literal "accept a handle" met — `kill_handle()` is a required method. BUT the conformance
      check doesn't verify the component **honours** the kill on its `admit` path — see Feedback #1,
      a demonstrated hole for a kill-switch contract.)_

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-risk` 10 unit tests + `qe-runtime` conformance test pass; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok

Key AC-proving tests:
- **AC #1 (compiles + referenced by runtime interface)** — `qe-runtime` depends on `qe-risk` and
  defines `OrderPort: qe_risk::OrderGate`; `crates/runtime/tests/order_port_conformance.rs` builds a
  sample `OrderPort` *constructed with* a `KillHandle` and runs the conformance check (workspace build
  + that test prove the reference compiles and is honoured).
- **AC #2 (must accept a kill handle)** — `qe_risk::assert_honours_kill_switch` is exercised in both
  `qe-risk` (`gate::tests::sample_gate_passes_conformance`) and `qe-runtime`. The `OrderGate` trait
  makes holding a `KillHandle` a compile-time requirement; the conformance fn asserts the semantics
  (untripped admits; tripped → `FlattenAndHalt` + `Halt` disposition).

### Design notes for the reviewer
- **Contract, not enforcement** (limit-checking maths is QE-215/216). This ships: `LimitKind` + the
  per-kind `LimitOutcome` policy (`default_outcome`), validated `Leverage`/`Fraction` caps (serde
  rejection too — QE-007 lesson), `RiskLimits`, the out-of-band latching `KillHandle`/`KillSwitch`,
  and the `OrderGate` contract with a reusable conformance check.
- A tripped kill / `Halt`-outcome limit → Fatal `QeError` (`disposition == Halt`), same channel as
  QE-008. `KillHandle` is `Arc`-shared (clones observe the same trip), latching, `SeqCst`.
- `qe-risk` deps (qe-domain/qe-error/rust_decimal) and the `qe-runtime → qe-risk` edge add no
  forbidden runtime↔training dependency → QE-001 topology guard green.

### Review notes

**Verdict: [Reviewed]** — strong contract crate: the kill switch is concurrency-sound, the serde
discipline correctly applies the QE-007 lesson, the per-kind outcome policy is sensible, and the
runtime reference is genuine. Holding short of approval for **one demonstrated hole in the AC #2
conformance check** — for a kill-switch contract it matters that "conformance" actually proves the
order path honours the kill, and right now it doesn't.

**Independent re-verification (branch `qe-009/risk-kill-switch-contract`):**
- `cargo fmt --all --check` clean · `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean · `cargo test --workspace --locked` **107 passed, 1 ignored** (qe-risk 10 + runtime
  conformance) · `cargo deny check` ok · QE-001 topology guard green (`qe-runtime → qe-risk` adds no
  forbidden runtime↔training edge).

**Focus areas verified positively:**
- **AC #1 — genuine, not cosmetic.** `pub trait OrderPort: qe_risk::OrderGate` is a real supertrait
  bound: you cannot implement `OrderPort` without implementing `OrderGate`, whose required
  `kill_handle()` forces holding a `KillHandle` at compile time. The runtime test builds a
  `SamplePort` (a `qe_runtime::OrderPort`, not just a qe-risk gate), constructed *with* a handle, and
  runs the conformance check — it exercises the runtime layer.
- **Focus 3 — kill switch is concurrency-sound.** `trip` writes the reason **under the mutex** before
  the `SeqCst` `tripped` store; any observer that sees `tripped == true` is forced to observe the
  reason (it either blocks on the still-held reason mutex, or acquires-after-release). "First reason
  wins" is deterministic (mutex-serialised, `if slot.is_none()`); latching is correct (nothing ever
  resets `tripped`); out-of-band via `Arc`-shared clones. `KillHandle` is **not** `Serialize`/
  `Deserialize`, so kill state can't be injected from untrusted data. No race found.
- **Focus 4 — serde discipline clean (no QE-007 regression).** `Leverage`/`Fraction` have manual
  `Serialize` (exact string) + validating `Deserialize` that calls `new` — verified `"-1"` leverage
  and `"1.5"` fraction are rejected on deserialize. `RiskLimits`/`OrderIntent` compose only validated
  types (the QE-007-round-2 `InstrumentId`/`Qty`/`Price` validate on deserialize). No bypassable
  invariant. `default_outcome` (clamp sizing caps; reject portfolio/margin breaches; halt on
  drawdown) is a sensible, conservative policy for a leveraged perp venue.

### Feedback

1. **[Blocker — the AC #2 conformance check doesn't prove the order path honours the kill].**
   `assert_honours_kill_switch` exercises only the **provided** `kill_precheck()` / `ensure_live()`
   helpers (which are wired to the handle and trivially honour a trip) — it **never calls `admit`**.
   So a component can implement `OrderGate`, hold a handle, pass conformance, and still submit orders
   while the kill is tripped. I demonstrated this with a `BadGate` whose `admit` returns
   `Admission::Admit` unconditionally: it **passes `assert_honours_kill_switch`**, yet `admit(intent)`
   returns `Admit` even after the handle is tripped. The "Implementations must call `kill_precheck`
   first" rule is only a doc comment — exactly the convention-vs-enforcement trap QE-007 taught this
   team to close. For a P0 kill-switch contract this is the property that matters most. **Fix (either,
   ideally both):**
   - Strengthen `assert_honours_kill_switch` to also assert the admission path: after the internal
     `trip`, call `gate.admit(&intent)` and require `Admission::FlattenAndHalt(_)` (the fn can take a
     representative `OrderIntent`, or the trait can expose a no-op intent for conformance).
   - Structurally guarantee it: give `OrderGate::admit` a **default** impl that does `kill_precheck`
     first and delegates the limit decision to a new required method (e.g. `admit_within_limits`), so
     a gate can't accidentally (or silently) skip the kill check. Then "admit honours the kill" is a
     compile-time/structural property, not a convention — and the conformance check has teeth.

   _(AC #2's literal text "must **accept** a kill handle" is met — `kill_handle()` is required — so the
   box is ticked; but the conformance check is the AC's deliverable and currently gives false
   assurance, which is why this blocks approval rather than being a nit.)_
