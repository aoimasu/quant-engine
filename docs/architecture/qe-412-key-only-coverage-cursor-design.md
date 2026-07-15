# QE-412 — Coverage query without full `Bar` decode (key-only LMDB cursor)

`Phase: PreP3` · `Area: backend / storage efficiency` · `Effort: M`
Spec of record: `### QE-412` in `docs/reviews/2026-07-15-team-improvement-review.md` (there is no
`docs/mds/tickets/qe-412.md`).

## Problem / current-state evidence

`GET /api/market-data/coverage` → `qe_storage::coverage_all` → `coverage`
(`crates/storage/src/coverage.rs`). For every instrument × every `Resolution::ALL`, `coverage` calls:

```rust
let bars = store.scan_bars(instrument, resolution,
    Timestamp::from_millis(i64::MIN), Timestamp::from_millis(i64::MAX))?;
```

`scan_bars` (`crates/storage/src/store.rs`) delegates to `scan_series`, which iterates the prefix and
**decodes every value** through `SerdeJson<Bar>` into a `Vec<Bar>`:

```rust
for item in db.prefix_iter(rtxn, prefix)? {
    let (key, value) = item?;      // value is a fully-deserialised Bar
    ...
    out.push(value);
}
```

`coverage` then reads only `bars.first()`, `bars.last()`, `bars.len()` — every JSON `Bar`
deserialisation and the whole `Vec<Bar>` allocation is discarded. On a real corpus this is potentially
millions of wasted `serde_json` deserialisations per coverage request.

### Timestamps are recoverable from the KEY alone

Key layout (`crates/storage/src/key.rs`):

- `bar_prefix(instrument, resolution)` = `instrument ‖ 0x00 ‖ [resolution_ordinal]`.
- `bar_key(...)` = `bar_prefix ‖ order(time)` where `order(time)` is a sign-flipped big-endian `i64`
  (order-preserving: byte order == time order for all `i64`, negatives included).
- `time_from_key(key)` recovers the timestamp from the **trailing 8 bytes** of the key — no value
  involved.

Because keys under one prefix sort chronologically, the **first** key in the prefix carries the
earliest `open_time` and the **last** key the latest. So `(first_open_time, last_open_time, count)`
are all derivable from keys alone.

### Precedent: `bar_instruments` already does key-only iteration

`MarketStore::bar_instruments` iterates `self.bars.remap_data_type::<Bytes>().iter(...)` and reads only
keys — the value type is remapped so `SerdeJson<Bar>` is never invoked. QE-412 mirrors this, using
`heed::types::DecodeIgnore` (whose `bytes_decode` returns `Ok(())` without touching the value bytes) to
make "no value decode" a **type-level** guarantee.

## Decisions

1. **New method** `MarketStore::coverage_bounds(&self, instrument, resolution) -> Result<Option<(Timestamp, Timestamp, usize)>, StorageError>`.
   - Opens a read txn, builds `bar_prefix(instrument, resolution)`.
   - Iterates `self.bars.remap_data_type::<DecodeIgnore>().prefix_iter(&rtxn, &prefix)?`.
   - For each item decodes **only the key** via `time_from_key`; tracks first-seen time, last-seen
     time, and a running count.
   - Returns `None` when the prefix is empty, else `Some((first, last, count))`.
   - `DecodeIgnore::DItem = ()` → the closure never sees a `Bar`; the `SerdeJson<Bar>` decoder is
     provably never called on this path.

2. **Reimplement** `coverage` / `coverage_all` on top of `coverage_bounds`. `coverage_all` still
   enumerates via `bar_instruments`; `coverage` still loops instruments (caller order) × `Resolution::ALL`
   (ascending) and pushes one `CoverageRow` per non-empty pair — identical shape and row ordering.

3. **Public API stability.** `coverage`'s signature is unchanged (CLI re-exports it at
   `crates/cli/src/jobs/ingest.rs:20`). `coverage_bounds` is additive.

### Boundary-behaviour note (intentional, non-observable on fixtures)

The old path scanned the half-open range `[i64::MIN, i64::MAX)`, which would *exclude* a hypothetical
bar whose `open_time == i64::MAX`. `coverage_bounds` counts the entire prefix, so it would *include*
such a bar. This differs only at `open_time == i64::MAX` — an impossible value for a real market bar
(epoch-ms), absent from all fixtures. Output is therefore byte-identical on every realistic input; the
new behaviour is if anything strictly more correct. Documented here so the divergence is not a surprise.

## Test plan

- **Byte-identical coverage output (AC #1):** the existing integration test
  `bar_instruments_dedupes_and_coverage_reports_all_instruments` (`crates/storage/tests/store.rs`)
  asserts exact `CoverageRow` vectors (values, `from`/`to` millis, `bars`, ordering) for `coverage` and
  `coverage_all` over a multi-instrument / multi-resolution fixture. It must stay green unchanged —
  that is the byte-identical fixture check.
- **No `Bar` value decode on the coverage path (AC #2):** a unit test in `store.rs` writes a bar under a
  valid `bar_key` but with **undecodable value bytes** (via `remap_data_type::<Bytes>()`), then asserts:
  - `coverage_bounds` (and hence `coverage`) returns the correct `(first, last, count)` — proving the
    coverage path reads keys only; and
  - `scan_bars` (the decode path) errors on the same store — proving the value really is undecodable,
    so the coverage success is meaningful rather than vacuous.
  Combined with `DecodeIgnore`'s type-level guarantee, this is a direct, provable "no value decode".
- **Bounds correctness:** a unit test on `coverage_bounds` for empty (`None`), single-bar
  (`first == last`, `count == 1`), and multi-bar prefixes.

## Risks

- **Count cost.** `coverage_bounds` still iterates every key in the prefix to count. That is inherent
  without a persisted count index (explicitly out of scope), but it avoids all value decode + the
  `Vec<Bar>` allocation — the dominant cost. Key iteration is cheap relative to `serde_json` decode.
- **Firewall.** No new crate edges: `coverage.rs` continues to depend only on `MarketStore` + `qe-domain`.
- **Blast radius.** Additive storage method + internal rewrite of two functions. Public `coverage` /
  `coverage_all` / `CoverageRow` unchanged, so CLI and server callers are untouched.

## Out of scope

LMDB schema changes; a persisted coverage/count index.
