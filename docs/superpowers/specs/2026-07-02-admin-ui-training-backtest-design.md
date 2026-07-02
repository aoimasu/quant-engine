# Admin UI for training & backtesting — design spec

- **Date:** 2026-07-02
- **Status:** Draft — awaiting review
- **Author:** brainstormed with Claude Code
- **Scope marker:** pre-Phase-3 (built before QE-301+; does not touch live capital)
- **Design source:** Claude Design "Quant Engine Design System"
  (`claude.ai/design/p/2b443e2a-c374-4c7c-8437-e0d253e4bc65`)

## 1. Summary

An authenticated admin UI to **trigger, monitor, and review** the engine's training and backtesting runs
from a browser. A quant researcher signs in with Google (gated by an email allowlist), starts a backtest of a
sealed vintage over a chosen window/universe/cost model, watches progress, and reviews results (metrics,
equity/drawdown curves, monthly-returns heatmap, trades). Training-run monitoring and the live-trading
surfaces follow in later specs.

This is not a single project but a **stack of three**, delivered in sequence. This spec covers the whole
architecture and details **v1 (the backtest vertical)**; later sections outline the follow-on specs.

## 2. Goals / non-goals

**Goals (v1)**
- Trigger a **backtest** of an existing sealed vintage from the UI, watch progress, and review full results.
- A one-time **ingest** path so the market-data store is populated and backtests have real data to run against.
- **Google OAuth** sign-in gated by an **env email allowlist**; nothing is reachable unauthenticated.
- Reuse the existing engine crates and the Claude Design design system faithfully.

**Non-goals (v1 — explicitly deferred)**
- Training-run monitoring (WFO/MAP-Elites search progress, CV folds, G1 gate) — **spec 4**, fast-follow.
- Live-trading surfaces (Dashboard / Positions / Orders / Risk order-ticket) — **Phase 3**, out of scope.
- Triggering ingestion *from the UI* — v1 ships a **read-only** market-data coverage view; UI-driven ingest
  is a later add.
- Parametric hand-tuned strategies — the engine has no such path (see decision D1).

## 3. Decisions (the four settled questions)

> These are the load-bearing choices this spec commits to. Recorded also in
> `docs/architecture/admin-ui-decisions.md` and `docs/current-state.html`.

### D1 — Backtest semantics: **backtest an existing vintage over a window** (not parametric config)
The engine has no parametric-strategy path. Strategies are **evolved genomes** discovered by the WFO/MAP-Elites
search and sealed into a **vintage**; the backtester (`qe-wfo`) runs genomes, not hand-typed parameters. The
design mock's `z_entry=1.8` / `lookback=48h` are generic demo filler (its README states the brand was invented
to brief, with no codebase supplied).

**Consequence.** A "backtest run" takes *(vintage id [+ optional single strategy within it], window, universe,
resolution, cost/slippage model)*. The genome's own parameters render **read-only** as header tags (exactly
where the design places them). The editable "config"/right-rail fields become **evaluation** knobs, not
strategy knobs.

### D2 — Sequencing: **backtest first; training monitor is a later spec**
Backtest is fast, deterministic, self-contained, and both its machinery (`qe-wfo` backtester) and its **screen**
already exist in the design. Training is the opposite: hours-long, data-heavy, unwired, and has **no screen**
designed. Building the whole platform (auth, orchestration, run store, UI shell) against the cheap job first
de-risks everything; the training job then drops into a proven harness. Training is **spec 4**, not v1.

### D3 — Market data: **assume pre-ingested; add a minimal `ingest` command; UI coverage is read-only**
Backtests read the **local LMDB store** — no network in the hot path (matches the engine's ingest→store→backtest
separation and its determinism ethos). Because ingestion is not yet wired into any command, v1 adds a minimal
runnable `qe-cli ingest` to populate the store once (public Binance historical data, no API key). The UI's
**Market data** surface is a **read-only coverage view** (symbols × ranges present) so the user can only pick
valid windows. UI-triggered ingest is deferred.

### D4 — Infrastructure bundle
- **(a) `qe-server` crate — axum + tokio — a second composition root** alongside `qe-cli`. All-Rust; reuses the
  engine crates and types directly; async is isolated to this one crate (the deterministic core stays sync). It
  depends only on training-side + shared crates in v1 and **never on `qe-runtime`** (that is Phase-3 live).
- **(b) Execution model: the server supervises `qe-cli` subprocesses.** New `qe-cli backtest / ingest`
  subcommands emit **JSON-line progress** on stdout and write result artifacts to the run directory; the server
  spawns, supervises, tails progress, and serves the artifacts. Rationale: a long/heavy run cannot destabilise
  the web server; the deterministic core stays a clean, independently-runnable CLI (also usable from cron/CI);
  crash isolation is free.
- **(c) Run store: file-based.** `data/runs/<run_id>/meta.json` (status/params/progress/timestamps) + result
  artifacts, with a lightweight index. Matches the existing file layout (LMDB market store, JSON vintage
  manifests, `data/artifacts/`); no new database dependency. SQLite only if query needs grow.
- **(d) Auth: Google Authorization-Code redirect flow** → verify the Google ID token → check the email against
  `QE_ADMIN_ALLOWED_EMAILS` (comma-separated) → establish an **HTTP-only signed session cookie**. The server
  serves the built SPA **and** the `/api` routes **same-origin** (no CORS; cookie auth just works; single
  deploy). A Google OAuth client id/secret is supplied via env.

## 4. Architecture

```
qe-cli ingest   → populates the LMDB market store (one-time, public data, no key)
qe-cli backtest <vintage> --window --universe --costs --json
        │  JSON-line progress on stdout + writes result artifacts to the run dir
        │  (spawned & supervised by)
qe-server (axum + tokio, second composition root)
        │  /api : trigger runs · read run status/results · market-data coverage · auth
        │  run store: data/runs/<id>/{meta.json, result.json, artifacts…}
        │  auth:      Google OAuth (redirect) + QE_ADMIN_ALLOWED_EMAILS + signed cookie
        │  serves the built SPA at /  (same origin as /api)
        ▼
React SPA (ported design system)
        Login · Backtests (list) · Backtest result · Market-data coverage
        (Training monitor + Trade/Risk surfaces = later specs)
```

**Crate dependency posture (v1).** `qe-server → {qe-config, qe-domain, qe-signal, qe-storage, qe-vintage,
qe-wfo, qe-validation, qe-gate, qe-report, qe-determinism, qe-telemetry}`. It is a composition root like
`qe-cli`; the QE-001 decoupling and QE-132 firewall guards forbid only specific edges
(`qe-runtime ⊥ wfo/ensemble`; `wfo/ensemble ⇏ runtime/venue`) and place **no** constraint on a root crate
touching the training side. v1 does not depend on `qe-runtime`/`qe-venue` at all.

## 5. Component 1 — CLI jobs (`qe-cli`)

Two new subcommands wire existing libraries into runnable jobs. Both are **deterministic** and **offline**
except `ingest` (which reads public historical data).

### 5.1 `qe-cli ingest`
- **Purpose.** Populate the LMDB market store for a universe + window so backtests have data (D3).
- **Inputs.** `--config`, `--universe` (or from config), `--start`, `--end`, `--resolution`.
- **Behaviour.** Uses the existing ingest/fusion + storage libraries (`qe-ingest`, `qe-storage`) to fetch
  public klines/funding/OI/premium (behind the default-off `http` feature), reconcile, and persist to LMDB.
- **Progress.** JSON-line records on stdout (see 5.3) so the future UI ingest could stream it.
- **Out of scope for the *design*:** the concrete Binance decoders are the same injectable seam the bootstrap
  already leaves open; this spec assumes they are supplied/enabled behind `http`.

### 5.2 `qe-cli backtest`
- **Purpose.** Run a sealed vintage (or one strategy within it) over a window and produce the full result
  contract the UI consumes (D1).
- **Inputs.** `--vintage <id>` (required), `--strategy <chromosome-id>` (optional; default = the vintage's
  ensemble), `--start`, `--end`, `--resolution`, `--universe` (default = the vintage's roster), cost knobs
  `--taker-fee-bps`, `--slippage-model`, and `--json` (emit progress + write artifacts).
- **Behaviour.** Load the vintage read-only → replay the window from the LMDB store through the shared evaluator
  / backtester (`qe-wfo` friction-true backtest: fees + funding + slippage) → compute metrics and the equity /
  drawdown / monthly / trades outputs (via `qe-report` / `qe-validation`) → write `result.json` (+ any large
  arrays as sidecar artifacts) into the run dir.
- **Determinism.** Single-threaded, pull-based, no wall-clock in outputs — same inputs ⇒ identical result.

### 5.3 Progress protocol (stdout, one JSON object per line)
```json
{"t":"progress","pct":64,"stage":"simulate","msg":"Simulating 2021-01-01 → 2024-12-31…"}
{"t":"progress","pct":100,"stage":"report","msg":"Scoring"}
{"t":"done","result":"result.json"}
{"t":"error","msg":"…"}
```
The server reads these to update `meta.json`. `pct` + `msg` map 1:1 to the design's progress bar + status line.

## 6. Component 2 — backend service (`qe-server`)

### 6.1 Run store (file-based, D4c)
```
data/runs/
  index.json                       # append-only list: [{id, type, status, created_ms, label}]
  <run_id>/
    meta.json                      # {id, type, status, params, progress{pct,stage,msg},
                                    #  created_ms, started_ms, finished_ms, exit, artifacts[]}
    result.json                    # the result contract (§8) once succeeded
    stdout.log                     # captured subprocess output (progress lines + any logs)
```
`run_id` is a content-ish id (e.g. ULID-like from a passed-in seed/counter — no wall-clock in the id itself).
Status: `queued → running → succeeded | failed`.

### 6.2 API (all under `/api`, all require a valid session except the auth routes)
| Method & path | Purpose |
|---|---|
| `GET  /api/auth/login` | Begin Google OAuth (redirect to Google). |
| `GET  /api/auth/callback` | OAuth callback: verify id-token, check allowlist, set session cookie. |
| `POST /api/auth/logout` | Clear session. |
| `GET  /api/me` | Current user (email) or 401. |
| `GET  /api/vintages` | List sealed vintages available to backtest (from the artifacts dir). |
| `GET  /api/market-data/coverage` | Read-only: symbols × ranges present in the LMDB store (D3). |
| `POST /api/runs` | Create+start a run `{type:"backtest", params:{…}}` → spawns the CLI subprocess. |
| `GET  /api/runs` | List runs (from `index.json`), newest first. |
| `GET  /api/runs/:id` | One run's `meta.json` (status + progress). |
| `GET  /api/runs/:id/result` | The run's `result.json` once succeeded. |
| `GET  /api/runs/:id/stream` | Optional: SSE that tails progress (else the UI polls `GET /api/runs/:id`). |

**Concurrency.** A small bounded worker pool supervises subprocesses; excess requests queue (`status:queued`).
A crashed/nonzero subprocess ⇒ `status:failed` with the captured stderr tail in `meta.json`.

### 6.3 Auth (D4d)
- **Flow.** Authorization-Code: `/api/auth/login` → Google consent → `/api/auth/callback?code=…` → exchange for
  tokens → verify the Google **ID token** (signature, `aud`, `iss`, expiry) → extract `email` +
  `email_verified`.
- **Gate.** `email_verified == true` **and** `email ∈ QE_ADMIN_ALLOWED_EMAILS` (comma-separated, trimmed,
  case-insensitive). Otherwise 403 — a valid Google login that isn't allowlisted is rejected.
- **Session.** HTTP-only, `Secure`, `SameSite=Lax` signed cookie carrying the email + expiry; verified on every
  `/api` call. Signing key + OAuth `client_id`/`client_secret`/`redirect_uri` via env.
- **Static SPA.** Served by the same server at `/`; unauthenticated app loads render the login screen, which
  hits `/api/auth/login`.

### 6.4 Config (env)
`QE_ADMIN_ALLOWED_EMAILS`, `QE_OAUTH_GOOGLE_CLIENT_ID`, `QE_OAUTH_GOOGLE_CLIENT_SECRET`,
`QE_OAUTH_REDIRECT_URI`, `QE_SESSION_SECRET`, `QE_SERVER_ADDR`, `QE_DATA_DIR` (where `runs/`, the LMDB store,
and `artifacts/` live). All layered through the existing `qe-config` conventions where practical.

## 7. Component 3 — frontend (React SPA)

### 7.1 Design-system port
The Claude Design project is a full design system: CSS **tokens** (colors/typography/spacing/effects), ~23
**primitives** (`Button`, `Input`, `Select`, `DataTable`, `StatTile`, `Pnl`, `Badge`, `Tag`, `Card`, `Tabs`,
`Callout`, `Toast`, `Icon`, `Avatar`, …), and **UI kits** (`AppShell`, `BacktestResearch`, …). We stand up a
**Vite + React** app and port the tokens + primitives + the two relevant kits, keeping the dark-first violet
system faithfully (Space Grotesk / Hanken Grotesk / JetBrains Mono; Lucide icons). The `design-sync` skill can
keep the local component library in sync with the Claude Design project.

### 7.2 Screens (v1)
- **Login** — net-new (no login screen exists in the kit); built from the system: brand lockup, "Sign in with
  Google" button, allowlist-rejection state.
- **App shell** — port `AppShell.jsx`. v1 shows only the **Research** nav group active (Strategies · Backtests ·
  Market data); Trade/Risk items are present-but-disabled placeholders (Phase 3).
- **Backtests (list)** — runs table (from `GET /api/runs`): id, vintage/label, window, status badge, key
  metrics, created — click a row → result. A **"New backtest"** action opens the trigger form.
- **New backtest (trigger)** — form built from the right-rail/config fields (D1 knobs): vintage select, window
  (start/end), resolution, universe (from the vintage roster), taker-fee bps, slippage model → `POST /api/runs`.
- **Backtest result** — port `BacktestResearch.jsx`, data-driven from `GET /api/runs/:id/result`: header
  (name + read-only genome param tags + status), the **progress card** while `status:running` (polling
  `GET /api/runs/:id`), the 6-metric strip, Overview (equity/drawdown/heatmap), Trades table, Config (read-only
  eval params). "Re-run" clones params into a new run.
- **Market data (coverage)** — read-only table/heatmap of symbols × date ranges present (`GET
  /api/market-data/coverage`), so window pickers can validate.

### 7.3 Data mapping (design → API)
The backtest result screen defines the exact contract; §8 is the schema the CLI must emit and the API serves.

## 8. Data contracts

### 8.1 Backtest result (`result.json`)
```jsonc
{
  "strategy": { "name": "…", "status": "sealed|deployed", "tags": ["crypto","perp"],
                "params": { "…": "…" } },          // read-only genome params (header tags)
  "window":   { "start": "2021-01-01", "end": "2024-12-31", "resolution": "1h" },
  "universe": { "symbols": ["BTC-PERP","ETH-PERP","…"], "count": 41 },
  "costs":    { "taker_fee_bps": 2.0, "slippage_model": "square-root-impact" },
  "metrics":  { "cagr": 0.412, "sharpe": 2.14, "sortino": 3.08,
                "max_dd": -0.083, "win_rate": 0.582, "profit_factor": 1.94 },
  "equity_curve": [ /* float, for the log area chart */ ],
  "drawdown":     [ /* float ≤ 0 */ ],
  "monthly_returns": [ { "year": 2021, "months": [/* 12 floats, % */] }, … ],  // heatmap
  "trades": [ { "id":"#2041","symbol":"BTC-PERP","side":"LONG",
                "entry":"61,204","exit":"63,180","hold":"4d 6h",
                "return_pct":3.23,"result":"WIN" }, … ]
}
```
Field provenance (backtester / `qe-report` / `qe-validation`) is confirmed during implementation; any metric
the engine doesn't already emit is computed in the `report` stage of `qe-cli backtest`, not invented in the UI.

### 8.2 Run meta (`meta.json`) & progress — see §6.1 / §5.3.

## 9. Testing strategy

- **CLI jobs:** unit/prove-it tests that a `backtest` over a fixture vintage + a small committed sample store
  produces a deterministic `result.json` matching a golden file; the JSON-line progress ends with `done`.
- **qe-server:** handler tests for the run lifecycle (create → running → succeeded/failed), the run-store
  read/write, and **auth gating** (no session ⇒ 401; valid-but-not-allowlisted ⇒ 403; allowlisted ⇒ 200) with
  a mocked OAuth verifier. Subprocess supervision tested against a stub job binary.
- **Firewall:** the existing QE-132 firewall + QE-001 decoupling tests must stay green after adding `qe-server`
  (assert `qe-server` pulls in no forbidden edge; it must not reach `qe-runtime`/`qe-venue` in v1).
- **Frontend:** component/render checks for the ported primitives; a smoke test that the result screen renders
  the full contract; the design system's own `@dsCard` render-checks where applicable.
- **Green gate:** the repo's standard gate (`fmt` / `clippy -D warnings` / `test --workspace` / firewall /
  `deny`) must pass; the frontend adds its own lint/build/test.

## 10. Decomposition & sequencing

Each sub-project gets its own spec → plan → implementation cycle. This document is the program spec + v1 detail.

1. **Spec 1 — runnable jobs (this spec, v1 core):** `qe-cli ingest` + `qe-cli backtest` with the progress
   protocol and the result contract (§5, §8). Deliverable: a backtest runnable from the terminal producing
   `result.json`.
2. **Spec 2 — backend + auth:** `qe-server` (run store, API, Google OAuth + allowlist, subprocess supervision,
   static-SPA serving) (§6).
3. **Spec 3 — frontend:** the Vite/React SPA (design-system port + the four v1 screens) wired to the API (§7).
4. **Spec 4 — training monitor (fast-follow):** wire `qe-cli train` into a real runnable search job with rich
   progress (generations, MAP-Elites archive coverage, CV folds, G1 gate), plus the net-new training-monitor
   screen. Reuses the spec-2 orchestration.

**Later / Phase-3 (out of scope here):** live Trade/Positions/Orders/Risk surfaces (need `qe-runtime`);
UI-triggered ingestion; multi-user roles.

## 11. Risks & open questions

- **Metric provenance.** The design's six metrics + trades/equity/heatmap must be produced by the
  backtester/report layer; if any aren't currently emitted, they're computed in the `backtest` report stage.
  Confirm exact provenance when writing spec 1's plan.
- **Data availability.** A meaningful backtest needs real data in the store; `ingest` (behind `http`) must be
  functional. For tests, a small **committed sample store fixture** avoids network.
- **Async in an all-sync codebase.** tokio/axum are new; contained entirely within `qe-server`. The
  deterministic core remains sync and is only invoked as a subprocess.
- **Frontend build in a Rust repo.** Adds a Node toolchain (Vite) for the SPA; kept under a `web/` directory
  and built into static assets the server embeds/serves. Confirm this is acceptable vs a separate repo.
- **Vintage availability.** v1 assumes at least one sealed vintage exists to backtest; producing one is the
  domain of the (deferred) real `train` job — for v1 a fixture/sample vintage may be needed to demo.

## 12. Out of scope (restated)

Live-trading surfaces and order submission (Phase 3), training-run monitoring (spec 4), UI-triggered ingest,
parametric hand-tuned strategies, multi-tenant/roles, and any real-capital path.
