# QE-125 — Regime labelling — design note

`Phase: P1` · `Area: ⑤/⑥ support` · `Depends on: QE-106`
`Branch: qe-125/regime-labelling`

## Goal (from backlog)

*(Reviewer-added.)* "Regime-sensitive" optimisation and reporting are aspirational without regime tags;
needed so the ensemble is required to work across regimes, not just on blended history.

- Produce regime labels (vol state / trend-vs-chop, or a simple HMM) over history.
- Expose labels to the DE objective (QE-127) and validation reporting (QE-133).

**Acceptance criteria.**
- [ ] A per-regime expectancy table can be produced for any strategy/ensemble.

**Out of scope.** Strategy genome conditioning on regimes.

## Current-state evidence & placement

- **QE-106** reconstructs `qe_domain::Bar` (OHLCVT) — the history the labeller reads. Bar reconstruction
  lives in `qe-signal::reconstruct`, so the regime labeller — a derived label over those bars — is a
  natural new `qe-signal` module alongside it.
- **Placement is forced by the information firewall.** `crates/ensemble/Cargo.toml` *deliberately* has no
  `qe-wfo` dependency (QE-001/QE-132 search⟂portfolio firewall), yet QE-127's DE objective (in `ensemble`)
  must consume regime labels and so must QE-133 reporting. The only crate **both** `qe-wfo` and `ensemble`
  already depend on is `qe-signal`. So regime labelling lives in `qe-signal`, importable by either side
  without a new edge or a firewall breach.

## Design

### D1 — Two interpretable axes, deterministic (no HMM)

A regime is two orthogonal axes, both classics, both cheap and deterministic (an HMM would add an RNG /
EM fit and non-determinism for no AC benefit):
- **Volatility** `VolState ∈ {Calm, Volatile}` — rolling realised volatility (std-dev of log-returns over
  `window`), split at the **median** of the series' own rolling vols. A median split is adaptive
  (asset/scale independent) and deterministic; bars on or below the median are `Calm`.
- **Trend-vs-chop** `TrendState ∈ {Trending, Choppy}` — Kaufman's **efficiency ratio**
  `|close[i] − close[i−W]| / Σ|close[k] − close[k−1]|` ∈ `[0, 1]`: net move over summed absolute moves. A
  pure trend → 1, pure chop → 0. Already normalised, so a fixed `trend_threshold` (default `0.5`) is
  meaningful across assets; `≥ threshold` ⇒ `Trending`.

`Regime { vol, trend }` is the product (4 regimes). `label_regimes(bars, cfg) -> Vec<Option<Regime>>`
returns a per-bar label; the first `window` bars are `None` (stats undefined in the warm-up).

### D2 — The expectancy table (AC)

`expectancy_table(returns, labels) -> ExpectancyTable` pairs a strategy/ensemble's per-bar `returns[i]`
with `labels[i]` and aggregates per regime: `count`, `mean_return` (the expectancy), `total_return`,
`win_rate`. Rows come out in a fixed `Regime` order (a `BTreeMap` over the derived `Ord`); `unlabelled`
counts the warm-up/`None` bars so the rows + unlabelled reconcile to the input length. It is
strategy-agnostic — *any* return series aligned to the labels yields its per-regime expectancy, which is
exactly what lets QE-133 report "does this ensemble work in every regime, or only on blended history?" and
what QE-127's DE objective reads to demand cross-regime performance.

### D3 — Exposure to QE-127 / QE-133

The public surface — `label_regimes`, `expectancy_table`, `Regime`/`VolState`/`TrendState`,
`RegimeExpectancy`/`ExpectancyTable`, `RegimeConfig` — is the integration point. Both consumers import
`qe_signal`; neither needs anything beyond these pure functions over bars + a return series.

## Module / API plan

New module `crates/signal/src/regime.rs`, re-exported from `qe_signal`:

- `VolState`, `TrendState`, `Regime` (`Copy`/`Eq`/`Ord`/`Hash`).
- `RegimeConfig { window, trend_threshold }` (+`Default`/`with_defaults`), `DEFAULT_REGIME_WINDOW = 20`,
  `DEFAULT_TREND_THRESHOLD = 0.5`.
- `label_regimes(bars: &[Bar], cfg: &RegimeConfig) -> Vec<Option<Regime>>`.
- `RegimeExpectancy { regime, count, mean_return, total_return, win_rate }`,
  `ExpectancyTable { rows, unlabelled }` + `get(regime)`/`row(...)`.
- `expectancy_table(returns: &[f64], labels: &[Option<Regime>]) -> ExpectancyTable`.
- No new deps (`rust_decimal` already present; `Decimal::to_f64` via its `prelude::ToPrimitive`).

## Test plan (TDD)

1. **Per-regime expectancy table (AC).** History = a calm uptrend segment then a volatile choppy segment;
   label it; feed a long-biased return series (positive in the trend, ~0/negative in chop). The table has
   a row for each present regime, counts + unlabelled reconcile to the input length, and the trending
   regime's `mean_return` exceeds the choppy regime's — the table demonstrably distinguishes regimes for
   an arbitrary strategy.
2. **Labeller — trend vs chop.** A smooth monotone series labels `Trending`; a zig-zag (mean-reverting)
   series of similar amplitude labels `Choppy`.
3. **Labeller — vol split.** Concatenated low-vol then high-vol segments label the high-vol bars
   `Volatile` and the low-vol bars `Calm` (median split).
4. **Warm-up.** The first `window` labels are `None`; later ones are `Some`.
5. **Determinism & reconciliation.** `label_regimes` is pure (equal inputs → equal labels); the
   expectancy table's per-regime counts + `unlabelled` sum to the aligned length.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-signal`,
`cargo test --workspace`.

## Risks

- **Thresholds are config-ready, not tuned.** The median vol split and `0.5` efficiency cutoff are
  deliberately simple; the AC needs the table to *distinguish* regimes, not a calibrated regime model.
  Real calibration (and the optional HMM upgrade) is a downstream concern, behind the same API.
- **Boundary bars.** A `window` straddling a regime change is labelled by the mixed statistics — a few
  transition bars may be "wrong"; this does not affect the table's ability to separate the bulk of each
  regime. Documented.
- **Median split fractures a near-constant-vol segment.** Because the split is the 50th percentile of the
  *whole* series' rolling vols, a segment whose rolling vol is near-constant (near-zero spread) and forms
  the majority will have the median land *inside* it, splitting that segment ~50/50 on noise. The robust,
  construction-independent claim is therefore the *rate ordering between regimes* (a genuinely high-vol
  segment carries a higher `Volatile` rate than a low-vol one), which is what the test asserts and what
  the expectancy table needs — not a per-bar label within a uniform stretch. A calibrated/quantile or HMM
  split is the downstream upgrade behind the same API.
- **Alignment is the caller's contract.** `expectancy_table` pairs `returns[i]` with `labels[i]` by
  index; the caller must align its strategy returns to the same bar index (the warm-up `None`s are
  skipped, not dropped from the index).
