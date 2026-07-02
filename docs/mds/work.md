# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. A PR entry is added here when it
reaches review, the dedicated review agent writes `[Reviewed]`/`[Approved]` + comments inline, and on
merge the approved block is archived to `docs/mds/reviewed/<ticket>.md` and this file is **cleared back
to empty**. No running "Completed" list is kept here — the traceable history lives solely in
`docs/mds/reviewed/`.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

---

## QE-219 — Vintage load (read-only) + rollover — [Ready-for-review]

- **PR:** #70 — https://github.com/aoimasu/quant-engine/pull/70
- **Ticket:** QE-219 (`Phase: P2` · `Area: ① Vintage inputs` · `Depends on: QE-129, QE-207`)
- **Branch:** `qe-219/vintage-rollover`
- **Latest commit:** `98558aeb90f1c39c9ce04d71adf0718b8d728cc5`
- **Evidence / design:** `docs/architecture/qe-219-vintage-rollover-design.md`
- **Changed files:** `crates/runtime/src/vintage_rollover.rs` (new), `crates/runtime/src/lib.rs` (module +
  re-exports), `crates/runtime/Cargo.toml` (promote `qe-determinism` to a direct dep), design note. (Also
  archives QE-218 → `docs/mds/reviewed/qe-218.md` + clears the prior `work.md` entry.)

### Goal
Runtime loads the ensemble repo + calibration profile **read-only** at startup; periodic **rollover** replaces
the vintage in place when training emits a new one, without violating the firewall.

### Acceptance criteria (from backlog)
- [x] Startup loads a vintage read-only; a rollover swaps it atomically with lineage recorded —
  `startup_loads_vintage_read_only`, `rollover_swaps_in_place_with_lineage_recorded`,
  `rollover_rejects_unverified_vintage_keeping_current`.

### Implementation summary
- New `crates/runtime/src/vintage_rollover.rs`: `ActiveVintage` holds the current sealed `Vintage` + a
  `RolloverRecord` history. `load(repo, id)` uses `VintageRepository::load` (open + **hash-verify**, never
  write) — read-only startup. `rollover(next)`/`rollover_from(repo, id)` **verify before** the single
  `current = next` swap (atomic: a bad vintage never becomes active; repo + calibration — calibration lives
  inside `current.content` — never come from two vintages). Every rollover records `from/to vintage_id` +
  `from/to Lineage`.
- Promotes `qe-determinism` (`Lineage`) to a direct dependency — cross-cutting (QE-006), not on either side of
  the QE-132 firewall, already transitive via `qe-vintage`; firewall test green.
- **Scrutinise:** (1) atomicity is verify-before-commit on a single-threaded runtime — is that the right
  reading of "atomically", or is an `Arc`-swap expected now? (2) read-only proven by *using only* `load` (never
  `write`) + a byte-unchanged assertion — sufficient? (3) unbounded `history` growth across many rollovers —
  acceptable at rollover cadence? (4) `qe-determinism` promoted dev→direct dep — right call vs re-exporting
  `Lineage` from `qe-vintage`? (5) rollover accepts *any* verified vintage (no monotonic-lineage / same-id
  guard) — right boundary, or should a no-op/backwards rollover be rejected?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 559 passed / 1 ignored / 56 suites (+6 vintage_rollover tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok
