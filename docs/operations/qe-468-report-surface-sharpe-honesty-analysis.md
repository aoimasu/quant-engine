# QE-468 — Report-surface Sharpe honesty: current-state evidence, decisions, test plan, risks

> **Ticket:** `docs/mds/tickets/QE-468.md` (R3 — AFML panel #1). **Depends on:** QE-439 (DSR/PSR),
> QE-467 (persisted seal evidence). **Scope:** *reporting only* — no change to selection/gating.

## 1. Problem (from the spec)

The engine keeps two Sharpe numbers with unequal rigour. The **selection-path** Sharpe (DSR/PSR/PBO in
`crates/validation`) is per-period, non-normality- and multiple-testing-aware. The **reported headline**
Sharpe a human reads is not: `metrics::sharpe` annualises a *per-bar* Sharpe by a naïve `√ppy`
(`crates/cli/src/jobs/metrics.rs:68,78` — `mean / var.sqrt() * periods_per_year.sqrt()`), assembled at
`crates/cli/src/jobs/backtest.rs:262` with `ppy = periods_per_year(resolution)` (`backtest.rs:257,366`).
At `H1`, `ppy = 8760 ⇒ √ppy ≈ 93.6`. `√t` scaling assumes IID returns; crypto perp returns are
autocorrelated / vol-clustered, so `√ppy` **overstates** the annualised figure, and the reported number
carries **no** PSR haircut and **no** trial-count `N` beside it — even though
`probabilistic_sharpe_ratio` (`crates/validation/src/dsr.rs:39`) and the deflation basis already exist
and are persisted at seal time (QE-467, `crates/vintage/src/lib.rs:62` `SealEvidence`).

## 2. Current-state evidence (file:line)

- **Headline metric assembly** — `crates/cli/src/jobs/backtest.rs:255-267` (`assemble_doc`): builds
  `Metrics` from `metrics::sharpe(ensemble, ppy)` / `metrics::sortino(ensemble, ppy)` on the **per-bar**
  `ensemble` series. `ppy = periods_per_year(resolution)` (`backtest.rs:257,366`). `times` (aligned to
  `ensemble`, i.e. `decision_bars[1..]` open times) is already computed at `backtest.rs:253` for monthly
  bucketing — reused here for daily bucketing.
- **Per-period primitives** — `crates/cli/src/jobs/metrics.rs:68` (`sharpe`), `:84` (`sortino`),
  `:106` (`monthly_returns`, the existing bucket-and-compound pattern I mirror for daily),
  `:200` (`sharpe_zero_variance_is_zero_not_nan`, the degenerate-case test I mirror).
- **PSR** — `crates/validation/src/dsr.rs:39` `probabilistic_sharpe_ratio(returns, sr_benchmark)`,
  re-exported at `crates/validation/src/lib.rs:56-58`. `qe-validation` is already a `qe-cli` dep
  (`crates/cli/Cargo.toml:38`).
- **Persisted seal evidence** — `crates/vintage/src/lib.rs:62` `SealEvidence { dsr, pbo, spa_pvalue,
  n_trials, … }`, carried on `VintageContent::seal_evidence` (`:303`). The backtest job already loads &
  verifies the vintage (`backtest.rs:103-104`), so `vintage.content.seal_evidence` is in hand — **read
  by handle, no recompute**.
- **Report contract** — `crates/cli/src/jobs/result.rs:80` `Metrics` (the six headline fields the
  `result.json` / SPA read). `Metrics` is emitted by the CLI and served verbatim to the SPA; there is
  **no** separate stdout metrics table in `main.rs` (the CLI backtest output *is* `result.json`).
- **SPA surface** — `web/src/app/backtest/BacktestResult.tsx:288-308` (the 6-metric strip) and the
  `BacktestResult` contract type `web/src/api/runs.ts:335-342`.
- **Golden determinism lock** — `crates/cli/tests/backtest_job.rs:246` asserts the result is
  byte-identical to `tests/fixtures/golden_result.json`. Regenerated via the `#[ignore]`d
  `regenerate_fixtures` (`backtest_job.rs:301`).

## 3. Implementation decisions

1. **Daily aggregation (`metrics::daily_returns`).** New pure fn buckets per-bar returns into UTC days
   and **compounds within the day** (`Π(1+r) − 1`), *mirroring the existing `monthly_returns` /
   `equity_curve` convention* — this is the operative reading of "match the existing `equity_curve`
   return convention". For the tiny per-bar net returns the difference from a plain sum is negligible,
   and consistency with the equity/monthly surfaces is the stronger constraint. Keyed on the same
   `times` already used for monthly bucketing.
2. **Lo (2002) haircut (`metrics::sharpe_lo_annualised`).** New pure fn: per-period Sharpe `SR =
   mean/std` (sample, `n−1`), annualised by `√q / √(1 + 2·Σ_{k=1}^{q-1}(1−k/q)·ρ_k)` where `ρ_k` are the
   sample autocorrelations (`metrics::autocorrelation`) of the passed (daily) series. Lags are capped at
   `min(q−1, n−1)` (beyond `n−1` there are no overlapping pairs ⇒ `ρ_k = 0`, no contribution). Guards:
   `n < 2` or zero variance ⇒ `0.0` (never NaN); a non-positive Lo denominator (pathological strong
   negative autocorrelation) falls back to the naïve `√q` factor rather than producing NaN. The existing
   `sharpe`/`sortino` stay as the per-period primitives.
3. **Headline = daily.** In `assemble_doc`, the headline `sharpe = sharpe_lo_annualised(daily, 365)` and
   headline `sortino = sortino(daily, 365)` (Lo is a Sharpe-specific correction; Sortino keeps the plain
   `√365` daily annualisation). The per-bar `√ppy` Sharpe is retained as a **diagnostic**
   (`sharpe_per_bar`), never the headline. The per-bar `ensemble` series is untouched (DSR/selection path
   unaffected — this job does not run selection anyway).
4. **PSR beside every Sharpe.** `psr = probabilistic_sharpe_ratio(daily, 0.0)` on the same daily series.
5. **Persisted DSR/PBO/N by handle.** `dsr`, `pbo`, `n_trials` copied from
   `vintage.content.seal_evidence` — no recomputation, no deflation call in the report path.
6. **Labelling.** A `sharpe_clock: String` field (`"daily, Lo-adj"`) rides `Metrics` so every surface
   states the clock+adjustment; the SPA renders `Sharpe (daily, Lo-adj)` and a PSR·DSR·PBO·N row.
7. **`Metrics` extension is additive.** The six original keys keep their names (the SPA/`json_keys`
   contract test still passes); `sharpe`/`sortino` change *value* (now daily), and new fields
   `sharpe_clock`, `sharpe_per_bar`, `psr`, `dsr`, `pbo`, `n_trials` are added. The `golden_result.json`
   is regenerated — it is the **report** surface, not the sealed artefact. **No `VintageContent` field,
   no `VINTAGE_FORMAT_VERSION` bump, no sealed-vintage hash drift** (QE-006 determinism harness untouched).

## 4. Test plan (TDD)

`crates/cli/src/jobs/metrics.rs` unit tests:
- `sharpe_lo_zero_autocorr_matches_naive` — an autocorrelation-free (near-IID) series reproduces the
  naïve `sharpe(·, q)` `√q` factor to tolerance.
- `sharpe_lo_positive_autocorr_lowers_vs_naive` — a positively autocorrelated series yields a strictly
  lower annualised Sharpe than the naïve `√q`.
- `sharpe_lo_zero_variance_is_zero_not_nan` — mirrors `sharpe_zero_variance_is_zero_not_nan`.
- `autocorrelation_*` — lag-0 sanity (≈1 by construction is skipped; we test a known-sign lag-1) and
  out-of-range lag ⇒ 0.
- `daily_returns_buckets_and_compounds` — two bars in one UTC day compound; a bar in the next day starts
  a new bucket.

`crates/cli/tests/backtest_job.rs` regression:
- `reported_sharpe_is_below_naive_sqrt_ppy_on_autocorrelated_series` — on a synthetic autocorrelated
  per-bar series, the assembled headline Sharpe is **strictly below** the old `sharpe(ensemble, ppy)`
  `√ppy` figure (proves the inflation is removed), and PSR ∈ [0,1] is populated.
- Golden test still green after regeneration; a new assertion that `metrics.dsr/pbo/n_trials` equal the
  vintage's persisted `seal_evidence` (read-by-handle), and `sharpe_clock == "daily, Lo-adj"`.

`crates/cli/src/jobs/result.rs`: `sample_doc` updated; `round_trips_through_serde` and
`json_keys_match_the_contract_verbatim` extended to cover the new keys.

SPA (`web/`, not in the cargo gate): `api/runs.ts` `Metrics` type extended; `BacktestResult.tsx` renders
the labelled Sharpe + PSR/DSR/PBO/N; `BacktestResult.test.tsx` fixture + assertions updated.

## 5. Green gate

`cargo fmt --all --check` · `cargo clippy --workspace --all-targets --locked -- -D warnings` ·
`cargo test --workspace --locked` · `cargo deny check` · `cargo build --workspace --all-features
--locked`. Plus (best-effort, ungated) the `web` vitest suite for the SPA surface.

## 6. Risks / rollback

- **Golden drift (expected).** `golden_result.json` changes (headline Sharpe now daily + new fields).
  This is the report surface, regenerated deterministically; **no sealed-vintage hash changes**. Rollback
  = revert the branch.
- **Sparse-day windows (small-sample annualisation).** A very short window gives few daily buckets, so
  the daily per-period Sharpe is a tiny-sample estimate and its `√365` annualisation is noisy — e.g. the
  5-day committed fixture (120 hourly bars ⇒ ~5 daily points) yields a large headline `sharpe` (~32) that
  is a small-sample artifact of the fixture, not the multi-year real path the ticket targets, where daily
  aggregation *lowers* the per-bar `√ppy` figure. The determinism golden simply locks this deterministic
  value; the inflation-removal property is proven on a proper 30-day series in
  `metrics::reported_daily_lo_sharpe_below_naive_per_bar_sqrt_ppy`. With few buckets the Lo lag set is
  small (Σ over an empty lag set = 0 ⇒ the naïve `√q` factor), the correct conservative fallback.
- **Lint discipline.** No `unwrap`/`expect`/`panic!` outside `#[cfg(test)]`; guards return `0.0` instead
  of producing NaN. No money paths touched (metrics are `f64` ratios by existing contract).
