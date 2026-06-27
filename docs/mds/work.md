# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.
- QE-012 — Instrument-universe config & point-in-time membership — PR #12 — Approved & merged.
- QE-013 — Local run & deployment-agnostic packaging — PR #13 — Approved & merged. **(P0 complete)**
- QE-101 — Binance public-dumps downloader — PR #14 — Approved & merged.
- QE-102 — Venue REST month-to-date backfill client — PR #15 — Approved & merged.
- QE-103 — Data-integrity & source reconciliation validation — PR #16 — Approved & merged.

---

## QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — [Ready-for-review]

- **Branch:** `qe-104/fusion-normalisation-arrow`
- **PR:** https://github.com/aoimasu/quant-engine/pull/17
- **Latest commit:** _(post-approval advisory follow-up — see below)_
- **Evidence/design:** `docs/architecture/qe-104-fusion-normalisation-arrow-design.md`
- **Changed surface:** `crates/ingest` — **new** `src/{canonical,derive,coalesce,fuse,arrow}.rs`,
  `src/lib.rs` (module wiring + exports), `Cargo.toml` (+`rust_decimal`; new default-off `arrow`
  feature = `arrow-array`/`arrow-schema`/`arrow-ipc`), `Cargo.lock`. Pure logic, no network. Also
  bundles the QE-103 archive (`docs/mds/reviewed/qe-103.md`) + `docs/mds/work.md` bookkeeping —
  branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Fusion is byte-reproducible for fixed inputs.
- [x] Derived fields match hand-computed references on a fixture window.

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow --all-targets -- -D warnings` — clean)
- `cargo test --workspace --locked` — **222 passed, 1 ignored**; `-p qe-ingest --features arrow`
  — 78 passed (+3 arrow tests)
- `cargo deny check` — advisories/bans/licenses/sources ok; also `cargo deny --all-features check`
  ok (covers the optional arrow tree: all Apache-2.0/MIT, no chrono/zstd/lz4)

Key AC-proving tests:
- **AC #1 (byte-reproducible)** — `fuse::tests::fuse_is_byte_reproducible_for_fixed_inputs` (two
  `fuse()` runs → identical canonical JSON); `arrow::tests::ipc_bytes_are_byte_reproducible` (two
  `corpus_to_ipc()` calls → identical Arrow IPC bytes). Columns are emitted in
  `CanonicalSeries::ALL` order and the grid is fixed.
- **AC #2 (derived fields vs hand-computed)** — `derive::tests::vwap_matches_hand_computed_reference`
  (Σ(tᵢ·vᵢ)/Σvᵢ = 22.8 over a 3-bar fixture), `typical_price_is_exact_thirds`,
  `price_factor_scales_ohlc_and_preserves_invariant`, `spread_is_signed_perp_minus_spot`.
- **Supporting:** `coalesce` merge/dedup(last-wins)/sort; `fuse::align_fills_within_bound_and_holes_beyond`
  (forward-fill within bound, holes beyond — ties to QE-103 AC #1, via `plan_fill`);
  `spread_is_hole_where_spot_missing` (no leakage where the underlier is absent).

### Design notes for the reviewer
- **Fusion consumes the QE-103 fill plan, not its own re-derivation.** `align_onto_grid` calls
  `plan_fill`; within-bound misses carry the last present value forward, over-bound/leading runs
  become `Cell::Hole` (NaN). The distinct `Hole` type introduced in QE-103 is honoured — fusion
  never confuses a fill-hole with an `integrity::Gap`.
- **Canonical series set is owned by fusion** (`CanonicalSeries`), source-abstract: the QE-101/102
  fetchers keep `DataKind`/REST endpoints and don't reference it.
- **Exact money throughout** — derived fields are `rust_decimal` (never float). The Arrow column is
  `Float64` (interchange only); the exact values live in `FusedCorpus`, which is the source of truth.
- **Arrow gated behind a default-off `arrow` feature** (the `http` precedent): keeps CI's default
  build + `cargo deny` dependency-light and green; the fusion *logic* is fully tested in the default
  build, and the Arrow path is covered under the feature and verified `deny`-clean.
- **Out of scope:** persistence into the LMDB market store (QE-105); indicators (QE-107).
  **Topology:** all additions stay within `qe-ingest`; QE-001 guard unaffected.

### Review notes

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `bfb8d62`) — read all five new modules and every test. Both ACs are met and **correct**; fusion
inherits QE-103's leakage-safety rather than re-deriving it.

**AC #1 — byte-reproducible (PASS).** `FusedCorpus` serialises through a fully `Vec`/scalar shape with
**no maps in the output** (the internal `BTreeMap`s are sorted and don't reach the wire), columns are
emitted in the fixed `CanonicalSeries::ALL` order (enforced + asserted by
`fuse_emits_all_canonical_columns_in_order`), the grid/slots are fixed, and `Cell::Value` serialises its
`Decimal` via exact `rust_decimal::serde::str` — so `to_json_bytes` is deterministic
(`fuse_is_byte_reproducible_for_fixed_inputs`). The Arrow path mirrors it: a **fixed** schema (`Int64
slot_ms` + one nullable `Float64` per series in `ALL` order, holes→null), and IPC stream bytes that embed
no clock/random (`ipc_bytes_are_byte_reproducible`).

**AC #2 — derived fields vs hand-computed (PASS).** All exact `rust_decimal`, hand-verified:
`typical_price = (H+L+C)/3`; `vwap = Σ(typᵢ·volᵢ)/Σvol` → traced to `22.8` on the 3-bar fixture (and
`None` on empty/zero-volume); `adjust_bar` scales OHLC×`price_factor`/vol×`qty_factor` and **re-validates
through `Bar::new`** (so a negative factor errors rather than silently corrupting order); `spread =
perp_close − spot_close`, signed.

**Leakage-safety (verified).** `align_onto_grid` delegates the fill/hole decision to QE-103 `plan_fill`
and consumes the distinct `Hole` type — within-bound misses carry the last *present* value forward
(`present_map[&from_ms]` is safe because `plan_fill` only fills from a real present sample), over-bound /
leading runs become `Cell::Hole`. `subtract_columns` makes the spread a hole wherever *either* input is a
hole, so no spread is fabricated where the underlier is absent (`spread_is_hole_where_spot_missing`).
`coalesce_bars` is deterministic (BTreeMap by open-time, last/REST-partition wins on a duplicate).

**Deps / topology (PASS).** `rust_decimal` added; the `arrow` feature is **default-off** (the `http`
precedent) with `arrow-array`/`arrow-schema`/`arrow-ipc` all `default-features = false` + `optional` — so
arrow-ipc's default lz4/zstd compression and arrow-array's chrono are dropped, structurally supporting the
"no chrono/zstd/lz4" deny claim. Exact money stays in `FusedCorpus`; the `Float64` Arrow column is
interchange-only. All additions stay within `qe-ingest`; QE-001 guard untouched.

**Verification caveat (transparency).** The Rust toolchain is absent from this review environment, so I
did not execute the gates (incl. `--features arrow` clippy/test and `cargo deny --all-features`). The
verdict rests on full static review + hand-traced execution of the tests (pure deterministic logic). I did
not rely on the PR's "all green" claim; treat the gate results as developer-reported. Nothing in the
review contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. **`align_onto_grid` silently drops present samples whose timestamps aren't on a grid slot.** Both
   `plan_fill` (`present.contains(&slot)`) and the placement (`present_map.get(&slot)`) key on exact
   `start + k·interval` timestamps, so an off-grid sample is neither placed nor counted — the slot simply
   stays a hole. In-domain this is fine (klines are grid-native; funding/premium/metrics cadences divide
   the 5m base), but it's an **unstated precondition** and a latent footgun for the QE-105 caller feeding
   raw venue timestamps. Recommend documenting "samples must fall on grid slots" and/or a `debug_assert`
   (or snapping) so a misaligned feed fails loudly instead of silently losing data.
2. **(Trivial) stale Cargo.toml comment** at the `qe-config` dep: "fusion into qe-storage is QE-104" —
   persistence is **QE-105** per this entry's own out-of-scope note; the comment is also mis-placed above
   `qe-config`.
3. **(Trivial) `arrow` column maps a `Decimal::to_f64()` failure to a silent `null`** (`.and_then(|d|
   d.to_f64())`). Unreachable for realistic prices; noted only for completeness.

### Post-approval follow-up (coder) — advisories resolved; status → [Ready-for-review]

Resolved all three non-blocking advisories from the approval (strictly additive; no AC behaviour
changed).
- **#1 (latent footgun for QE-105: `align_onto_grid` silently drops off-grid samples) — DONE.**
  Documented the **grid-phase precondition** on `align_onto_grid` (in-window samples must satisfy
  `open_time == start + k·interval`; off-phase ones match no slot and are dropped; out-of-window
  ones are intentionally ignored), and added a **`debug_assert`** that catches an off-phase in-window
  sample in debug builds. `fuse` already satisfies the precondition (klines are grid-aligned by
  construction). New test `out_of_window_sample_is_ignored_not_placed` pins the windowing path.
- **#2 (stale Cargo.toml comment) — DONE.** `qe-ingest`'s dep comment now reads "persistence into
  qe-storage is **QE-105**" (was QE-104).
- **#3 (arrow Decimal→f64 failure → silent null) — DONE (doc).** `corpus_to_record_batch` now
  documents that a `to_f64` failure maps to null (interchange-only; exact value stays in
  `FusedCorpus`; does not occur for real prices).
- Gates re-run green: fmt ok; clippy clean (default **and** `--features arrow`); `qe-ingest` 76
  unit + arrow 79; workspace unaffected; deny unchanged (no dependency change).
