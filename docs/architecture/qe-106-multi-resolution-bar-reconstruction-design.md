# QE-106 — Multi-resolution bar reconstruction (batch) — design note

`Phase: P1` · `Area: ④ Signal generation` · `Depends on: QE-105, QE-011`
`Branch: qe-106/multi-resolution-bar-reconstruction`

## Goal (from backlog)

Strategies operate across resolutions (5m/30m/4h); bars must be reconstructed **deterministically
with the same code runtime will stream**.

- Base bar = **5m**; deterministically reconstruct **30m + 4h** (tier set configurable) with
  deterministic boundaries.
- Output cached to the synthetic LMDB store; designed for **batch + streaming parity** (QE-206).

**Acceptance criteria.**
- [ ] Batch-reconstructed bars equal streaming reconstruction on the same input (parity fixture).

**Out of scope.** Streaming reconstruction wiring into the live runtime (QE-205).

## Current-state evidence

- **`qe-signal`** is still a scaffold (only `crate_name()`); QE-106 lands its first real API. It
  depends on `qe-domain` only — and **must stay storage-free**: it is shared by the runtime hot path
  (QE-003 "no database on the critical path"), so the reconstruction *logic* cannot pull in LMDB.
- **`qe-storage::SyntheticStore`** (QE-011) already stores multi-resolution bars:
  `put_recon_bars(instrument, source_lineage, bars)` / `get_recon_bar` / `scan_recon_bars`, keyed by
  instrument+resolution+time and **tagged with source lineage** (stale entries detected, not served).
  So the cache target exists; QE-106 only needs to feed it.
- **`qe-domain`**: `Bar` (validated OHLCVT; `open`/`close` ∈ `[low, high]`), `Resolution`
  (`M5`/`M30`/`H4`, with `minutes()`), `Timestamp` (epoch-ms).

## Decisions

### D1 — Reconstruction logic is pure, in `qe-signal`; caching is a separate thin bridge

The fold that turns 5m bars into 30m/4h bars is **storage-agnostic** and lives in `qe-signal` so the
*identical code* runs in batch (this ticket) and streaming (QE-205/206). Persisting the result to the
synthetic store is a batch-only concern and lives in a small `qe-ingest` bridge (`qe-ingest` already
owns "persist to LMDB", QE-105, and depends on `qe-storage`; it gains a `qe-signal` dep). This keeps
the hot-path crate free of LMDB.

### D2 — Batch *is* streaming fed the whole slice → parity is structural

A single incremental fold ([`BarReconstructor`]) accumulates the current target-window and emits a
coarser bar when an incoming base bar crosses into the next window; `reconstruct_batch` is literally
"push every bar through a `BarReconstructor`, then `finish()`". Batch and streaming therefore share
**one** code path, so AC parity is structural — the fixture demonstrates feeding bars one-at-a-time
vs all-at-once yields byte-identical output.

### D3 — Deterministic boundaries by epoch-aligned windows

A base bar at `open_time` belongs to target window `floor_div(open_time, target_ms) · target_ms`
(`div_euclid`, so negatives are deterministic too). Boundaries depend only on the timestamp and the
target resolution — never on batch size, arrival order, or thread count (QE-006).

### D4 — Aggregation (standard OHLCVT roll-up)

For the base bars in one window (ascending): `open` = first open, `high` = max high, `low` = min
low, `close` = last close, `volume` = Σ volume, `trades` = Σ trades, `open_time` = window start,
`resolution` = target. The OHLC invariant is preserved (first.open and last.close each lie within
their own bar's `[low,high]` ⊆ the window's `[min low, max high]`), so `Bar::new` never rejects a
roll-up of valid base bars.

## Module / API plan

**`crates/signal/src/reconstruct.rs`** (new):
- `BarReconstructor::new(base: Resolution, target: Resolution) -> Result<Self, ReconError>` (rejects
  `target ≤ base` or `target % base ≠ 0`); `push(&mut self, &Bar) -> Result<Option<Bar>, ReconError>`
  (emits the completed coarser bar when crossing a boundary; rejects a bar whose resolution ≠ base);
  `finish(&mut self) -> Option<Bar>` (flush the last window).
- `reconstruct_batch(base_bars: &[Bar], base, target) -> Result<Vec<Bar>, ReconError>`.
- `reconstruct_tiers(base_bars, base, tiers: &[Resolution]) -> Result<Vec<Bar>, ReconError>` —
  configurable tier set; each tier independent.
- `ReconError` (thiserror): `TargetNotCoarser`, `TargetNotMultiple`, `UnexpectedResolution`,
  `InvalidBar(DomainError)`.
- `lib.rs`: wire `reconstruct` + re-export.

**`crates/ingest/src/recon.rs`** (new; +`qe-signal` dep):
- `cache_reconstructed_tiers(store: &SyntheticStore, instrument, source_lineage, base_bars, base,
  tiers) -> Result<usize, ReconCacheError>` — reconstruct every tier and `put_recon_bars`; returns
  the count cached. `ReconCacheError` wraps `ReconError` + `StorageError`.

## Test plan (TDD)

- **AC parity** (`qe-signal`): a fixture of 5m bars spanning multiple 30m (and 4h) windows —
  `reconstruct_batch` == bars emitted by pushing one-at-a-time through `BarReconstructor` then
  `finish`. Hand-checked aggregate for one window (open/high/low/close/volume/trades). Boundary
  alignment (a window starting mid-stream). Errors: `M30→M5` not coarser; a base bar with the wrong
  resolution.
- **tiers** (`qe-signal`): `reconstruct_tiers(.., [M30, H4])` yields both tiers; 4h roll-up equals
  rolling the 30m roll-ups’ equivalent base set.
- **cache bridge** (`qe-ingest` integration test): reconstruct→cache→`scan_recon_bars` round-trips
  the coarser bars under the source lineage; a *different* lineage read misses (staleness), proving
  the synthetic-store tagging is honoured.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
`cargo test --workspace --locked`; `cargo deny check` (no new third-party deps — both new edges are
workspace-internal); `cargo test -p qe-cli --test dependency_topology` (new `qe-ingest→qe-signal`
and `qe-signal` staying domain-only edges are allowed; runtime↔training invariant untouched).

## Risks

- **Ascending-input precondition:** the incremental fold groups consecutive same-window bars; the
  synthetic/market scans are chronological, so this holds. Documented on the API; out-of-order input
  would split a window (same in batch and streaming, so parity is preserved regardless).
- **Scope discipline:** live-runtime streaming wiring is QE-205; this ticket ships the shared logic +
  batch caching only. The streaming `BarReconstructor` is exposed now precisely so QE-206 can prove
  parity against the live path.
