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

## QE-221 — Real-time reconciliation divergence alarm — [Ready-for-review]

- **PR:** #72 — https://github.com/aoimasu/quant-engine/pull/72
- **Ticket:** QE-221 (`Phase: P2` · `Area: ⑨ + risk` · `Depends on: QE-217`)
- **Branch:** `qe-221/recon-divergence-alarm`
- **Latest commit:** `b4789c58e23b28cc580965553a775456b6e106e5`
- **Evidence / design:** `docs/architecture/qe-221-recon-divergence-alarm-design.md`
- **Changed files:** `crates/runtime/src/reconciliation.rs` (new), `crates/runtime/src/lib.rs` (module +
  re-exports), design note. (Also archives QE-220 → `docs/mds/reviewed/qe-220.md` + clears the prior
  `work.md` entry.)

### Goal
*(Reviewer-added.)* Reconciliation should not be post-hoc only; a live position mismatch beyond tolerance
should be a fast safety check that can trip the kill-switch.

### Acceptance criteria (from backlog)
- [x] An injected position desync beyond tolerance raises an alarm and can halt —
  `divergence_beyond_tolerance_alarms_and_halts`.

### Implementation summary
- New `crates/runtime/src/reconciliation.rs`: `ReconciliationGuard::check(expected, &PositionReport)` compares
  the runtime's expected signed position against the venue's authoritative report. `delta = |expected − venue|`;
  within tolerance → `Reconciled`; beyond → an alarm (`alarms += 1`) and, under `AlarmAction::Halt`, trips the
  QE-216 `KillHandle` (out-of-band flatten-and-halt). `AlarmOnly` alarms without halting.
- **Sign-aware** (a flip sums magnitudes), **latching** (QE-009 kill), **fails safe** (negative tolerance
  clamped to 0). Detector only — attribution is QE-302; takes `expected` explicitly (no circular self-check).
- **Scrutinise:** (1) tolerance is **absolute contracts** — right unit, or should it be fractional of the
  expected magnitude? (2) `expected` is caller-supplied (not the keeper the report updated) to avoid a circular
  check — is that the right seam, or should the guard own both sides? (3) sign-flip summing magnitudes — the
  correct severity ordering? (4) latching means a single halt persists across periods while alarms keep
  counting — acceptable? (5) is a stateless-per-check guard (no divergence-streak / hysteresis) sufficient for
  "periodically compare", or is a consecutive-breach threshold needed to avoid flapping?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 571 passed / 1 ignored / 57 suites (+8 reconciliation tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok
