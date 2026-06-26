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

## QE-009 — Risk-limit & kill-switch contract (shared types) — PR #9 — [Ready-for-review]

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
- [ ] The contract compiles and is referenced by the runtime crates' interfaces.
- [ ] A conformance test asserts any order-submitting component must accept a kill handle.

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
