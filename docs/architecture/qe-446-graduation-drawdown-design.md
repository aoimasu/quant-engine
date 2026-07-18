# QE-446 — Report strategy-level max/avg drawdown at the lifecycle graduation gate

**Ticket:** [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-446`](../reviews/2026-07-16-maxdama-panel-review.md)
· Backlog Review R2.b · Panel #17 (unanimous, P3) · Depends on QE-134, QE-114.

**Spec ref:** maxdama §5.5 (Dubno: *"maximum and average drawdown are better objectives than
Sharpe"*).

## Problem (evidence)

`log_growth` (`crates/wfo/src/fitness.rs:21`) optimises the ergodic mean `ln(1+r)`. It makes
terminal **ruin** absorbing (a single `r ≤ −1 ⇒ −∞`) but is **indifferent to intermediate
peak-to-trough drawdown at a fixed size**: a path that halves and recovers scores the same as a
smoother path to the same terminal wealth.

The graduation gate `QualityGate` (`crates/wfo/src/lifecycle.rs`) admits a candidate iff it is in
`Phase::Exploitation` and its **log-growth lower confidence bound** `mean − k_sigma·se` clears the
quality threshold (`persists`, `lifecycle.rs:156`). There is **no drawdown term**. So a
high-growth / deep-drawdown genome can graduate on growth alone — *before* the ensemble's CVaR/CDaR
tail objective (`crates/ensemble/src/objective.rs`) ever sees it. The panel judged the residual risk
small (the ensemble tail objective mostly covers it) — hence P3 — but real for a **single** graduated
strategy that is deployed standalone.

## What this change does

1. **Statistic (reporting).** A new local `max_drawdown(returns)` in `crates/wfo/src/fitness.rs`
   computes the worst peak-to-trough decline of the strategy's own equity path, as a **non-negative
   magnitude in `[0, 1]`** (`0.30` = a 30 % drawdown; ruin ⇒ `1.0`). Computed **locally in
   qe-wfo** — it does **not** import `qe-ensemble`'s `cdar` (no new `qe-wfo → qe-ensemble` edge),
   though it mirrors that helper's equity/running-peak construction.

2. **Attach to the per-strategy record.** `StrategyRecord` (`crates/wfo/src/strategy_repo.rs`, the
   QE-123 per-strategy record) gains a `max_drawdown: f64` field, populated at `try_record` time from
   the candidate's realised return path.

3. **Optional drawdown ceiling on the gate (behaviour-changing part, default OFF).** `QualityGate`
   gains `max_drawdown_ceiling: Option<f64>`, **`None` by default** (no ceiling). A builder
   `with_drawdown_ceiling(ceiling)` opts in. A new `persists_with_drawdown(fitness, max_drawdown,
   threshold)` = existing `persists(...)` **AND** `drawdown_within_ceiling(max_drawdown)`. When the
   ceiling is `None`, `drawdown_within_ceiling` is always `true`, so `persists_with_drawdown`
   degenerates to `persists` — the graduation decision is unchanged. The existing `persists` method is
   **left byte-identical** and remains ceiling-agnostic.

## Golden-safety analysis (why no golden moves)

- **The statistic is on a NON-hashed record.** `StrategyRecord` / `StrategyRepository` is only
  re-exported (`crates/wfo/src/lib.rs:107`) and exercised by its own module tests. It is **not**
  consumed by `crates/cli/src/jobs/train.rs`, is **not** a field of `VintageContent`
  (`crates/vintage/src/lib.rs:45` — the only thing the `content_hash` covers), and appears in **no
  golden fixture**. Adding a field to it therefore cannot move any `content_hash` or vintage id — the
  same pattern by which the G1 decision rides the non-hashed `TrainResultDoc` (QE-437).
- **The ceiling defaults OFF.** `QualityGate::new` / `with_defaults` set `max_drawdown_ceiling =
  None`; `train.rs` builds the graduation gate via `QualityGate::with_defaults()`, so graduation
  behaviour is byte-identical unless a ceiling is explicitly configured.
- **`persists` is untouched**, so every existing caller and test keeps its exact result.
- The one repository golden, `crates/cli/tests/fixtures/golden_result.json`, is the CLI **backtest
  report** sidecar; this change touches only `crates/wfo` (`fitness.rs`, `lifecycle.rs`,
  `strategy_repo.rs`) and never the CLI report path, so it is unaffected.

**Conclusion:** no golden / `content_hash` moves. The drawdown statistic is **not hashed** (it rides
the non-hashed per-strategy record); the ceiling is **default OFF**. `VINTAGE_FORMAT_VERSION` is **not**
bumped (nothing hashed changed).

## Money / determinism

Drawdown is a pure `f64` reduction over the already-`f64` net-return path (the same representation
`log_growth` / `cdar` use); it never feeds a hash, so no `rust_decimal` requirement applies. The
computation is a deterministic left-to-right fold — bit-reproducible and thread-count-independent.

## Composition with QE-436

QE-436's graduation-champion parsimony tie-break (`graduation_cmp` / `most_parsimonious`) is a pure
equal-robust-fitness tie-break and is left unchanged. The drawdown ceiling is an **admission**
predicate (`persists_with_drawdown`), orthogonal to the parsimony **ranking** — a candidate blocked by
the ceiling is simply not admitted; among admitted candidates the parsimony ordering is identical.

## Acceptance-criteria mapping

- *max-drawdown computed correctly, peak-to-trough* → `max_drawdown` unit tests (known equity paths,
  recovery, ruin).
- *high-growth-deep-drawdown genome flagged/blocked when a ceiling IS set* →
  `persists_with_drawdown` test: a candidate that clears the log-growth bar but whose drawdown exceeds
  the ceiling is rejected.
- *ceiling OFF = current graduation behaviour byte-identical* → test asserting
  `persists_with_drawdown(.., None-gate) == persists(..)` across cases.
- *the stat is on the per-strategy record* → `StrategyRecord.max_drawdown` populated + round-trips
  through JSONL.
