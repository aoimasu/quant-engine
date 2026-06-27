# QE-104 — Fusion, normalisation & Arrow serialisation — design note

`Phase: P1` · `Area: ② Import & fusion` · `Depends on: QE-103`
`Branch: qe-104/fusion-normalisation-arrow`

## Goal (from backlog)

Coalesce the two ingress paths (QE-101 bulk dumps + QE-102 REST month-to-date) into **one
normalised, temporally-aligned corpus**.

- **This ticket fixes the canonical series set** (perps klines, funding, premium index, spot
  klines, futures metrics, spread-to-underlier); the fetchers (QE-101/102) stay source-abstract
  and do not hard-code it.
- Daily→monthly coalescence; derived fields (VWAP, split/contract adjustments); temporal
  alignment across series; Arrow record-batch output.
- Deterministic given inputs (QE-006).

**Acceptance criteria.**
- [ ] Fusion is byte-reproducible for fixed inputs.
- [ ] Derived fields match hand-computed references on a fixture window.

**Out of scope.** Persistence into the LMDB market store (QE-105); indicators (QE-107).

## Current-state evidence

- `qe-ingest` already holds the ingress + integrity layers: `source`/`fetcher`/`downloader`
  (QE-101 dumps), `rest`/`backfill` (QE-102 month-to-date), and `integrity`/`fill`/`coverage`/
  `reconcile`/`quality` (QE-103). Fusion is the natural next module **in the same crate** — it
  consumes those outputs and keeps the `qe-runtime ⊥ qe-wfo/qe-ensemble` topology guard
  (QE-001) untouched (fusion is offline/training-only).
- The shared vocabulary is in `qe-domain`: `Bar` (OHLCVT, validated), `Resolution` (single
  shared enum, `M5` is the base bar per the project decision), `Timestamp`/`TimeInterval`
  (epoch-ms, half-open), `Price`/`Qty`/`Notional` (exact `rust_decimal`, **never float money**),
  `FundingRateSample`, `InstrumentId`.
- **QE-103 hand-off:** `fill::plan_fill(present, interval_ms, start, end, max_gap_ms)` returns a
  `FillPlan { filled: Vec<FilledPoint>, holes: Vec<Hole> }`. QE-104's temporal alignment is the
  declared consumer of `holes` (the distinct `Hole` type was introduced in QE-103 precisely so
  the fuser cannot confuse a fill-hole with an `integrity::Gap`). Filled slots carry the last
  present value forward (within bound); holes stay **NaN/None** — leakage-safe, no fabricated
  values across a wide outage.
- `deny.toml` allows MIT/Apache-2.0/BSD/ISC/Zlib/MPL/Unicode/CC0; the load-bearing invariant
  since QE-005 is that **CI's default build + `cargo deny` stay green**. QE-101/102 established
  the pattern of gating heavy/optional deps behind a default-off cargo feature (`http`).

## Decisions

### D1 — Arrow lives behind a default-off `arrow` feature; the fusion core is always-on

The headline "Arrow output" is real, but the dependency must not bloat the default build or risk
the deny gate. Mirroring the `http` precedent:

- **Always-on, pure fusion core** (no third-party data deps beyond `serde`/`rust_decimal`): the
  canonical series set, coalescence, derived fields, temporal alignment, and a deterministic
  `FusedCorpus` value with a **canonical JSON byte serialisation**. All of QE-104's *logic* and
  both ACs are exercised here, offline, in the default `cargo test`.
- **`arrow` feature** (`arrow-array` + `arrow-schema` + `arrow-ipc`, all `default-features =
  false`): `FusedCorpus::to_arrow_ipc()` builds a fixed-schema `RecordBatch` and writes it to
  Arrow IPC stream bytes. Verified locally to be **`deny`-clean** (whole stack is Apache-2.0 /
  MIT, advisories+bans ok; no `chrono`/`zstd`/`lz4` pulled — IPC built without compression).

Byte-reproducibility (AC #1) is proven **twice**: the canonical JSON in the default build, and
the Arrow IPC bytes under `--features arrow` (Arrow IPC embeds no clock/random state, so equal
inputs ⇒ equal bytes).

### D2 — Canonical series set is a first-class enum, source-abstract

`CanonicalSeries { PerpKlines, Funding, PremiumIndex, SpotKlines, FuturesMetrics,
SpreadToUnderlier }` with `as_str()`/`ALL`. The fetchers (`source.rs`/`rest.rs`) keep their own
`DataKind`/endpoint vocabulary and **do not reference** `CanonicalSeries` — fusion owns the
canonical mapping, satisfying "fetchers stay source-abstract".

### D3 — Derived fields are exact and hand-computable

All on `rust_decimal` (no float):

- `typical_price(bar) = (high + low + close) / 3`.
- `vwap(bars) = Σ(typicalᵢ · volumeᵢ) / Σ volumeᵢ`, `None` when total volume is zero/empty — a
  true volume-weighted average over the window, hand-computable for the fixture test (AC #2).
- `Adjustment { price_factor, qty_factor }` (+ `IDENTITY`); `adjust_bar` multiplies OHLC by
  `price_factor` and volume by `qty_factor` — models contract-multiplier / split adjustments
  deterministically (default identity is a no-op).
- `spread_to_underlier(perp_close, spot_close) = perp_close − spot_close` — the derived
  spread-to-underlier series from aligned perp & spot closes (signed ⇒ `Notional`/`Decimal`).

### D4 — Coalescence: merge → dedup → sort, deterministic

`coalesce_bars(partitions)` flattens daily partitions into one ascending series keyed by
`open_time`; on a duplicate `open_time` the **last** partition wins (the fresher REST backfill
overrides the vendor dump, consistent with QE-102/103 where REST is the fresher source). Output
is always sorted ascending and unique — the deterministic precondition for alignment.

### D5 — Temporal alignment onto the base grid via the QE-103 fill plan

`align_onto_grid(bars, interval, [start,end), max_gap_ms)` walks the expected `M5` grid, runs
`plan_fill`, and emits one `Cell` per slot: `Filled(value, from_ms)` within the bound, or
`Hole` (None) where QE-103 says the gap is too wide. No fill across a hole ⇒ no leakage.

## Module plan (all in `qe-ingest`)

| file | responsibility |
|---|---|
| `canonical.rs` | `CanonicalSeries` enum (the fixed set), `as_str`/`ALL` |
| `derive.rs` | `typical_price`, `vwap`, `Adjustment`/`adjust_bar`, `spread_to_underlier` |
| `coalesce.rs` | `coalesce_bars` daily→monthly merge/dedup/sort |
| `fuse.rs` | grid build, `align_onto_grid`, `FusedColumn`/`FusedCorpus`, canonical JSON bytes, `fuse()` orchestration |
| `arrow.rs` (`#[cfg(feature = "arrow")]`) | `FusedCorpus → RecordBatch → IPC bytes`, fixed schema |

`lib.rs`: wire the modules + re-export the public surface; add `arrow` to `[features]`.

## Test plan (TDD)

- **derive (AC #2 — hand-computed):** `typical_price` exact thirds; `vwap` over a 3-bar window
  vs a hand-computed `Σ(t·v)/Σv`; zero-volume → `None`; `adjust_bar` identity is a no-op and a
  2× price factor doubles OHLC while preserving the OHLC invariant; `spread_to_underlier` sign.
- **coalesce:** two daily partitions merge, sort, and dedup (last-wins) into one ascending
  series; already-sorted input is unchanged.
- **align:** small gap fills forward within bound; over-bound region is `Hole` (None), never
  filled across (ties to QE-103 AC #1); leading missing run is a hole.
- **fuse (AC #1 — byte-reproducible):** two `fuse()` runs on identical inputs produce
  byte-identical canonical JSON; column order + grid are fixed; the canonical series set is
  exactly `CanonicalSeries::ALL`.
- **arrow (`--features arrow`):** `to_arrow_ipc()` returns identical bytes across two calls
  (byte-reproducible), and the round-tripped schema has the expected fixed columns/order.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`
(and `cargo clippy -p qe-ingest --features arrow`); `cargo test --workspace --locked` (and
`-p qe-ingest --features arrow`); `cargo deny check` (and `--all-features` to cover the arrow
tree). Topology (QE-001) unaffected — all additions stay inside `qe-ingest`.

## Risks

- **Arrow gated, not default** — same posture as `http`; the *fusion logic* is fully tested in
  the default build, and the Arrow path is covered under the feature + verified `deny`-clean.
  Wiring the fused Arrow batch into the LMDB store is explicitly QE-105.
- **Coalescence dedup policy** (last-wins) is a deliberate, documented choice; reconciliation of
  *divergent* overlap values remains QE-103's `reconcile`/`quality` responsibility, not silent
  here.
