# ADR — Admin UI for training & backtesting (pre-Phase-3)

- **Date:** 2026-07-02
- **Status:** Accepted (design), pending implementation
- **Full spec:** [`docs/superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md`](../superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md)
- **Design source:** Claude Design "Quant Engine Design System"
  (`claude.ai/design/p/2b443e2a-c374-4c7c-8437-e0d253e4bc65`)

## Context

An authenticated admin UI to trigger, monitor, and review training and backtesting runs from a browser, to be
built **before Phase 3**. A key constraint surfaced during design: the training/backtest pipeline is **not yet
wired into any runnable command** — `qe-cli` today only has `version` and `train` (which writes a vintage
manifest, not a real run) — and there is **no async runtime or HTTP server** anywhere (the core is deliberately
synchronous, deterministic, and firewalled). So this is a stack of projects, not one, and it is decomposed into
sequenced specs (see the full spec §10).

## Decisions

| # | Decision | Rationale (short) |
|---|----------|-------------------|
| **D1** | **Backtest = run an existing sealed *vintage* over a window** (not parametric strategy config). | Strategies are evolved genomes sealed in a vintage; the backtester (`qe-wfo`) runs genomes, not hand-typed params. The design mock's `z_entry`/`lookback` are generic demo filler. Editable config becomes *evaluation* knobs (vintage, window, universe, resolution, costs); genome params render read-only. |
| **D2** | **Backtest first; training monitor is a later spec.** | Backtest is fast/deterministic/self-contained with existing machinery *and* an existing screen; training is hours-long, data-heavy, unwired, and has no screen. Build the platform against the cheap job first, then drop training into a proven harness. |
| **D3** | **Assume pre-ingested data; add a minimal `qe-cli ingest`; UI market-data view is read-only.** | Keep the backtest pure (reads the local LMDB store, no network in the hot path). Ingestion isn't wired yet, so a minimal `ingest` populates the store once (public data, no key). UI-triggered ingest deferred. |
| **D4a** | **New `qe-server` crate (axum + tokio), a second composition root** beside `qe-cli`. | All-Rust, reuses engine crates/types; async isolated to this crate. Depends only on training-side + shared crates in v1; **never `qe-runtime`** (Phase-3 live). Allowed by the QE-001/QE-132 guards (they forbid only `runtime ⊥ wfo/ensemble` edges, not a root touching the training side). |
| **D4b** | **Execution = server supervises `qe-cli` subprocesses** (JSON-line progress on stdout, artifacts to the run dir). | A long/heavy run can't destabilise the web server; the deterministic core stays an independently-runnable CLI (cron/CI-friendly); crash isolation is free. |
| **D4c** | **Run store = file-based** — `data/runs/<id>/{meta.json,result.json,stdout.log}` + an index. | Matches the existing file layout (LMDB store, JSON vintage manifests, `data/artifacts/`); no new DB dependency. SQLite only if query needs grow. |
| **D4d** | **Auth = Google Authorization-Code redirect flow → verify ID token → check `QE_ADMIN_ALLOWED_EMAILS` → HTTP-only signed session cookie; SPA + `/api` served same-origin.** | Standard, secure for an internal admin tool; same-origin avoids CORS and makes cookie auth trivial; single deploy. OAuth client id/secret + session secret via env. |

## Consequences

- Adds a Node/Vite frontend toolchain (under `web/`) and an async Rust server crate — both new to this repo,
  both isolated.
- `qe-server` becomes a second composition root. The current firewall/decoupling tests assert nothing about
  `qe-server` (it doesn't exist yet); **QE-254 extends the guard** to assert `qe-server` pulls in no
  `qe-runtime`/`qe-venue` edge in v1.
- A backtest is **not** a direct `scan_bars → backtest` call: `qe_wfo::backtest::backtest` consumes a *decision*
  bar (`FeatureVector` + price + funding), so a feature-engineering step (`qe_signal::feature::assemble_batch`,
  using the vintage's catalogue schema) sits between the OHLCV store read and the backtester. Captured in the
  v1 plan (QE-251 Task 5a) — it expands that ticket's scope.
- Delivery order: (1) runnable CLI jobs → (2) `qe-server` + auth → (3) React SPA → (4) training monitor.
- Live-trading surfaces (Dashboard/Positions/Orders/Risk order ticket), UI-triggered ingest, and any
  real-capital path remain **out of scope** (Phase 3).
