# QE-133 — Validation reporting — design note

`Phase: P1` · `Area: ⑤/⑥/⑧ support` · `Depends on: QE-131, QE-125, QE-128, QE-109`
`Branch: qe-133/validation-reporting`

## Goal (from backlog)

A human-readable, per-vintage **evidence pack** for the G1 decision.

- Report: net-of-cost performance, cost-sensitivity (1×/2×) sweep, DSR/PBO/SPA, per-regime expectancy,
  pairwise return-correlation distribution, capacity at target AUM, worst-case loss.

**Acceptance criteria.**
- [ ] A single per-vintage report contains all the above and is reproducible.

**Out of scope.** Interactive viewer (QE-136).

## Current-state evidence & placement

- The pieces already exist as computed artefacts: DSR/PBO/SPA = `qe_validation::RobustnessReport`
  (QE-131); per-regime expectancy (QE-125/127), pairwise correlation (QE-115 `pearson`), capacity
  (QE-128), worst-case loss (QE-130) live in `qe-signal`/`qe-ensemble`; cost-sensitivity comes from the
  friction model (QE-109). QE-133 **aggregates and renders** them — it does not recompute them.
- **A new crate `qe-report`** (Area ⑧ diagnostics). It is the most-downstream consumer (an evidence pack
  over finished outcomes), so it is unconstrained by the firewall (QE-132 constrains `qe-wfo`/`qe-ensemble`
  as upstreams only; a downstream reporter may read anything). It embeds `qe_validation::RobustnessReport`
  directly (the DSR/PBO/SPA section *is* that type — its primary dependency, QE-131) and represents the
  remaining sections as **plain serde rows** populated by the caller, so the report stays a stable
  presentation layer that does not couple to every upstream type or re-run any backtest.

## Design

### D1 — The report data model (serde)

`VintageReport` bundles, for one vintage:
- `vintage_id`, `content_hash` — provenance (plain strings; ties the pack to a specific vintage artefact,
  QE-129, without a `qe-vintage` dep).
- `performance: PerformanceSummary { total_return, mean_return, sharpe, max_drawdown, n_periods }` —
  net-of-cost.
- `cost_sensitivity: Vec<CostScenario { cost_multiple, sharpe, total_return }>` — the 1×/2× sweep (a
  strategy whose edge evaporates at 2× cost is fragile).
- `robustness: qe_validation::RobustnessReport` — DSR/PBO/SPA + observed Sharpe + effective trials.
- `regime_expectancy: Vec<RegimeRow { regime, count, mean_return, win_rate }>` — per-regime (QE-125).
- `correlation: CorrelationSummary { min, median, max, mean }` — the pairwise return-correlation
  distribution across ensemble members (QE-115).
- `capacity: CapacitySummary { target_aum, capacity, weight_cap }` — capacity at target AUM (QE-128).
- `worst_case_loss: f64`, `binding_scenario: String` — the stress figure (QE-130).

Every field is `serde`, so the whole report round-trips (reproducibility evidence) and can be persisted
alongside the vintage.

### D2 — Deterministic rendering

`render_markdown(&self) -> String` produces the human-readable evidence pack: a titled section per item,
fixed field order, fixed numeric formatting (so the bytes are a pure function of the inputs). No clock, no
RNG, no map iteration — same `VintageReport` ⇒ identical markdown.

### D3 — Reproducibility

Two independent guarantees: (a) `serde` round-trip identity (`report == from_json(to_json(report))`); (b)
`render_markdown` is a pure function (two renders are byte-identical). Together with the embedded
`content_hash` (pinning the report to a vintage), the pack is reproducible — the AC's requirement.

## Module / API plan

New crate `crates/report` (`qe-report`), `[workspace.dependencies]`-registered:
- `VintageReport` + section structs (`PerformanceSummary`, `CostScenario`, `RegimeRow`,
  `CorrelationSummary`, `CapacitySummary`) — all `serde`.
- `VintageReport::render_markdown(&self) -> String`.
- `pairwise_correlation_summary(series: &[Vec<f64>]) -> CorrelationSummary` — a convenience that computes
  the min/median/max/mean of all pairwise correlations (the one bit of computation the report owns, so the
  caller need not pre-summarise; uses a local Pearson to avoid a `qe-ensemble` dep).
- Deps: `qe-validation` (RobustnessReport), `serde`; dev: `serde_json`.

## Test plan (TDD)

1. **All sections present (AC).** A populated `VintageReport::render_markdown()` contains every required
   section: net-of-cost performance, the 1×/2× cost sweep, DSR/PBO/SPA, per-regime expectancy, the
   correlation distribution, capacity at target AUM, and worst-case loss.
2. **Reproducible.** `render_markdown` is byte-identical across two calls; the report round-trips through
   serde (`from_json(to_json(r)) == r`).
3. **`pairwise_correlation_summary`** — on a known set (e.g. one perfectly correlated pair + one
   anti-correlated) the min/median/max/mean match a hand computation; `<2` series ⇒ a zeroed summary.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-report`,
`cargo test --workspace`, `cargo deny check`.

## Risks

- **Caller-populated rows.** The report trusts the caller (the future orchestration, QE-209) to extract
  each section from the real upstream artefacts; the regime/capacity/cost rows are plain data, not the
  upstream types, so a wiring error would mis-populate rather than fail to compile. Mitigated by keeping
  the embedded `RobustnessReport` as the real type (the central QE-131 dependency) and by the
  reproducibility/round-trip tests; the orchestration ticket wires and tests the extraction.
- **Markdown only.** A human-readable text pack (the AC); an interactive viewer is explicitly QE-136. The
  serde form is the machine-readable counterpart for any later renderer.
- **Correlation owned here.** `pairwise_correlation_summary` re-implements Pearson locally (a few lines)
  rather than depending on `qe-ensemble::pearson`, keeping the report dep-light and firewall-trivial.
