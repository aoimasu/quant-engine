# QE-211 — Bootstrap→live in-process cutover — design note

`Phase: P2` · `Area: ③→④` · `Depends on: QE-210, QE-207` · `Branch: qe-211/bootstrap-live-cutover`

## Goal (from backlog)

The handoff from replay to live is **in-process** — bootstrap and live share state.

- Switch the evaluator session in place from replay to wss continuation at cutover; **no state copy**, no
  gap/overlap in evaluated bars.

**Acceptance criteria.**
- [ ] Cutover produces **no duplicated or skipped bar**; post-cutover decisions match a
  **continuously-running reference** on a fixture.

**Out of scope.** Netting (QE-213); the concrete wss bar decode/drive (runtime plumbing — this operates on
already-decoded base `Bar`s, per QE-205).

## Current-state evidence & placement

- QE-207 built the shared `EvaluatorSession`: `on_bar(&Bar) -> EvalOutput` and `go_live()` — a **one-way,
  state-preserving** flip of the mode *label* only (positions, factor-join warm-up carry over untouched),
  so the decision stream is continuous across the boundary.
- QE-209 produces a `Reconstructed { session, decisions, .. }`; the last replayed base bar's open time is
  `decisions.last().time_ms`. QE-205's live-kline stitch already established the pattern for the
  bootstrap→live seam: a **monotonic open-time dedup** — a bar whose `open_time <= last` is already covered
  (dropped), a strictly-greater contiguous bar is accepted.
- So QE-211 is **not** new evaluation logic. It is the thin **cutover coordinator** that owns the warmed
  session and, driving live base bars into it: (1) **drops** overlap bars the replay already covered (no
  duplicate), (2) **detects a gap** if the next bar skips ahead (no silent skipped bar), (3) flips the
  session to live **in place** (no new object, no state copy) on the first genuinely-new bar.
- **Placement: new `crates/runtime/src/cutover.rs`**, exported from `lib.rs`. Uses `qe_domain::{Bar,
  Resolution}` and the QE-209 `Reconstructed` / QE-207 `EvaluatorSession` — all already in `qe-runtime`. No
  new dependency, no cross-crate edge → QE-132 firewall unaffected.

## Design

### D1 — `CutoverStep` / `CutoverError`

```
pub enum CutoverStep { Duplicate, Evaluated(EvalOutput) }
pub enum CutoverError { EmptyReplay, Gap { expected_open_ms, got_open_ms } }
```

`Duplicate` = a live bar already covered by the replay window (`open <= last`), **dropped** — never
re-evaluated (re-evaluating would double-advance positions). `Gap` = the next bar skips past the expected
contiguous open time (a missed bar) — surfaced, not silently accepted. `EmptyReplay` = no replay bar to
anchor the boundary.

### D2 — `Cutover`

Owns the warmed session, the last evaluated base open time, and the base interval:

- `from_reconstructed(reconstructed: Reconstructed, base: Resolution) -> Result<Self, CutoverError>` —
  **moves** the session out of `Reconstructed` (no copy), anchoring `last_open_ms =
  decisions.last().time_ms` (`EmptyReplay` if none). `interval_ms = base.minutes() * 60_000`.
- `new(session, last_open_ms, base)` — lower-level constructor (used by tests / callers that already hold a
  session).
- `feed_live_bar(&mut self, bar: &Bar) -> Result<CutoverStep, CutoverError>`:
  - `open <= last_open_ms` → `Duplicate` (drop; state untouched).
  - `open == last_open_ms + interval_ms` → **contiguous**: on the *first* such bar, `go_live()` **in place**
    (the cutover — one-way, no state copy); then `on_bar(bar)`; advance `last_open_ms`; return
    `Evaluated(out)`.
  - `open > last_open_ms + interval_ms` → `Gap { expected, got }`.
- Forwards the session's scalar observations (`observe_funding` / `observe_open_interest` /
  `observe_premium`) so the live driver feeds as-of context without reaching around the cutover; `mode()`,
  `is_live()`, `last_open_ms()`, and `into_session()` expose state.

**Why lazy `go_live` on the first new bar:** nothing is evaluated between the last replay bar and the first
live bar, and `go_live` only flips a label, so flipping exactly when the first live bar is evaluated yields
a decision stream identical to a session that ran continuously and flipped at the same bar (the AC's
reference). Overlap duplicates arriving first are dropped without flipping — correct, since they aren't
evaluated.

## Test plan (deterministic; reuses the QE-209 vintage/bar fixtures)

1. `cutover_matches_continuous_reference_with_no_dup_or_gap` (**the AC**) — build one vintage; a **reference**
   session fed bars `0..N` continuously with `go_live()` at bar `K`; a **cutover** whose session replayed
   bars `0..K`, then fed live bars that **re-deliver** the overlap (`K-2, K-1` → `Duplicate`) and then
   `K..N` (→ `Evaluated`). Assert: the overlap bars are `Duplicate` (no re-eval), and the cutover's
   `Evaluated` outputs (incl. `mode == Live` and `time_ms`) **equal** the reference's `outputs[K..N]`
   bar-for-bar — no duplicated or skipped bar, decisions match the continuous reference.
2. `duplicate_bar_does_not_advance_state` — feeding a bar with `open <= last` returns `Duplicate` and leaves
   `last_open_ms` / the session decisions unchanged.
3. `gap_bar_is_reported` — a bar skipping past `last + interval` yields `CutoverError::Gap { expected, got }`.
4. `first_live_bar_flips_mode_in_place` — before the first `Evaluated` the session is `Replay`; after, it is
   `Live` (same session object — `go_live` in place, no new object).
5. `empty_replay_is_rejected` — `from_reconstructed` with no replay decisions → `CutoverError::EmptyReplay`.

## Risks

- **In-process handoff assumes the same base resolution on both sides.** The caller passes the bootstrap's
  base `Resolution`; a mismatch would mis-size the contiguity step. Documented; the live driver uses the
  same config as the bootstrap.
- **Non-bar events (funding/OI/premium/mark).** Bars drive decisions and the AC; scalar context is forwarded
  to the session; mark handling is QE-208's separate loop. No decision-affecting event bypasses the session.
