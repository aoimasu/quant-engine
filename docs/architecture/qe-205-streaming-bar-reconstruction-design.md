# QE-205 — Streaming bar reconstruction + live kline source — design note

`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-202, QE-106` · `Branch: qe-205/streaming-bar-reconstruction`

## Goal (from backlog)

Live multi-resolution bars must be reconstructed by streaming, primed by REST and stitched to wss, using
the **same** reconstruction as batch.

- Live kline source: REST prime + wss stitch; streaming multi-resolution reconstruction.

**Acceptance criteria.**
- [ ] Streaming bars equal batch reconstruction on replayed data (parity with QE-106).

**Out of scope.** Factor join (QE-206); the wss JSON decode / REST decode (runtime wiring — this ticket
operates on already-decoded base `Bar`s, which is exactly the QE-106 reconstruction contract).

## Current-state evidence & placement

- **QE-106's `qe_signal::reconstruct::BarReconstructor` is already streaming-shaped**: "the same
  incremental fold drives both batch and streaming — batch is literally streaming fed the whole slice", and
  it already carries a `batch_equals_streaming_parity` test. So QE-205 does **not** re-implement
  reconstruction; it reuses `BarReconstructor` and adds the **live kline source**: the REST-prime + wss-
  stitch that produces the ordered base-bar sequence, plus multi-resolution fan-out (one reconstructor per
  tier).
- **Placement: `qe-runtime`.** It already depends on `qe-signal` (the shared reconstructor — the parity
  mechanism) and `qe-venue` (REST client QE-201 + wss registry QE-202). The live pipeline (Area ④) is
  runtime territory. Firewall: `qe-runtime` is runtime-side and already reaches `qe-venue`/`qe-signal`; no
  `qe-wfo`/`qe-ensemble` edge is added, so the QE-132 guard is unaffected.

## Design

### D1 — The stitch (REST prime + wss continuation)

REST primes the source with **closed** historical base (5m) bars; wss then continues with live closed base
bars. At the boundary the two overlap (the venue re-delivers recent bars). The stitch is a monotonic
open-time dedup: track `last_open_ms`; a bar whose `open_time <= last_open_ms` is already covered →
**dropped**; a strictly-greater bar is accepted and advances the marker. This yields a single, gap-free,
strictly-increasing base-bar sequence whether a bar arrived via prime or wss — the prime/wss seam is
invisible downstream (the parity requirement).

### D2 — Multi-resolution fan-out

`LiveKlineSource` holds **one `BarReconstructor` per target tier** (e.g. 30m, 4h), all fed the base bars
the stitch accepts. Each accepted base bar is `push`ed to every tier's reconstructor; any completed coarser
bars (tagged with their own resolution) are emitted, in tier order within a step and time order across
steps. `finish()` flushes every tier's final in-progress window.

### D3 — Parity by construction

Because the only reconstruction is `BarReconstructor` (QE-106) and the stitch is a pure pre-filter on the
base sequence, the live source's per-tier output is, bar-for-bar, `reconstruct_batch(deduped_base, base,
tier)`. The AC is proven by feeding a base sequence — split into a prime prefix and a wss suffix **with a
deliberate boundary overlap** — through the live source, and asserting each tier's emitted bars equal the
batch reconstruction of the deduped base sequence.

## Module / API plan

New module `crates/runtime/src/live_kline.rs`, re-exported from `lib.rs`:
- `LiveKlineSource { base, reconstructors: Vec<(Resolution, BarReconstructor)>, last_open_ms: Option<i64> }`.
- `new(base, tiers) -> Result<Self, ReconError>`.
- `prime(&mut self, base_bars: &[Bar]) -> Result<Vec<Bar>, ReconError>` — REST prime (closed bars).
- `push_live(&mut self, bar: &Bar) -> Result<Vec<Bar>, ReconError>` — a wss closed base bar (stitched).
- `finish(&mut self) -> Result<Vec<Bar>, ReconError>` — flush all tiers.
- Internal `accept(bar)` does the resolution check + stitch dedup + fan-out (shared by `prime`/`push_live`).
- `last_open_ms()` accessor (the stitch marker) for introspection/tests.

No new external deps (uses `qe-signal` + `qe-domain`, both already runtime deps).

## Test plan (TDD)

1. **Prime+stitch == batch parity (AC).** A base sequence over several 30m & 4h windows split into a prime
   prefix and a wss suffix whose first bar **duplicates** the last primed bar. Run the live source
   (`prime` then `push_live` each suffix bar, then `finish`); for each tier assert the emitted bars equal
   `reconstruct_batch(deduped_base, M5, tier)`. Also assert the duplicate was dropped (`last_open_ms`
   unchanged by it).
2. **Stitch drops overlap, keeps order.** A wss bar at or before `last_open_ms` is ignored; a strictly
   later one advances the marker.
3. **Multi-tier fan-out.** With tiers `[M30, H4]`, 48 base bars → 8×30m completed + 1×4h on finish; per-tier
   counts match `reconstruct_batch`.
4. **Wrong base resolution rejected** (delegates to `BarReconstructor` → `UnexpectedResolution`).
5. **Empty prime then live-only** still parity-equal to batch over the live bars.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-runtime`,
`cargo test --workspace`, `cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Parity is the whole point.** It holds because the live source adds *only* a pure stitch pre-filter in
  front of the unmodified QE-106 reconstructor — no second reconstruction path exists to drift. The test
  asserts byte-equality against `reconstruct_batch`.
- **Stitch correctness.** Monotonic open-time dedup assumes closed bars arrive ascending (the venue
  contract; QE-202 already reports gaps if they don't). An out-of-order or older bar is simply dropped,
  identically to how batch would treat a re-fed window — so parity is preserved even off the happy path.
- **Decode deferred.** Turning wss/REST JSON into `Bar`s is runtime wiring (and the shared catalogue's job
  in QE-206); QE-205 operates on decoded base bars, matching the QE-106 reconstruction contract exactly.
- **Firewall.** No new deps; `qe-runtime` keeps its existing edges; QE-132 guard stays green.
```
