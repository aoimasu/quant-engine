# QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — design note

`Phase: P1` · `Area: ④ Signal generation (shared)` · `Depends on: QE-106`
`Branch: qe-107/indicator-catalogue`

## Goal (from backlog)

The catalogue is the **single shared module** used offline and online; quantised states are the
substrate the strategy genome reasons over.

- A broad starter set (~20+ indicators) — MA/EMA, RSI, MACD, ATR, Bollinger, ADX/Stochastics, OBV,
  momentum/returns, plus funding/OI/premium-derived factors — each producing **quantised states**
  with **deterministic lookback** and a **configurable number of states**.
- Built **batch + streaming compatible from day one** (same code path drives both).
- Catalogue is **versioned**; each indicator declares its **max lookback** (feeds purge/embargo).

**Acceptance criteria.**
- [ ] Each indicator's batch output equals its streaming output bar-for-bar.
- [ ] Declared lookback matches actual data dependency (verified).

**Out of scope.** Feature assembly (QE-108); genome (QE-110).

## Current-state evidence

- `qe-signal` is `qe-domain`-only and **storage-free** (runtime hot path, QE-003); QE-106 added the
  `reconstruct` module with the "one fold drives batch + streaming" pattern. QE-107 reuses that
  pattern: **one `update(sample) -> Option<QState>` path**, so batch is just streaming over a slice
  → AC #1 is structural.
- `qe-domain::Bar` gives OHLCV + trades; `rust_decimal` is already a `qe-signal` dep (QE-106). The
  funding/OI/premium factors read the fused scalar series (QE-104), carried alongside the bar.

## Decisions

### D1 — One `update` path ⇒ batch == streaming is structural (AC #1)

Every indicator implements `Indicator::update(&mut self, &Sample) -> Option<QState>` (state until
warm, then a quantised state each bar). `compute_batch` simply folds `update` over a slice, so a
streaming feed and a batch feed execute identical code — they cannot diverge. The AC test runs both
forms over the **whole catalogue** and asserts equality.

### D2 — FIR (windowed) indicators ⇒ lookback == data dependency exactly (AC #2)

A true EMA/Wilder-RSI is IIR (infinite, decaying memory), so "lookback == data dependency" could not
hold strictly. The catalogue therefore uses **finite-window (FIR) variants**: each indicator's latest
output reads **exactly the last `lookback` samples** (a ring buffer), nothing older. This makes the
leakage-relevant property — *the output at bar t is independent of any bar older than `t -
lookback`* — **literally true and testable**, which is what purge/embargo (QE-128/WFO) actually
needs. The AC test verifies it generically: perturbing any out-of-window bar leaves the latest output
byte-identical; perturbing the most recent bar changes it.

### D3 — Quantisation: deterministic, no future data, configurable states

Each indicator owns a [`Quantiser`] mapping its continuous value → `QState(0..num_states)`:
- `Linear { min, max, states }` — equal-width bins over a known range (bounded oscillators: RSI,
  Stoch, Williams %R, %B…), values clamped.
- `Bands { edges }` — ascending interior thresholds, `states = edges.len() + 1` (signed/zone factors:
  momentum, ROC, MACD-hist, funding…).
Both are **point-wise** (no rolling quantiles / no dataset-wide fit), so quantisation never peeks at
future data and is identical batch vs streaming. `num_states` is configurable per indicator via
`CatalogueConfig`.

### D4 — Versioned catalogue + declared lookback

`CATALOGUE_VERSION: u32` bumps when the set or any indicator's semantics change. Each indicator
exposes `IndicatorSpec { id, lookback, num_states }`; `catalogue(&CatalogueConfig)` builds the full
`Vec<Box<dyn Indicator>>`. `max_lookback()` over the catalogue feeds purge/embargo.

## Module plan (`crates/signal/src/indicator/`)

| file | responsibility |
|---|---|
| `mod.rs` | `Sample`, `QState`, `Indicator` trait, `IndicatorSpec`, `CatalogueConfig`, `catalogue()`, `CATALOGUE_VERSION`, `compute_batch`, generic AC tests |
| `quant.rs` | `Quantiser` (`Linear`/`Bands`) + tests |
| `roll.rs` | `Roll` fixed-capacity ring buffer (last-N values) with min/max/sum/mean/std + tests |
| `price.rs` | price/volume indicators (SMA, EMA-window, RSI, ROC, momentum, Stoch %K, Williams %R, ATR, %B, bandwidth, CCI, std-returns, volume-ratio, signed-volume, CMF, MFI, DPO, Aroon-osc, MACD-hist) |
| `flow.rs` | funding/OI/premium factors (funding state, funding avg, OI ROC, premium state) |

`lib.rs` wires `indicator` + re-exports the public surface.

**Indicator set (~24, ≥20):** `sma_n`, `ema_n` (windowed), `rsi_n`, `roc_n`, `momentum_n`,
`stoch_k_n`, `williams_r_n`, `atr_n`, `bb_percent_n`, `bb_bandwidth_n`, `cci_n`, `std_returns_n`,
`volume_ratio_n`, `signed_volume_n`, `cmf_n`, `mfi_n`, `dpo_n`, `aroon_osc_n`, `macd_hist_f_s_g`
(price/volume); `funding_state`, `funding_avg_n`, `oi_roc_n`, `premium_state` (flow). Each declares
its exact finite lookback (e.g. `rsi_14` reads 15 closes → lookback 15).

## Test plan (TDD)

- **AC #1 (parity), generic:** for every indicator in `catalogue(default)`, feed a fixture series
  one-bar-at-a-time (streaming) and via `compute_batch` (batch); assert the state vectors are equal.
- **AC #2 (lookback == dependency), generic:** for every indicator with lookback `L` over a series
  longer than `L`: perturbing a bar at index `len-1-L` (just outside the window) leaves the **latest**
  state unchanged; perturbing the latest bar changes it (skipping indicators whose value the perturbed
  field can't affect — perturb a field each indicator reads).
- **Per-indicator spot checks:** hand-computed SMA/RSI/Stoch/ROC on a tiny fixture; quantiser bin
  edges (Linear clamping, Bands thresholds); `Roll` capacity/stat helpers; warmup returns `None`
  until exactly `lookback` samples seen; flow indicators emit `None` when their scalar is absent.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
`cargo test --workspace --locked`; `cargo deny check` (no new third-party deps); topology guard
(`qe-signal` stays `qe-domain`-only).

## Risks

- **FIR vs textbook IIR:** the catalogue ships finite-window variants (Cutler-style RSI, simple-mean
  ATR, windowed EMA) on purpose, so lookback is exact and leakage-safe. Documented per indicator;
  IIR smoothing can be added later behind a declared, embargo-aware lookback if needed.
- **Scope:** feature assembly/normalisation (QE-108) and the genome (QE-110) are out — this ticket
  ships the catalogue + quantised states + the two ACs only.
