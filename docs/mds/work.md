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

---

## QE-107 — Indicator catalogue (quantised, deterministic, parity-ready) — PR #20 — [Ready-for-review]

- **Branch:** `qe-107/indicator-catalogue`
- **PR:** https://github.com/aoimasu/quant-engine/pull/20
- **Latest commit:** `5de015d`
- **Evidence/design:** `docs/architecture/qe-107-indicator-catalogue-design.md`
- **Changed surface:** `crates/signal` — **new** `src/indicator/{mod,quant,roll,price,flow}.rs`,
  `lib.rs` wiring, `Cargo.toml` (rust_decimal +`maths` feature, +`thiserror`), `Cargo.lock`. No new
  third-party crates (only the pure `maths` feature). Also bundles the QE-106 archive
  (`docs/mds/reviewed/qe-106.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks direct
  `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Each indicator's batch output equals its streaming output bar-for-bar.
- [x] Declared lookback matches actual data dependency (verified).

### Verification (run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean (also
  `cargo clippy -p qe-ingest --features arrow` — clean)
- `cargo test --workspace --locked` — **255 passed, 1 ignored** (qe-signal 24, incl. the generic AC
  tests over the whole catalogue)
- `cargo test -p qe-cli --test dependency_topology` — passes (`qe-signal` stays `qe-domain`-only)
- `cargo deny check` — advisories/bans/licenses/sources ok (no new crates; only the pure `maths`
  feature on the existing `rust_decimal`)

Key AC-proving tests (generic over the whole 22-indicator catalogue):
- **AC #1 (batch == streaming)** — `ac1_batch_equals_streaming_for_every_indicator`: for every
  indicator, `compute_batch` over a slice equals feeding the same samples one-at-a-time. Structural:
  there is one `update` path; batch is literally the streaming loop.
- **AC #2 (lookback == data dependency)** — proven from both sides:
  - `ac2_warmup_emits_none_until_exactly_lookback_then_some` — each indicator emits `None` until it
    has seen exactly `lookback` samples, then `Some` (consumes ≥ lookback).
  - `ac2_latest_output_independent_of_out_of_window_samples` — perturbing a sample at index
    `len-1-lookback` (just outside the latest window) leaves the latest state byte-identical (depends
    on ≤ lookback). Together ⇒ dependency == lookback.
- **Supporting:** `catalogue_has_at_least_twenty_indicators_with_unique_ids` (22, unique),
  `every_indicator_respects_configured_state_count`, hand-computed SMA/RSI/Stoch/ROC, quantiser bin
  edges, `Roll` stats, flow-factor scalar-skip + presence.

### Design notes for the reviewer
- **AC #1 is structural.** One `Indicator::update`; `compute_batch` = the streaming loop. Batch and
  streaming cannot diverge — same as the QE-106 reconstruction pattern.
- **AC #2 by FIR construction.** Every indicator's latest output reads **exactly the last `lookback`
  samples** via a ring buffer (`Roll`) — nothing older. So declared lookback == data dependency,
  which is the leakage-relevant property purge/embargo (QE-128/WFO) needs. The catalogue ships
  finite-window variants (Cutler RSI, simple-mean ATR, windowed EMA) **on purpose** so this holds
  strictly; IIR smoothing could be added later behind a declared embargo-aware lookback.
- **Quantisation is point-wise.** `Quantiser::{Linear,Bands}` map a value → state with no rolling
  quantile / dataset-wide fit, so the discrete state never peeks at future data and is identical
  batch vs streaming. `num_states` is configurable via `CatalogueConfig`.
- **Storage-free hot-path crate.** `qe-signal` stays `qe-domain`-only; `rust_decimal`'s pure `maths`
  feature adds `Decimal::sqrt` (std-dev/Bollinger) with no new crates, so `cargo deny` is unaffected.
- **Out of scope:** feature assembly/normalisation (QE-108); genome (QE-110).
