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
- QE-104 — Fusion, normalisation & Arrow serialisation — PR #17 — Approved & merged.
- QE-105 — Persist fused market data to LMDB — PR #18 — Approved & merged.
- QE-106 — Multi-resolution bar reconstruction (batch) — PR #19 — Approved & merged.
- QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — PR #20 — Approved & merged.

---

## QE-108 — Feature vector assembly → synthetic store — PR #21 — [Ready-for-review]

- **Branch:** `qe-108/feature-vector-assembly`
- **PR:** https://github.com/aoimasu/quant-engine/pull/21
- **Latest commit:** _(post-approval advisory follow-up — see below)_
- **Evidence/design:** `docs/architecture/qe-108-feature-vector-assembly-design.md`
- **Changed surface:** `crates/signal` (**new** `src/feature.rs`, `lib.rs` wiring, `QState::from_index`
  in `indicator/quant.rs`), `crates/ingest` (**new** `src/features.rs` + `tests/features.rs`, `lib.rs`
  wiring). No new third-party deps. Also bundles the QE-107 archive (`docs/mds/reviewed/qe-107.md`) +
  `docs/mds/work.md` bookkeeping — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Feature vectors are reproducible and parity-safe (batch == streaming).

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow` — clean)
- `cargo test --workspace --locked` — **263 passed, 1 ignored** (qe-signal feature 5, ingest
  features integration 3)
- `cargo test -p qe-cli --test dependency_topology` — passes (`qe-signal` stays `qe-domain`-only)
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps)

Key AC-proving tests:
- **AC (reproducible + batch == streaming)** — `feature::tests::ac_batch_equals_streaming`:
  `assemble_batch` equals the streaming `FeatureAssembler::push` loop **and** is byte-identical across
  two runs. Structural: one `push` path drives the whole catalogue; `assemble_batch` is that loop.
- **Cache bridge** (`crates/ingest/tests/features.rs`):
  `assemble_cache_and_read_back_complete_vectors` (cache count == #complete vectors; last vector
  round-trips byte-for-byte via `get_indicator_state`), `cached_feature_is_stale_under_a_different_lineage`,
  `caching_is_reproducible_across_runs`.
- **Supporting:** schema↔catalogue match, byte codec round-trip (incl. `None` slots + width mismatch),
  `vectors_become_complete_after_max_lookback`.

### Design notes for the reviewer
- **Structural parity.** `FeatureAssembler::push` collects every catalogue indicator's `update` state
  (in schema order) + the bar time; `assemble_batch` is the push loop — batch == streaming inherited
  from QE-107.
- **Self-describing vectors.** `FeatureSchema` (ordered ids + lookbacks + `CATALOGUE_VERSION`) is the
  decode contract; `FeatureVector` has a compact deterministic byte codec (`i64` time + `u16` states,
  `0xFFFF` = `None`) — no serde, so `qe-signal` stays lean.
- **Closes the QE-107 flow caveat.** The assembler builds one `Sample` per bar with the scalar context
  carried alongside, so flow-factor lookback is in bar units (callers forward-fill sparse scalars onto
  the grid before assembly).
- **Caching.** Only **complete** vectors (every indicator warm) are cached — the rows WFO/DE consume —
  as one blob per bar under reserved `indicator_id "feature_vector"` (cannot collide; key also carries
  `max_lookback`), lineage-tagged for staleness. Topology: `qe-signal` stays `qe-domain`-only.
- **Out of scope:** strategy evaluation (QE-120).

### Review notes

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs `main`
(head `dd2c314`) — read `feature.rs`, the `features.rs` bridge, both test files, and the `quant.rs`/lib
wiring. The AC is met and the code is correct; the advisories below are forward-looking robustness, not
defects.

**AC — reproducible + batch == streaming (PASS, structural).** `FeatureAssembler::push` maps each
catalogue indicator's one `update(sample)` into the states vec (schema order), and `assemble_batch` *is*
the push loop, so batch == streaming is inherited from QE-107. `ac_batch_equals_streaming` checks batch ==
a fresh streaming run **and** == a second `assemble_batch` (reproducible, since `catalogue()` builds a
deterministic Vec). `vectors_become_complete_after_max_lookback` confirms completeness timing.

**Byte codec (PASS).** `to_bytes` = `i64` time (BE) + one `u16` (BE) per slot, `0xFFFF` = `None`;
`from_bytes(bytes, width)` validates `len == 8 + width·2` and round-trips (incl. `None` slots + width
mismatch). **On the `NONE_SENTINEL` question I'm comfortable — no guard needed:** `num_states` is a `u16`
(≤ 65535), so the maximum real quantiser index is `states-1 ≤ 65534`, which can never equal `0xFFFF`
(that would require `states = 65536`, impossible for the type). The sentinel is safe **by construction**.

**Cache bridge + flow-caveat closure (PASS).** Only **complete** vectors are cached (the rows WFO/DE
consume), one opaque blob per bar under the reserved `FEATURE_VECTOR_ID = "feature_vector"` — which no
catalogue indicator uses, and QE-011's length-prefixed `IndicatorKey` makes collision impossible — keyed
also by `max_lookback`, lineage-tagged via `put_indicator_state`; `read_cached_feature` rebuilds the key
and decodes with `schema.len()` width. Round-trip, lineage-staleness, and reproducibility are proven by
the three integration tests. The QE-107 flow caveat is addressed at the right seam: the assembler builds
one `Sample` per bar with scalar context carried alongside (so flow lookback is in bar units), and
combined with QE-104's within-bound forward-fill the normal pipeline feeds dense scalars.

**Topology (PASS).** `qe-signal` stays `qe-domain`-only (the codec is hand-rolled, no serde); the bridge
lives in `qe-ingest`, which already depends on signal + storage — no new crate edge, QE-001 guard
unaffected.

**Verification caveat (transparency).** The Rust toolchain is absent from this review environment, so I
did not execute the gates. The verdict rests on full static review + hand-traced tests and the
`num_states ≤ u16` bound for the sentinel. I did not rely on the PR's "all green" claim; treat the gate
results as developer-reported. Nothing in the review contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. **The cached blob carries no schema/version self-description.** The blob encodes only `time + states`;
   the decode **width** and the **meaning** of each state index come from the reader's `CatalogueConfig`,
   not the blob. The key carries `max_lookback` (catches lookback changes) and the value is lineage-tagged
   (catches vintage changes), but neither the key nor the blob carries `CATALOGUE_VERSION` or `num_states`.
   So a config change that alters `num_states`/`CATALOGUE_VERSION` while keeping the catalogue size **and**
   `max_lookback` would produce same-key, same-width blobs that **silently mis-decode** (a bucket index
   computed under 5 states read as if 7) — *unless* it also changes the vintage lineage (→ cache miss).
   The safety therefore hinges on **`CatalogueConfig` being folded into the vintage lineage**;
   `CatalogueConfig` lives in `qe-signal` (not `qe_config::Config`), so please confirm that linkage, or
   embed `CATALOGUE_VERSION` (+ a state-count discriminant) in the cache key or a blob header so a
   schema-drifted read can't mis-decode. This is the one I'd most want addressed before a config migration.
2. **`from_bytes` doc clause is vacuous.** Its comment says it returns `None` on "a state index `>=
   NONE_SENTINEL` other than the sentinel" — but nothing exceeds `u16::MAX`, so that branch is
   unreachable and the code never rejects an out-of-range non-sentinel index. Either reword the doc, or
   (defense-in-depth) have `from_bytes` take `num_states` and reject `code >= num_states`. Trivial.
3. **(Informational) Flow-caveat closure is by contract, not enforcement.** One-`Sample`-per-bar +
   documented forward-fill is the right mechanism, but density isn't asserted at the assembly boundary — a
   caller feeding sparse scalars still gets flow lookback in present-scalar units. Consistent with the
   QE-107 advisory (QE-108 dense feed **or** QE-128 embargo); just noting the residual is a caller
   contract.

### Post-approval follow-up (coder) — advisories addressed; status → [Ready-for-review]

Addressed the review advisories (the substantive one + the trivial doc; #3 was informational).
- **#1 (cached blob not self-describing → silent mis-decode risk on a same-lineage catalogue change)
  — DONE (fix).** The byte codec is now **self-describing**: `to_bytes(&schema)` prepends a header
  `[version u32][num_states u16][width u16]`, and `from_bytes(bytes, &schema)` **rejects** (returns
  `None`) any blob whose embedded version / state-count / width disagrees with the reader's schema —
  so a `CATALOGUE_VERSION` or `num_states` change can never be silently mis-read even at the same
  catalogue size. `FeatureSchema` now carries `num_states`. New test
  `decode_rejects_state_count_mismatch` (same width, different `num_states` ⇒ `None`).
- **#2 (vacuous `from_bytes` doc clause) — DONE.** Rewrote the codec docs to describe the real header
  validation; dropped the impossible "index >= sentinel" clause.
- **#3 (flow density is a contract, not enforced) — informational, acknowledged.** Left as a
  documented caller contract (assembler builds one `Sample`/bar; callers forward-fill sparse scalars
  onto the grid, per QE-104). Enforcement at the assembly boundary would belong with the QE-128
  embargo work, not here.
- Gates re-run green: fmt ok; clippy clean; `qe-signal` 30 / workspace 264 tests; deny unchanged.
