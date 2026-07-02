# QE-210 — Reconstructed state — design note

`Phase: P2` · `Area: ③ Bootstrap` · `Depends on: QE-209` · `Branch: qe-210/reconstructed-state`

## Goal (from backlog)

Bootstrap output: per-strategy positions, dormancy latches, and **committed peak equity** — the last is
load-bearing for drawdown breakers.

- Produce per-strategy positions, dormancy latches, committed peak equity; the peak must be the **true**
  committed peak, not a windowed peak (else breakers are mis-anchored).

**Acceptance criteria.**
- [ ] Reconstructed committed peak equals the **true historical peak** on a fixture **longer than the
  bootstrap window**.

**Out of scope.** Restart parity test (QE-220); cutover (QE-211); the breaker itself (QE-212). The
net-of-cost live equity feed is a QE-212/QE-217 concern — see D3.

## Current-state evidence & placement

- QE-209 (`crates/runtime/src/bootstrap.rs`) already lands `BootstrapPipeline::cold_start` → a
  `Reconstructed` carrying the `EvaluatorSession`, the per-bar `decisions: Vec<EvalOutput>`, `coarse_bars`,
  `bars_replayed`, and `last_mark_price`. QE-209's design **explicitly deferred the reconstructed-state
  object to QE-210** ("surfaced for QE-210/persistence").
- The `EvaluatorSession` (`evaluator.rs`) holds `positions: Vec<PositionState>` — one per chromosome —
  advanced by the shared `decide`/`advance` pair (train/live parity). Each `EvalOutput` carries the
  per-chromosome `ChromosomeDecision { index, decision }`.
- The breaker (QE-116, `crates/risk/src/breaker.rs`) anchors **total drawdown** on an **all-time** equity
  `peak` (`self.peak = p.max(equity)`), while only the *fast* tier uses a rolling window. So "committed
  peak equity, true not windowed" == the all-time peak that seeds that drawdown anchor at live start; a
  windowed peak would under-anchor the drawdown and mis-fire the breaker (the reviewer's concern).
- **Placement: new `crates/runtime/src/boot_state.rs`**, exported from `lib.rs`. No new dependency (uses
  `qe_signal::PositionState`, `rust_decimal`, and the QE-209 `Reconstructed`), no new cross-crate edge →
  QE-132 firewall unaffected.

## Design

### D1 — `CommittedPeak` — the true (all-time) running-max accumulator

```
pub struct CommittedPeak { peak: Option<Decimal> }
impl CommittedPeak { fn observe(&mut self, equity: Decimal); fn peak(&self) -> Option<Decimal>;
                     fn from_series(series: &[Decimal]) -> Self }
```

The **entire** equity path folds into a monotone running maximum — never a trailing window. This is the
load-bearing anti-mis-anchoring primitive and the crux of the AC: over a fixture longer than any window,
with the peak occurring early and equity declining after, `peak()` still returns the early true peak,
whereas a trailing-window max would lose it.

### D2 — `DormancyLatch` — per-strategy dormancy

```
pub struct DormancyLatch { dormant: bool }
impl DormancyLatch { fn active() -> Self; fn dormant() -> Self; fn is_dormant(&self) -> bool;
                     fn activate(&mut self) }
```

**Cold-start semantic (reconstruction):** a strategy is reconstructed **dormant** iff it never held a
position across the replay (emitted no `Enter`) — it has made no committed exposure, so the live planner
resumes it dormant (breaker anchored at seed) until it fires. Documented boundary: QE-212's breaker layer
may *additionally* latch dormancy on a gate/trip; QE-210 provides the type + the cold-start derivation, not
the breaker-driven latching.

### D3 — `ReconstructedState` / `StrategyState`

```
pub struct StrategyState { pub index: usize, pub position: PositionState,
                           pub dormancy: DormancyLatch, pub committed_peak_equity: Option<Decimal> }
pub struct ReconstructedState { pub strategies: Vec<StrategyState> }
```

Builder `ReconstructedState::from_replay(positions: &[PositionState], decisions: &[EvalOutput],
equity_paths: &[Vec<Decimal>])` — takes the replay outputs as **decomposed borrows** (rather than a whole
`&Reconstructed`) so it is decoupled from the QE-209 `Reconstructed`/`EvaluatorSession` and directly
unit-testable; the caller (QE-211) destructures `Reconstructed` itself, e.g.
`from_replay(recon.session.positions(), &recon.decisions, &equity_paths)`:
- **positions** ← the session's final `positions()` (new read-only accessor on `EvaluatorSession`),
- **dormancy** ← per chromosome, `dormant` iff no `EvalOutput` in `decisions` carries an `Enter` for that
  index,
- **committed_peak_equity** ← `CommittedPeak::from_series(equity_paths[i]).peak()`.

**Equity-path boundary (documented scope decision).** The per-strategy equity *series* is an **input** to
the reconstruction, not computed here. Rationale: a faithful live equity curve is **net-of-cost** (fees +
funding, QE-109) marked against real fills — that overlay and the live equity feed belong to QE-212/QE-217,
and recomputing a gross mark-to-market here would both duplicate the QE-120 backtester and risk train/live
drift. QE-210's job is the **state model + true-peak correctness** given the equity path; positions and
dormancy are wired to the real replay output. The AC (true vs windowed peak) is fully proven at this layer.

## Test plan (deterministic)

1. `committed_peak_is_true_all_time_not_windowed` (**the AC**) — an equity series longer than any trailing
   window whose maximum is **early**, then declines; `CommittedPeak::from_series` returns the early true
   peak, and it is shown to differ from a trailing-window max over the same series (proves no windowing).
2. `committed_peak_observe_matches_from_series` — incremental `observe` equals `from_series`.
3. `dormancy_latch_basics` — `activate` clears dormant; `is_dormant` reflects state.
4. `from_replay_assembles_positions_dormancy_and_peak` — build a small `Reconstructed` (2 chromosomes, one
   that entered during replay, one that never did) + equity paths; assert positions match the session, the
   never-traded strategy is dormant while the traded one is active, and committed peaks equal the true
   per-strategy maxima.
5. `from_replay_rejects_mismatched_equity_paths` — a wrong number of equity paths is a clear error, not a
   panic/silent truncation.

## Risks

- **Equity series is an input (D3).** Deliberate scope boundary, documented; the AC is met at the peak
  layer. If a reviewer wants gross mark-to-market wired here, that is a larger, parity-sensitive change best
  taken with QE-212's net-of-cost equity feed.
- **Dormancy semantic.** "Never-traded ⇒ dormant" is a conservative, replay-derivable cold-start rule;
  breaker-driven dormancy is QE-212. Documented so the boundary is explicit.
