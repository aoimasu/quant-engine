# QE-124 — Elite robustness gates — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-120`
`Branch: qe-124/elite-robustness-gates`

## Goal (from backlog)

*(Reviewer-added.)* Evolution overfits efficiently; elites must survive perturbation and re-evaluation
to be trusted.

- Reject/flag elites failing: **minimum-trade-count**, **parameter-perturbation robustness** (survive ±ε
  genome jitter), **descriptor-stability-under-reevaluation** (QE-111 metric).

**Acceptance criteria.**
- [ ] An elite that collapses under ±ε jitter or has unstable descriptors is rejected/flagged.

**Out of scope.** Statistical deflation (QE-131).

## Current-state evidence

This is an **overfitting defence** placed between the search and the strategy repository (QE-123). It
composes three merged pieces:
- **QE-120** (`qe_wfo::backtest`) — the fitness engine. `backtest(genome, bars, cfg)` returns
  `trades` and a noise-robust `fitness.mean` (`−∞` on ruin / below the min-trade gate). The gate
  re-evaluates the elite and a cloud of jittered neighbours through it.
- **QE-110** (`qe_wfo::genome`) — `Genome::repair(schema)` deterministically clamps any perturbed gene
  back onto the validity manifold, so a ±ε jitter always yields a *valid* genome to re-evaluate.
- **QE-111** (`qe_wfo::archive`) — `descriptor_for(genome, dir, schema)` is the genotype→`Cell`
  descriptor and `cell_reassignment_rate(a, b)` with `STABILITY_THRESHOLD = 0.05` is the stability
  metric. Descriptors are window-invariant, so the meaningful "re-evaluation" perturbation is the **±ε
  jitter**: an elite sitting on a band boundary (e.g. `max_holding_bars = SCALP_MAX_BARS = 6`) flips its
  `HoldingBand` under a 1-bar nudge — an *unstable descriptor*.
- **QE-006** (`qe_determinism`) — `task_rng(seed, i)` seeds each jitter sample independently, so the whole
  assessment is byte-deterministic and scheduling-independent.

## Design

### D1 — The three gates

`assess_robustness(elite, bars, schema, backtest_cfg, cfg, seed) -> RobustnessReport`:

1. **Minimum-trade-count.** Backtest the elite; `min_trades_ok = base_trades ≥ cfg.min_trades`. An elite
   that barely trades has too little evidence to trust.
2. **Parameter-perturbation robustness.** Draw `cfg.samples` jittered genomes (each gene nudged within
   ±ε via `task_rng(seed, i)`, then `repair`-ed) and backtest each. A sample **collapses** if its fitness
   is non-finite (jitter tips it into ruin / below its own trade floor) or retains less than
   `cfg.retain_fraction` of the elite's fitness. `perturbation_ok = collapsed_fraction ≤
   cfg.max_collapse_fraction`. An over-fit elite perched on a fitness spike collapses; a genuinely robust
   one degrades gracefully.
3. **Descriptor stability.** For each direction the elite occupies (`descriptor_for` is `Some`), assign
   the elite's `Cell` and each jitter's `Cell`, then `cell_reassignment_rate`. `descriptor_ok = rate ≤
   cfg.max_descriptor_reassignment` (default `STABILITY_THRESHOLD`). A boundary-sitting elite whose niche
   flips under ±ε is unstable.

`RobustnessReport::passed()` = all three ok; `flagged()` = `!passed()`; `reasons()` lists the failed
gates (`RejectReason::{MinTrades, PerturbationCollapse, UnstableDescriptor}`) — the gate *flags*, the
caller (QE-123 recorder / QE-125 promotion) decides reject-vs-quarantine. That satisfies the AC's
"rejected/flagged" without coupling to a specific downstream policy.

### D2 — The ±ε jitter

`jitter(base, cfg, schema, rng)` nudges the **continuous/ordinal** genes — each enabled clause's `lo`/`hi`
state bounds (±`eps_state`), `exit.max_holding_bars` (±`eps_holding`), `risk.size_bps` (±`eps_size_bps`)
— then `repair`s. It deliberately does **not** toggle `enabled` or change which feature a clause reads:
those are *structural* genes (they define the family/timescale niche), and the point is to test
sensitivity to small *parameter* moves, the way overfitting manifests. Holding is the one ordinal gene
that can cross a behavioural band, which is exactly what the descriptor-stability gate probes.

### D3 — Determinism

Sample `i` is driven by `task_rng(seed, i)` (QE-006/QE-118 convention), so the report is identical across
runs and independent of evaluation order. No floating-point reduction order matters — each sample is an
independent backtest.

## Module / API plan

New module `crates/wfo/src/robustness.rs`, re-exported:

- `RobustnessConfig { samples, eps_state, eps_holding, eps_size_bps, min_trades, retain_fraction, max_collapse_fraction, max_descriptor_reassignment }` (+`Default`/`with_defaults`).
- `RobustnessReport { base_fitness, base_trades, samples, collapsed, collapsed_fraction, descriptor_reassignment, min_trades_ok, perturbation_ok, descriptor_ok }` + `passed`/`flagged`/`reasons`.
- `RejectReason { MinTrades, PerturbationCollapse, UnstableDescriptor }`.
- `assess_robustness(...)`, `jitter(...)`.
- Consts `DEFAULT_ROBUSTNESS_SAMPLES`, `DEFAULT_EPS_STATE`, `DEFAULT_EPS_HOLDING`, `DEFAULT_EPS_SIZE_BPS`, `DEFAULT_RETAIN_FRACTION`, `DEFAULT_MAX_COLLAPSE_FRACTION`, `DEFAULT_MAX_DESCRIPTOR_REASSIGNMENT`.
- No new deps (`qe-determinism` already a normal dep).

## Test plan (TDD)

1. **Fragile elite collapses (AC).** An over-fit razor-thin-band elite that profits on a zig-zag series
   only at one feature state; widening the band under ±ε jitter adds systematically losing trades, so the
   jitter cloud collapses → `perturbation_ok = false`, `flagged()`, `reasons()` contains
   `PerturbationCollapse`.
2. **Unstable descriptor flagged (AC).** A fitness-robust elite sitting exactly on the `Scalp/Swing`
   holding boundary (`max_holding_bars = 6`) flips `HoldingBand` under the holding jitter →
   `descriptor_ok = false`, `reasons()` contains `UnstableDescriptor`.
3. **Robust elite passes.** A wide-band elite deep inside a holding band on a clean uptrend survives the
   jitter cloud (fitness graceful, niche stable, enough trades) → `passed()`.
4. **Min-trade gate.** A rarely-firing elite is flagged with `MinTrades`.
5. **Determinism.** Two `assess_robustness` calls with the same seed produce identical reports.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Fixture sensitivity.** The fragile/robust distinction is demonstrated on synthetic bars; the
  thresholds (`retain_fraction`, `max_collapse_fraction`) are config-ready constants tuned so the
  mechanism (over-fit spike collapses, robust plateau survives) is the thing under test, not a magic
  number. Real calibration is a downstream concern.
- **Jitter scope.** Structural genes are intentionally not jittered — perturbing which feature a clause
  reads would change the *family* niche and conflate descriptor stability with a different search move.
  Parameter jitter is the overfitting-relevant axis.
- **Flag, don't delete.** The gate returns a report; wiring it into the recorder/promotion path (auto-
  reject vs quarantine) is QE-123/QE-125's call, kept out of this unit so the policy stays where the
  lifecycle lives.
