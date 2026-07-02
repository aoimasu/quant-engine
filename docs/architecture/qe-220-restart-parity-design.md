# QE-220 — Bootstrap/restart parity test — design note

`Phase: P2` · `Area: ③ risk` · `Depends on: QE-210, QE-211` · `Branch: qe-220/restart-parity`

## Goal (from backlog)

*(Reviewer-added.)* If reconstructed peak/state diverges from continuous state, **every drawdown breaker is
mis-anchored** — a capital-risk event, not just a feature.

- **Scope.** A test asserting **bootstrap-reconstructed** state equals **continuously-running** state on the
  breaker-relevant fields (committed peak, dormancy latches, positions).
- **Out of scope.** Breaker logic (QE-212).

**Acceptance criteria.**
- [ ] Reconstructed vs continuous state match **bit-for-bit** on breaker-relevant fields.

`Spec ref: Runtime — stateless critical path / reconstruct on restart; reviewer: restart parity.`

## Current-state evidence & placement

- QE-210 (`crates/runtime/src/boot_state.rs`): `ReconstructedState::from_replay(positions, decisions,
  equity_paths)` — the **cold-restart** derivation. Per strategy it folds `committed_peak_equity` via
  `CommittedPeak::from_series` (the **true all-time** max, *not* a trailing window — the module explicitly
  warns a trailing window would lose an early peak), sets `dormancy` from whether the decision trace ever held
  a `Decision::Enter`, and carries the session's final `PositionState`. `StrategyState` /
  `ReconstructedState` derive `PartialEq`/`Eq` → bit-for-bit comparable.
- QE-207 (`EvaluatorSession`): `on_bar(bar) -> EvalOutput`, `positions() -> &[PositionState]` — the shared
  replay/live evaluator. A genome whose entry clause spans the full feature range fires every bar (trades); an
  all-clauses-off genome never fires (stays dormant) — the two branches this test needs.
- **Placement: new integration test `crates/runtime/tests/restart_parity.rs`.** The parity property is a
  cross-cutting, black-box invariant over the public API (`EvaluatorSession`, `ReconstructedState::from_replay`,
  `CommittedPeak`, `DormancyLatch`, `StrategyState`, `PositionState`). An integration test is its natural home
  and adds **no production surface** — the AC asks only for a test. No new dependency; firewall unaffected.

## Design

### D1 — the two derivations under test

- **Reconstructed (cold restart):** `ReconstructedState::from_replay(final_positions, &all_outputs,
  &equity_paths)` — the production restart path.
- **Continuous (live-accumulated), computed with an *independent* reference implementation** so agreement is a
  real parity check, not a tautology of calling the same code:
  - `committed_peak_equity[i]` = a plain running-max fold over `equity_paths[i]` (`acc = acc.map_or(e, |a|
    a.max(e))`), tick by tick — what a live breaker accumulates via `observe` as equity streams.
  - `dormancy[i]` = a plain boolean that latches `entered` on the first `Decision::Enter` seen for strategy
    `i` across the streamed `EvalOutput`s → `active` iff ever entered, else `dormant`.
  - `position[i]` = the session's final `positions()[i]` (the live position at the notional restart instant —
    the same single source both paths read).
  - assembled into a `ReconstructedState` of the same shape.

If `from_replay` ever regressed to an order-dependent or windowed peak (the exact capital-risk bug the ticket
guards), the independent true-max reference would diverge and the test would fail.

### D2 — the scenario (non-vacuous on every breaker-relevant field)

- **Two strategies:** index 0 a `cycling_genome` (entry clause spans `[0, num_states-1]` → fires → **trades**,
  ending non-flat), index 1 an **all-off** genome (never fires → **dormant**). So the dormancy field exercises
  *both* branches and positions differ from flat.
- **Equity paths that rise then fall** (peak in the interior, e.g. `[100, 150, 120]`), so the committed peak is
  an **early** value the final sample is below — a trailing-window max would get this wrong. The test asserts
  the peak equals the true interior max, not the declining tail.
- Feed a deterministic bar series (the QE-209 fixture shape) to one `EvaluatorSession`, collect the
  `EvalOutput` trace + final positions, and use the same synthetic per-strategy equity paths for both
  derivations.

### D3 — assertions

1. **`assert_eq!(continuous, reconstructed)`** — the headline AC: bit-for-bit on every `StrategyState`
   (`index`, `position`, `dormancy`, `committed_peak_equity`).
2. **Committed peak is the true all-time max** — `reconstructed.strategies[0].committed_peak_equity ==
   Some(true_interior_max)` and strictly greater than the final equity sample (guards the trailing-window
   regression directly).
3. **Dormancy is non-trivial** — strategy 0 `active` (it traded), strategy 1 `dormant` (it never did); and
   strategy 0's final position is non-flat (it really entered), so the trace is genuine.
4. **Equity-path/position count mismatch is rejected** — `from_replay` with a wrong `equity_paths.len()`
   returns `BootStateError::MismatchedEquityPaths` (guards the reconstruction's own precondition).

## Test plan (deterministic)

`crates/runtime/tests/restart_parity.rs`:
1. `reconstructed_state_matches_continuous_bit_for_bit` (**AC**) — the full parity assertion (D3.1) over a real
   session with a trading + a dormant strategy and rise-then-fall equity paths.
2. `committed_peak_is_true_all_time_max_not_trailing` — D3.2, the capital-risk guard: an early peak survives a
   later decline in both derivations.
3. `dormancy_latches_match_for_traded_and_untraded` — D3.3, both dormancy branches agree.
4. `mismatched_equity_paths_are_rejected` — D3.4.

## Gates

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo test -p qe-runtime --test restart_parity`, `cargo test --workspace --locked`,
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Independent reference must stay independent.** The continuous side deliberately does *not* call
  `CommittedPeak`/`from_replay`; it hand-rolls the true max + entered-flag, so the two sides can disagree if the
  production path regresses. If a future refactor makes the reference delegate to the production code, the
  parity becomes vacuous — documented so it is not "simplified" away.
- **Determinism.** One `EvaluatorSession` over a fixed fixture bar series; synthetic equity paths are literals.
  No clocks, RNG, or I/O — identical in replay/live.
- **Positions share a single source.** Both derivations read the session's final `positions()`, so that field
  matches by construction; it is still asserted (and the trading strategy is asserted non-flat) so the field is
  not vacuously equal on two flats.
- **Firewall / deps.** Test-only, over existing public APIs; no new crate edge. QE-132 guard stays green.
