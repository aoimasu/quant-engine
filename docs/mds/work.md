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

---

## QE-007 — Shared domain types — PR #7 — [Ready-for-review]

- **Branch:** `qe-007/shared-domain-types`
- **PR:** https://github.com/aoimasu/quant-engine/pull/7
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-007-shared-domain-types-design.md`
- **Changed surface:** fills the `crates/domain` scaffold — `src/{lib,money,instrument,time,
  resolution,bar,funding,side,vintage}.rs`, `Cargo.toml`; root `Cargo.toml` (+`rust_decimal`
  workspace dep, +`proptest` dev workspace dep). Also bundles the QE-006 archive
  (`docs/mds/reviewed/qe-006.md`) — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [ ] Money arithmetic is exact (property tests for associativity/rounding policy).
- [ ] Bar/resolution types are shared by both pipelines (single definition).

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-domain` 29 tests pass (incl. 4 proptest laws); workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (proptest pulls a 2nd `rand` major →
  `multiple-versions = "warn"`, non-fatal)

Key AC-proving tests:
- **AC #1 (exact money)** — `money.rs` proptests: `notional_addition_is_associative` /
  `_is_commutative` / `notional_sub_inverts_add` (exact equality), and
  `rounding_stays_within_one_ulp_and_target_scale` (every `RoundingPolicy`: `scale() ≤ target` and
  `|rounded − exact| < 10^-scale`); unit tests for negative rejection, banker-vs-half-up midpoint,
  exact-string serde round-trip.
- **AC #2 (single bar/resolution definition)** — `Resolution` defined once in `qe-domain`,
  `FromStr`/`Display`/`minutes` round-trip tests; `Bar::new` OHLC-invariant validation tests. Both
  pipelines consume the one re-exported definition.

### Design notes for the reviewer
- Money is `rust_decimal::Decimal` (96-bit fixed-point, no binary float); the only rounding point is
  `Price::notional(qty, scale, policy)`. Decimals serialise as strings for exact JSON round-trips.
- Wiring `qe-config`'s string resolutions onto `Resolution` is intentionally deferred to QE-012 (its
  scope) to avoid touching an already-merged crate here.
- `qe-domain` keeps zero internal-crate deps, so the QE-001 topology guard is unaffected (re-run green).
