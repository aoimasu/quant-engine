# QE-103 — Data-integrity & source reconciliation validation

`Phase: P1` · `Area: ② Import & fusion` · `Depends on: QE-101, QE-102`

## Goal

Binance dumps have gaps, dups, out-of-order rows, schema drift, and shorter coverage for
funding/premium/OI. **Silent NaN/forward-fill creates leakage and phantom edge.** QE-103 validates a
fused-but-pre-output corpus: gap/dup/monotonic checks, coverage maps, a **leakage-safe forward-fill
policy** (no fill across a gap beyond a bound), vendor↔REST overlap diffing, and a **per-vintage
data-quality report** that fails the run on configured hard violations.

## Current state (evidence)

- QE-101 produces raw vendor dumps + `SchemaRegistry` drift detection; QE-102 produces REST `fresh`
  rows + a retained `overlap` region (vendor↔REST) — exactly the inputs QE-103 reconciles.
- `qe-domain` has `Timestamp`/`Resolution`; `serde_json` is already a `qe-ingest` dep (QE-102). This
  is **pure logic** — no network — so the whole ticket is unit-tested offline.
- Nothing yet checks integrity or applies a fill policy; today a downstream stage would have to
  forward-fill blindly. QE-103 makes the policy explicit and leakage-safe.

## Design

Five focused modules in `qe-ingest`, composed by a report:

### `integrity.rs` — per-series structural checks
`check_series(timestamps, interval_ms) -> SeriesIntegrity`:
- **gaps** — consecutive `Δ > interval` → `Gap { from_ms, to_ms, missing }` (count of absent slots);
- **duplicates** — a repeated timestamp;
- **out-of-order** — a timestamp `< ` its predecessor (and `monotonic` summary bool).
Works on the bare timestamp sequence (value-agnostic), so it covers klines/funding/premium/OI alike.

### `fill.rs` — leakage-safe forward-fill **policy** (AC #1)
`plan_fill(present, interval_ms, [start, end), max_gap_ms) -> FillPlan`:
- walks the expected grid `start, start+interval, …, < end`;
- a missing slot is **filled-forward** from the last present sample **only while** the run of
  consecutive misses stays `≤ max_gap_ms`;
- once a gap exceeds the bound, the remaining slots in it are **holes** (`Gap`), never filled —
  *no fill across a gap larger than the bound*. Value-agnostic: it returns *which* slots fill (and
  from which source ts) vs which stay holes, so the fuser carries values without leakage.

### `coverage.rs` — coverage maps + short-history flags
`coverage(timestamps, interval_ms) -> Coverage { first_ms, last_ms, present, expected, missing }`;
`flag_short_history(base, others) -> Vec<ShortCoverage>` — flags any series (funding/premium/OI) that
starts later or ends earlier than the base klines, so shorter history is **surfaced, not silently
padded**.

### `reconcile.rs` — vendor↔REST overlap diffing with tolerance
`diff_overlap(vendor, rest, Tolerance { abs, rel }) -> Vec<Divergence>` over `(ts, value)` pairs
keyed by timestamp: a matched timestamp whose values differ by more than **both** `abs` and
`rel × max(|a|,|b|)` is a `Divergence::Value`; a timestamp present in only one source is
`Divergence::MissingIn`. Tolerance diffing is diagnostic, so `f64` (no new dep).

### `quality.rs` — the per-vintage report + hard-violation gate (AC #2)
- `DataQualityReport` (serde `Serialize`) aggregates the series integrity summaries, coverage flags,
  fill holes, and divergences → the **artefact** written per vintage (JSON).
- `HardViolationPolicy { max_gap_ms, allow_duplicates, allow_out_of_order, max_divergences }`.
- `report.evaluate(&policy) -> Result<(), Vec<Violation>>` — returns the configured **hard
  violations** (a hole/gap beyond `max_gap_ms`, a forbidden duplicate / out-of-order, too many
  divergences); a non-empty list **fails the run**.

### Why this shape

- **AC #1 (no silent fill across a big gap):** `plan_fill` structurally cannot fill past
  `max_gap_ms` — the slots beyond the bound are emitted as `holes`, proven by tests that a gap just
  over the bound yields a hole while one at the bound fills.
- **AC #2 (report + hard-fail):** `DataQualityReport` serialises to the vintage artefact and
  `evaluate` turns configured violations into a run-failing `Err`. A test produces the report JSON
  and asserts a hard violation fails while a clean corpus passes.
- **Leakage-safe by construction:** the fill policy is explicit and bounded; coverage gaps and
  source divergences are reported, never hidden — directly answering the reviewer's "silent NaN/
  forward-fill creates leakage" concern.
- **Pure + offline:** value-agnostic timestamp logic + `f64` tolerance; everything is unit-tested
  with hand-built series, no network, deny-clean.

## Test plan (TDD)

- **integrity** — a clean series (no findings); injected gap (right `missing` count), duplicate,
  out-of-order (monotonic=false).
- **fill (AC #1)** — a gap `≤ max_gap` fills every slot forward; a gap `> max_gap` leaves the
  over-bound slots as holes (none filled across it); a boundary gap exactly at the bound fills.
- **coverage** — `expected/present/missing` arithmetic; `flag_short_history` flags a later-starting
  funding series vs base klines.
- **reconcile** — equal-within-tolerance → no divergence; an abs+rel breach → `Value` divergence; a
  ts in one source only → `MissingIn`.
- **quality (AC #2)** — `evaluate` returns the hard violations for an over-bound hole / duplicate /
  excess divergences and `Ok` for a clean report; the report round-trips through JSON.

## Risks / out of scope

- **Out of scope:** the fusion **output format** / Arrow serialisation (QE-104) — QE-103 validates &
  reports; it does not emit the fused corpus. Wiring the report into the per-vintage run artefact is
  finalised alongside QE-104's output.
- **Topology:** stays within `qe-ingest` (already `→ qe-config`/`qe-domain`); QE-001 guard
  unaffected. No new third-party deps (serde already present via the workspace).
