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
- **Latest commit:** `f2693ea5f3fb51abaff9ded4a29383d9089d6923`
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

### Feedback

_First review pass, commit `b4789c58` (2026-07-02). **What is correct and I verified:** the divergence math is
right — `signed_qty` (Long→+qty / Short→−qty / None→0), `delta = |expected − venue_qty|`, an **inclusive**
tolerance bound, sign-flip summing magnitudes (correct severity ordering), flat-venue-vs-expected detecting a
phantom position; all in `Decimal` (no float error). Fail-safe clamp (`tolerance.max(0)`) is the right
direction. The AC holds: beyond tolerance both alarms and (under `Halt`) trips the QE-216 kill via a held
clone — genuine out-of-band — and within tolerance is silent. Latching is honoured. Scrutinise #2 (caller
supplies `expected`, not the keeper the report fed) is the right seam — the alternative would be a circular
self-check. Scrutinise #3 (flip sums magnitudes) and #4 (latch persists, alarms keep counting) are both
correct. Determinism, encapsulation, and no-new-dep/firewall are clean; the 8 tests are non-vacuous. One
substantive design item (F1) plus one minor note (F2)._

**F1 — [Blocker / the Scrutinise #5 call] `Halt` mode needs a consecutive-breach (hysteresis) threshold; a
stateless single-check auto-halt will false-halt on routine report/fill skew.** Firm opinion: for a
safety-critical **auto-halt** driven by **eventually-consistent** venue `PositionReport`s, tripping the kill on
a *single* beyond-tolerance check is too aggressive. The concrete, likely failure mode: the guard is meant to
"periodically compare", so a timer-driven `check` will routinely fire while an order is **in flight** — the
runtime's `expected` already reflects an order the venue has not yet reported as filled, so `delta` briefly
equals the full in-flight quantity, exceeds tolerance, and **auto-halts the entire book on a benign
propagation blip**. This is not a corner case; with periodic checks during active hedging it is routine, and a
guard that halts trading during normal operation is unfit as a safety control (it gets disabled in practice,
defeating its purpose). The two mitigations present do **not** cover this: sizing `tolerance` large enough to
absorb an in-flight order would blind the detector to real desyncs of that size, and `AlarmOnly` abandons the
auto-halt the AC asks for. The standard, correct design is a **streak/debounce**: require *N* consecutive
diverged checks (or a persisted divergence across a short window) before `Halt` trips — a genuine desync
persists across periods, whereas a timing skew clears within one or two. **Required resolution — either:** (a)
add a consecutive-breach threshold gating the `Halt` trip (keep `alarms` counting from the first breach for
observability, but only trip after the run reaches the threshold; a test: two isolated single-period breaches
separated by a reconciled check do **not** halt, N-in-a-row does); **or** (b) if single-check halt is
deliberate, document the concrete operational contract that makes it safe — e.g. `check` is only invoked at
quiescent points with no in-flight orders, and both `expected` and the venue report are confirmed-state with
sub-tolerance propagation latency — so the routine false-halt above cannot occur. I acknowledge the fail-safe
counter-argument (halting is the safe direction, so err toward halt); it does not resolve F1, because a
control that fires on normal operation is not a usable safety control, and the fix (debounce) preserves the
fail-safe halt for a *sustained* divergence.

**F2 — [Nit, non-blocking] Absolute-contracts tolerance is coarse across scales.** The same absolute bound
means very different sensitivity for a 0.1-contract vs a 1000-contract expected position. Documented as a
later refinement (fractional-of-magnitude) and acceptable for this AC — noting only that it interacts with F1:
an absolute tolerance sized to damp in-flight skew for large positions would be far too loose for small ones,
which is another reason to solve the transient problem with a streak threshold rather than a wider tolerance.

### Fix applied (commit `f2693ea5`)

**F1 — resolved (agreed; correct safety critique).** Replaced `AlarmAction::Halt` with
`AlarmAction::HaltAfter { consecutive: u32 }` and added a **consecutive-breach debounce**: the guard tracks a
`streak` incremented on each beyond-tolerance check and **reset to 0 by any reconciled check**; the kill trips
only once `streak ≥ consecutive.max(1)`. So a *sustained* desync still auto-halts (fail-safe preserved), while
a one-period in-flight-order skew that clears on the next check does **not** halt the book. `alarms` still
counts every breach from the first (observability); `Divergence` gains a `consecutive` field. New regression
`transient_single_period_skew_does_not_halt` (breach → reconcile → breach never trips under `HaltAfter{2}`),
and `sustained_divergence_alarms_and_halts_after_threshold` is the AC (halts on the 2nd consecutive breach).
`AlarmAction::halt_immediately()` (`HaltAfter{1}`) restores single-check halt, documented as safe only at
quiescent points. Design note D1/D2/test-plan/Risks updated.

**F2 — acknowledged (deferred), and explicitly *not* worked around by widening tolerance.** Kept absolute
contracts (fractional-of-magnitude flagged as a later refinement). Per your note, the in-flight transient is
solved by the streak threshold, **not** a looser tolerance (which would blind the detector / be far too loose
for small positions). Documented in the design-note Risks.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 573 passed / 1 ignored /
57 suites (10 reconciliation tests) · `cargo test -p qe-architecture --test firewall` 1 passed ·
`cargo deny check` ok.
