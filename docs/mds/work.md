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

---

## QE-106 — Multi-resolution bar reconstruction (batch) — PR #19 — [Ready-for-review]

- **Branch:** `qe-106/multi-resolution-bar-reconstruction`
- **PR:** https://github.com/aoimasu/quant-engine/pull/19
- **Latest commit:** `1ca1897`
- **Evidence/design:** `docs/architecture/qe-106-multi-resolution-bar-reconstruction-design.md`
- **Changed surface:** `crates/signal` (**new** `src/reconstruct.rs`, `lib.rs` wiring, `Cargo.toml`
  +`rust_decimal`/`thiserror`), `crates/ingest` (**new** `src/recon.rs` + `tests/recon.rs`, `lib.rs`
  wiring, `Cargo.toml` +`qe-signal`), `Cargo.lock`. No new third-party deps. Also bundles the QE-105
  archive (`docs/mds/reviewed/qe-105.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks
  direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Batch-reconstructed bars equal streaming reconstruction on the same input (parity fixture).

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow` — clean)
- `cargo test --workspace --locked` — **238 passed, 1 ignored** (reconstruct 6, recon-cache
  integration 2)
- `cargo test -p qe-cli --test dependency_topology` — passes (new `qe-ingest→qe-signal` +
  `qe-signal` staying `qe-domain`-only edges allowed; runtime↔training invariant untouched)
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps)

Key AC-proving tests:
- **AC (batch == streaming parity)** — `reconstruct::tests::batch_equals_streaming_parity`: a 70-min
  5m series fed as a batch vs one-at-a-time through `BarReconstructor` yields **identical** output
  across the three 30m windows. Batch is literally streaming over the whole slice (one shared fold),
  so parity is structural.
- **Supporting:** `rolls_up_one_window_to_hand_computed_values` (OHLCV+trades roll-up vs
  hand-computed), `windows_align_to_epoch_boundary_not_first_bar` (deterministic boundaries),
  `reconstruct_tiers_yields_all_configured_tiers` (48×5m → 8×30m + 1×4h), error cases
  (non-coarser target, wrong-resolution input).
- **Cache bridge** (`crates/ingest/tests/recon.rs`): `reconstruct_caches_tiers_and_round_trips_under_lineage`
  (reconstruct→cache→`scan_recon_bars`/`get_recon_bar` round-trip), `cached_tiers_are_stale_under_a_different_lineage`
  (lineage tagging honoured).

### Design notes for the reviewer
- **One fold, shared by batch + streaming.** `BarReconstructor` is the single incremental fold;
  `reconstruct_batch` = push-all + finish. So batch and streaming cannot diverge — the QE-206 parity
  guarantee is structural, not a coincidence of two implementations.
- **Storage-free hot-path logic.** `qe-signal` stays `qe-domain`-only (no LMDB) — reconstruction runs
  identically in batch and in the latency-sensitive runtime (QE-003). The synthetic-store caching is
  a separate `qe-ingest` bridge.
- **Deterministic boundaries.** Window start = `floor_div(open_time, target_ms)·target_ms` — depends
  only on the timestamp + target resolution, never on batch size / arrival order / thread count.
- **Topology.** New `qe-ingest → qe-signal` edge is acyclic (signal is `qe-domain`-only); QE-001
  guard re-runs green.
- **Out of scope:** live-runtime streaming wiring (QE-205). The streaming `BarReconstructor` is
  exposed now so QE-206 can prove parity against the live path.
