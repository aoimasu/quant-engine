# QE-011 — LMDB synthetic-data store — design / evidence

## Ticket

`Phase: P0` · `Area: ③ Storage` · `Depends on: QE-007`

**Goal.** Indicator caches and multi-resolution bars are derived artefacts read heavily by WFO/DE;
they belong in a separate store with its own lifecycle.

**Scope / requirements.**
- LMDB schema for indicator-state cache and multi-resolution bars keyed by instrument + resolution +
  indicator id + lookback + time.
- Cache invalidation tied to source lineage (QE-006).

**Out of scope.** Indicator computation (QE-107).

**Acceptance criteria.**
- Cached indicator states are byte-identical to freshly computed ones (parity test).
- Stale-source detection invalidates dependent cache entries.

## Current-state evidence

- QE-010 landed `MarketStore` + the order-preserving key helpers (`bar_key`/`bar_prefix`/
  `time_from_key`) and the `StorageError`/`SCHEMA_VERSION` patterns. QE-011 adds a **second, separate**
  store (own env, own lifecycle) reusing that infrastructure.
- QE-006 (`qe-determinism`) defines a `Lineage` with a stable `id()` (64-hex). The cache ties each
  entry to the **source lineage id** (a `&str`) it was derived from; that is the "tied to source
  lineage" hook — `qe-storage` takes the id as an opaque string (no dep on `qe-determinism`, keeping
  the storage layer lean; the caller passes `lineage.id()`).

## Design decisions

### Shared LMDB plumbing (DRY + single `unsafe`)
Extract crate-internal helpers (`engine.rs`) used by **both** stores:
- `open_env(path, map_size, max_dbs)` — the **one** `#[allow(unsafe_code)]` site for the whole crate
  (heed's `EnvOpenOptions::open` is `unsafe`), with the SAFETY note. `MarketStore` is refactored to
  use it too, so the crate has exactly one unsafe call.
- `check_or_init_schema(meta, wtxn, expected)` — records the version on first open, else rejects a
  mismatch (`SchemaMismatch`) / corrupt record (`SchemaCorrupt`). Shared by both stores.

### `SyntheticStore` (`synthetic.rs`)
One env, sub-dbs `indicators`, `recon_bars`, `meta`; `SYNTHETIC_SCHEMA_VERSION = 1`.

**Indicator-state cache (parity-critical, AC #1).**
- Key (unambiguous, length-prefixed so components can't collide):
  `u16(len sym) ‖ sym ‖ [resolution] ‖ u16(len indicator_id) ‖ indicator_id ‖ u32(lookback) ‖
  order(time)`.
- Value stores raw bytes, **not** JSON: `u32(len lineage) ‖ lineage ‖ state_bytes`. The indicator
  state is opaque `&[u8]` (computation is QE-107), and storing/returning the **exact bytes** is what
  makes "cached == freshly computed, byte-identical" hold by construction.
- `put_indicator_state(key, source_lineage, &state)`; `get_indicator_state(key, current_lineage) ->
  Option<Vec<u8>>` returns `Some` **only if the stored lineage matches** `current_lineage` — a
  stale-source entry is detected and **not served** (treated as a miss → recompute). This is the
  read-time half of AC #2.
- `invalidate_stale_indicators(current_lineage) -> usize` — scans and **deletes** every entry whose
  lineage differs from `current_lineage`, returning the count. The eviction half of AC #2.

**Multi-resolution (reconstructed) bars.**
- Reuses QE-010's `bar_key`/`bar_prefix`, value = `SerdeJson<ReconBar { source_lineage, bar }>`.
- `put_recon_bars(instrument, source_lineage, &[Bar])`, `get_recon_bar(.., current_lineage)` (lineage-
  checked like indicators), `scan_recon_bars(instrument, resolution, from, to)` (chronological window).

### Errors / deps
Reuses `StorageError`. No new third-party deps (heed/rust_decimal/serde already present). No internal
edge to wfo/ensemble → QE-001 topology guard stays green.

## Test plan (proves both ACs)

- **AC #1 (byte-identical parity):** `put_indicator_state` with crafted opaque bytes, then
  `get_indicator_state` returns a `Vec<u8>` **equal byte-for-byte** to the input (incl. empty and
  binary-with-NUL payloads). A second put under a new lineage round-trips its own bytes.
- **AC #2 (stale-source detection + invalidation):**
  - `get_indicator_state(key, "B")` returns `None` when the entry was stored under lineage `"A"`
    (detected, not served), while `(key, "A")` returns the bytes.
  - `invalidate_stale_indicators("B")` deletes the `"A"` entry (returns count ≥ 1) and keeps `"B"`
    entries; afterwards `(key, "A")` is `None` (evicted). A no-stale case returns `0`.
- Recon bars: round-trip + lineage-checked `get` + chronological `scan`.
- Schema: version recorded; mismatch + corrupt detected (mirrors QE-010, via the shared helper).
- Refactor safety: the full QE-010 `MarketStore` suite still passes after extracting the shared
  helpers (one `unsafe` site for the crate).

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **Key-component collision:** avoided by length-prefixing instrument + indicator_id (`u16` lengths —
  no truncation for any realistic id).
- **Lineage coupling:** kept as an opaque `&str` so `qe-storage` doesn't depend on `qe-determinism`;
  the caller supplies `Lineage::id()`. Documented.
- **Refactor touching merged QE-010 code:** same-crate internal extraction, behaviour-preserving;
  guarded by the unchanged QE-010 test suite.
