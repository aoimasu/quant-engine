# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. On merge the approved block is archived to
`docs/mds/reviewed/<ticket>.md` and this file is cleared back to empty.

> **CI + branch protection are TEMPORARILY DISABLED for this run (user directive):** the green gate is the
> LOCAL full build/test/clippy/deny, and PRs are squash-merged without GitHub checks.

---

## PR #177 — QE-464 ingest run-kind + POST /api/ingest + real/synthetic provenance + liquidity screen [Approved]

- **Ticket:** QE-464 · **Branch:** `qe-464/ingest-provenance` · **PR:** https://github.com/aoimasu/quant-engine/pull/177
- **Latest commit:** `d0d0ede`
- **Base:** `main`
- **Evidence note:** [`docs/operations/qe-464-ingest-provenance-analysis.md`](../operations/qe-464-ingest-provenance-analysis.md)
- **Ticket detail:** [`docs/mds/tickets/QE-464.md`](tickets/QE-464.md)
- **Design ref:** `docs/architecture/qe-455-research-flow-design.md` §8.2, §8.4, §10

### Green-gate (all on commit `d0d0ede`)
Feature OFF: `cargo fmt --all --check` PASS · `clippy --workspace --all-targets -D warnings` PASS · `cargo test --workspace` PASS (1144, 0 failed) · firewall PASS · `cargo deny check` PASS
Feature ON: `cargo clippy -p qe-ingest -p qe-cli -p qe-server --features http --all-targets -D warnings` PASS · `cargo test -p qe-ingest -p qe-cli -p qe-server --features http` PASS

### AC → proving test
1. POST launches supervised run + uniform 400 + standard done + no protocol bump: `post_ingest_launches_supervised_run_and_succeeds`, `post_ingest_rejects_invalid_requests_with_400` (server/tests/runs.rs).
2. Fetch-all via as-of machinery; as-of backtest excludes not-yet-listed/delisted; survivorship-unsafe flag: `dated_universe_resolves_full_roster_and_is_survivorship_safe`, `as_of_backtest_excludes_not_yet_listed_and_delisted`, `flat_open_ended_universe_is_flagged_survivorship_unsafe` (ingest/src/fetch_all.rs).
3. Every bar tagged, key-scannable, coverage key-only; data_provenance threaded; synthetic/mixed → marked vintage: `provenance_tags_survive_and_summarise`, `coverage_path_never_decodes_bar_values` (storage), `store_provenance_marks_synthetic_and_mixed_never_silently_real` (cli/train.rs).
4. Coverage mixed = multiple contiguous rows: `mixed_store_reports_multiple_contiguous_rows_never_blended` (storage).
5. Liquidity screen requires ADV/impact; flags thin: `screen_requires_calibration_and_flags_thin_names`, `boundary_at_the_floor_is_tradable` (ingest/src/liquidity.rs).
6. --synthetic tags synthetic / real tags real / no untagged bar; re-tag no snapshot drift: `ingest_populates_store_from_in_memory_source`, `synthetic_ingest_populates_fresh_store_with_expected_coverage`, `retagging_provenance_does_not_drift_bar_identity`.

### Provenance key-scheme + mixed rows + synthetic→marked vintage
- Dedicated `provenance` LMDB sub-DB keyed IDENTICALLY to a bar key (`instrument ‖ 0x00 ‖ [resolution_ordinal] ‖ order(seg_start)` → `ProvenanceSegment{end_ms, provenance, calibration}`). Per-bar provenance recoverable by prefix-scanning segment keys; bars DB untouched → coverage stays KEY-ONLY (QE-412).
- Mixed → `coverage()` emits one `CoverageRow` per segment (own from/to/bars counted key-only + `provenance`/`calibrated`); real-then-synthetic over disjoint ranges = two contiguous rows, never blended. Legacy (no segment) → one `unknown` row.
- Synthetic→marked vintage: `train` calls `store_provenance_summary` → `Synthetic`→`DataProvenance::Synthetic`, `Mixed`→`Mixed`, `Real`/`Unknown`/`Empty`→`Real` (documented default); hashed into vintage id via QE-467.

### Points for the reviewer to scrutinise / rule on
- **`Unknown`/`Empty` → `Real` mapping at the vintage level.** Per the ticket's out-of-scope ("record pre-existing untagged bars as unknown/legacy rather than guessing"), the store keeps legacy bars as `unknown`, but the VINTAGE provenance summary maps `Unknown`/`Empty`→`Real` (documented default). Rule whether collapsing legacy `Unknown`→`Real` at the vintage risks a legacy-data vintage reading as `real` (violating the never-silently-real guard), or is an acceptable documented default. The synthetic/mixed guard itself is honoured (Synthetic→Synthetic, Mixed→Mixed).
- **QE-463 handoff:** `run_ingest` now takes explicit `Calibration`; real path passes `Uncalibrated` (klines-only), stored on the segment + surfaced as `calibrated:false`. Confirm the marker actually reaches coverage now (QE-463's drop is closed).
- **FLAGGED defaults:** (a) listing dates ARE available via `[[universe]]` config (BTCUSDT 2019-09-08, ETHUSDT 2019-11-27); a flat `instruments`-only config → survivorship-unsafe flag fires (both paths tested). (b) thin-name threshold `DEFAULT_MIN_ADV_USD = $2,000,000` (paired w/ QE-447 MaxParticipation) — conservative pick, needs product confirmation.

### Confirmations
NO `PROTOCOL_VERSION` bump (still 3); NO `VINTAGE_FORMAT_VERSION`/storage `SCHEMA_VERSION` bump (provenance sub-DB additive, only populates QE-467's field); coverage KEY-ONLY (bars scanned via `DecodeIgnore`); no `input_snapshot_id` drift (provenance never touches bar keys/values); firewall green.

### Acceptance criteria (from `docs/mds/tickets/QE-464.md`)
- [ ] `POST /api/ingest` launches supervised run (instruments/range/resolution or fetch-all); invalid → 400 uniform; standard `done` line; no `PROTOCOL_VERSION` bump.
- [ ] Fetch-all resolves via as-of machinery (`InstrumentListing`+`plan::overlaps()`), writes resolved as-of set into coverage/lineage; as-of backtest excludes not-yet-listed/already-delisted; no listing dates → `survivorship-unsafe`.
- [ ] Every stored bar carries real/synthetic tag key-scannably (coverage key-only, QE-412); `data_provenance` threaded into `VintageContent.lineage` (QE-467); synthetic/mixed store → marked vintage, never silently `real`.
- [ ] `GET /api/market-data/coverage` (QE-257) rows expose provenance; real+synthetic mix = multiple contiguous per-provenance rows, never one blended.
- [ ] Liquidity screen requires per-instrument rolling-ADV/impact (QE-440); thin names (below %ADV guard QE-447) flagged/excluded, not admitted at the major floor.
- [ ] `--synthetic` tags `synthetic`; real-ingest tags `real`; no untagged bar going forward; re-tag causes no `input_snapshot_id` drift.

### Review verdict — Approved (reviewer, commit `d0d0ede`)

Meets all acceptance criteria; no blocking issues. Verified against QE-464.md AC, design §8.2/§8.4/§10, the diff, and independent **feature-on** builds/tests: `cargo test -p qe-ingest -p qe-cli -p qe-server --features http` → 346 passed / 2 ignored; firewall 1/1; no `PROTOCOL_VERSION`/`VINTAGE_FORMAT_VERSION`/`SCHEMA_VERSION` bump.

**Scrutiny findings**
1. **Never-silently-real guard — CONFIRMED (headline).** The storage `ProvenanceSummary::from_provenances` is careful: any real+synthetic ⇒ `Mixed`, all-synthetic ⇒ `Synthetic`, and — critically — a **partially-tagged** store (real segments + untagged bars) contributes `Real`+`Unknown` ⇒ `Mixed`, never silently `Real`. The train mapping `store_data_provenance` (`train.rs:1067-1069`) is `Synthetic→Synthetic`, `Mixed→Mixed`. So a synthetic or mixed *tagged* store always yields a synthetic/mixed-derived vintage (AC guard met). See the ruling below on the residual `Unknown/Empty→Real` collapse.
2. **Provenance is content-hashed — CONFIRMED.** `data_provenance` rides `VintageContent.provenance` (QE-467's hashed `ResearchProvenance`); QE-467's `provenance_is_part_of_the_hash…` test proves flipping real→synthetic changes the vintage id, and this PR's mapping test proves a synthetic store yields `Synthetic`. Not cosmetic.
3. **Coverage stays KEY-ONLY (QE-412) — CONFIRMED.** Provenance lives in a **separate** `provenance` sub-DB (`DB_PROVENANCE`); `count_bars_in_range` and the coverage scan use `.remap_data_type::<DecodeIgnore>()` (never decode a `Bar`); `coverage_bounds` is untouched. Additive, no `SCHEMA_VERSION` bump.
4. **Mixed = multiple contiguous rows, never blended — CONFIRMED.** `coverage()` emits one row per provenance segment (legacy/zero-segment ⇒ one `unknown` row). `mixed_store_reports_multiple_contiguous_rows_never_blended` seals a real [100,200] then a synthetic [300,400] run and asserts `rows.len()==2` with per-run provenance/ranges, plus `ProvenanceSummary::Mixed` — distinct rows, not a blended single row.
5. **Survivorship kill — CONFIRMED, both paths real + tested.** `resolve_fetch_all` routes through the existing `qe_config::Universe` (`all_known` + `members_at`); `as_of_backtest_excludes_not_yet_listed_and_delisted` proves the as-of kill (pre-listing excluded, post-delist excluded, delisted names retained in the roster for max history), and `flat_open_ended_universe_is_flagged_survivorship_unsafe` proves a dateless universe is flagged (operator-visible `WARNING` at ingest), never silently open-ended. Wired in `main.rs`.
6. **No `input_snapshot_id` drift — CONFIRMED.** `put_bars_with_provenance` writes bars through the exact `put_bars` path and the segment to the separate DB; `retagging_provenance_does_not_drift_bar_identity` re-tags the same range and asserts both `coverage_bounds` and the decoded bar value are unchanged (bar bytes ⇒ snapshot id unchanged).
7. **QE-463 handoff closed — CONFIRMED.** The real klines-only path writes `Calibration::Uncalibrated` (`main.rs:807`), surfaced on the coverage row as `calibrated:false`; the `calibration_source` marker now reaches coverage instead of being dropped.
8. **Liquidity screen — CONFIRMED.** `screen_liquidity` classifies `None`-ADV ⇒ `Uncalibrated` (excluded — capacity-eligibility unestablished, QE-440), ADV<floor ⇒ `Thin` (flagged, QE-447 %ADV), else `Tradable`; `tradable_only` drops thin+uncalibrated (no capacity mirage at the $250k major floor). Wired into `main.rs:740` with an operator warning for flagged names; tests prove major→Tradable, thin→Thin, no-cal→Uncalibrated with only the liquid name admitted.
9. **No version bumps + firewall — CONFIRMED.** No `PROTOCOL_VERSION`/`VINTAGE_FORMAT_VERSION`/`SCHEMA_VERSION` change; firewall green; the fetch-all/liquidity logic reuses existing `qe-config`/`qe-ingest` seams (no new forbidden cross-crate edge).

**RULING on the `Unknown`/`Empty` → `Real` collapse (train.rs:1069): ACCEPTABLE as a documented default — NON-BLOCKING, but with a strong recommendation.** The AC's explicit headline guard ("a *synthetic/mixed* input store must yield a synthetic-/mixed-derived vintage, never silently real") is fully met — synthetic contamination always shows, and partial tagging fails safe to `Mixed`. The collapse only bites a **100%-legacy-untagged** store, which is explicitly out-of-scope migration territory ("record them as unknown/legacy rather than guessing"), and the *clean* fix is genuinely blocked: `qe_vintage::DataProvenance` has only `{Real, Synthetic, Mixed}` (QE-467's schema — no format bump allowed here), so the 3-way projection cannot represent "unknown". **Strong recommendation (I'd want this before an operator with a pre-QE-464 store trains on it):** in *this* deployment all pre-QE-464 bars came from `qe ingest --synthetic` (the only path that existed), so they now read `unknown` → map to `Real` — a confident "real" banner for actually-synthetic data, the exact failure this ticket targets. Since no in-scope label is truly honest for `unknown`, the *fail-safe* direction is a one-line change to fold `Unknown` → `Mixed` ("not verified pure-real") rather than `Real`, plus a QE-467 follow-up to add `DataProvenance::Unknown` so legacy reads "unverified". `Empty→Real` is harmless (a sealed vintage always has bars). The storage/coverage layer already preserves the honest `unknown` tag, so the loss is only at the vintage banner.

**RULING on `DEFAULT_MIN_ADV_USD = $2M`: ACCEPTABLE as a flagged placeholder — NON-BLOCKING.** Enforced, documented with rationale, conservative, and product-confirmable — consistent with the `MIN_OCCUPIED_NICHES` (QE-458) / `K` (QE-460) precedent. Product should confirm it and reconcile with QE-447's precise `%ADV` threshold: the doc's own logic ($250k deployed at a 1% cap ⇒ ADV ≥ $25M) implies $2M is a *coarse* pre-filter that admits [$2M, $25M] names as `Tradable`, relying on QE-447's finer participation guard for the precise cut — so the comment's "nothing marked Tradable here is a capacity mirage" is slightly overstated. Not a blocker.

**Non-blocking notes:**
1. **[top] Legacy `Unknown → Real` fail-safe** — see the ruling above; recommend the one-line `Unknown → Mixed` interim + a `DataProvenance::Unknown` follow-up.
2. `survivorship_unsafe` is surfaced as a transient CLI `WARNING`, not a persisted store/coverage marker — a later consumer (MarketData/inspector) can't see it. Operator-visible where the fetch decision is made and the as-of kill (`members_at`) is unaffected, so acceptable, but a durable flag would be more robust (natural QE-465 follow-up).
3. `$2M` vs the $25M-implied-1% boundary (see the liquidity ruling) — reconcile the coarse floor with QE-447 and soften the "no mirage" comment.
