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

## QE-220 — Bootstrap/restart parity test — [Ready-for-review]

- **PR:** #71 — https://github.com/aoimasu/quant-engine/pull/71
- **Ticket:** QE-220 (`Phase: P2` · `Area: ③ risk` · `Depends on: QE-210, QE-211`)
- **Branch:** `qe-220/restart-parity`
- **Latest commit:** `6666b988ba2fd6b51d838f94bb40084d7accd8d8`
- **Evidence / design:** `docs/architecture/qe-220-restart-parity-design.md`
- **Changed files:** `crates/runtime/tests/restart_parity.rs` (new integration test), design note. (Also
  archives QE-219 → `docs/mds/reviewed/qe-219.md` + clears the prior `work.md` entry.)

### Goal
*(Reviewer-added.)* If reconstructed peak/state diverges from continuous state, every drawdown breaker is
mis-anchored — a capital-risk event. Assert bootstrap-reconstructed state equals continuously-running state on
the breaker-relevant fields (committed peak, dormancy latches, positions).

### Acceptance criteria (from backlog)
- [x] Reconstructed vs continuous state match bit-for-bit on breaker-relevant fields —
  `reconstructed_state_matches_continuous_bit_for_bit`.

### Implementation summary
- New integration test `crates/runtime/tests/restart_parity.rs` (no production surface). **Reconstructed** =
  `ReconstructedState::from_replay(final_positions, &outputs, &equity_paths)` (production restart path).
  **Continuous** = an **independent** reference (plain running max for the peak, plain entered-flag for
  dormancy, session's final positions — deliberately not calling `CommittedPeak`/`from_replay`), so agreement
  is a real parity check, not a tautology.
- Scenario: a `cycling_genome` (trades → active, non-flat) + an all-off genome (dormant) over a real
  `EvaluatorSession` (40 bars, past indicator warmup); rise-then-fall equity paths so the true peak is an early
  value the tail is below (guards the trailing-window regression the module warns about).
- **Scrutinise:** (1) is the "independent reference" genuinely independent (plain max / entered-flag), or does
  it smuggle in the production logic it should check against? (2) is 40 bars a robust warmup margin, or brittle
  if the catalogue's max lookback changes? (3) positions share one source (the session) — is asserting that
  field meaningful, and is the non-flat assertion enough to make it non-vacuous? (4) synthetic equity paths
  (length 3, decoupled from bar count) — faithful to "continuous state", or should equity derive from the
  replay? (5) integration test vs a reusable production parity primitive — right call for this AC?

### Verification (toolchain 1.96.0)
- `cargo fmt --all --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — 563 passed / 1 ignored / 57 suites (+4 restart_parity tests)
- `cargo test -p qe-architecture --test firewall` — 1 passed
- `cargo deny check` — advisories/bans/licenses/sources ok

### Feedback

_First review pass, commit `57a416ca` (2026-07-02). **What is genuinely sound** (I verified each Scrutinise
point): the "continuous" reference **is** independent — `continuous_state` hand-rolls the running max and the
entered-flag and never calls `CommittedPeak`/`from_replay`, so the peak/dormancy comparison is a real parity
check, not a tautology (Scrutinise #1). Non-vacuousness on the trading branch is properly guarded:
`dormancy_latches_match_for_traded_and_untraded` asserts strategy 0 is both `!is_dormant()` **and**
`position != flat()`, so a catalogue-warmup change that stopped the cycling genome entering within 40 bars
would make the test **fail loudly**, not pass vacuously — "it traded" is asserted, not assumed (Scrutinise
#2/#3). The shared-source position field is by-construction equal but is documented and rescued from vacuity
by that non-flat assertion. Determinism and test-only (no production/dep/firewall change) hold. One
substantive item below._

**F1 — [Blocker] The equity paths are too short to catch the trailing-window regression this test exists to
guard.** The reviewer-added rationale and `committed_peak_is_true_all_time_max_not_trailing` frame the
trailing-window peak (using a recent window instead of the all-time max) as "the exact bug this guards." But
the equity paths are length 3 with the peak in the **interior** (`[100, 150, 120]`, peak at index 1). A
trailing-window regression of window `W` only loses the peak when the peak falls **outside** the last `W`
samples — here that requires `W == 1` (i.e. "use the last equity value", equivalent to no accumulation at
all). For **any `W ≥ 2`** the window already contains index 1, so a windowed `from_replay` would compute
`150` — **identical to the plain-max reference** — and the parity assertion would still pass. A realistically
sized breaker window (the real drawdown lookback is far larger than 3) would therefore **not** be caught by
either `reconstructed_state_matches_continuous_bit_for_bit` or the dedicated guard test. So the answer to the
Scrutinise #-implied question "would a trailing-window regression make the test fail?" is: only for the
degenerate `W==1` / last-value case, **not** for a genuine windowing bug. **Fix:** make the peak precede a
declining tail **longer than any plausible breaker window** — put the true max at index 0 and add a multi-step
decline, e.g. `[150, 148, 146, …, 120]` (say 8–12 samples), for strategy 0 (and similarly strategy 1). Then a
trailing window of any size short of the full length excludes the index-0 peak and diverges from the plain-max
reference, so the guard actually bites. Update `committed_peak_is_true_all_time_max_not_trailing` to keep
asserting the peak equals the index-0 max and exceeds the tail.

_Scrutinise #4 (synthetic equity decoupled from bars): acceptable — both derivations consume the **same**
paths, so the parity of the peak-fold is validly isolated; just note it does not exercise where equity itself
comes from on restart (out of scope for this AC). #5 (integration test vs reusable primitive): an integration
test is the right home for a black-box parity invariant; agreed._

### Fix applied (commit `6666b988`)

**F1 — resolved.** Agreed — the length-3 interior-peak path only caught a `window == 1` regression. The equity
paths are now **peak-at-index-0 followed by a length-`EQUITY_LEN=20` monotonic decline** (`declining_from(150,
2)` / `declining_from(130, 2)`), a length deliberately longer than any plausible breaker drawdown window. With
the true max at index 0, a trailing window of *any* size short of the full path excludes it. Plus
`committed_peak_is_true_all_time_max_not_trailing` now **explicitly loops `window in 1..EQUITY_LEN` and asserts
each trailing-window max is strictly below the true peak (150)** — so a windowed `from_replay` regression of
any realistic size would diverge from the plain-max reference and fail parity. The peak values (150/130) are
unchanged, so the parity + dormancy assertions are otherwise identical. Design note D2/D3 updated.

**Re-verification (toolchain 1.96.0)** — `cargo fmt --all --check` clean · `cargo clippy --workspace
--all-targets --locked -- -D warnings` clean · `cargo test --workspace --locked` 563 passed / 1 ignored /
57 suites · `cargo test -p qe-architecture --test firewall` 1 passed · `cargo deny check` ok.
