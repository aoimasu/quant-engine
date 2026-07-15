# Team improvement / refactor review — agreed tickets (2026-07-15)

A four-discipline review of the codebase (Senior Frontend, Senior Backend, Trading Expert,
Architect) to surface what can be **improved or refactored**. Each specialist analysed
read-only and produced evidence-backed proposals; this document is the **facilitated
synthesis** — overlaps merged, priorities reconciled, sequencing agreed, and cross-team
contracts single-sourced.

- **Scope:** cross-cutting hardening / correctness / tech-debt surfaced by review. Not spec
  features. The two decoupled pipelines and the P0–P2 gates are unchanged.
- **Numbering:** proposed **QE-4xx "review / cross-cutting hardening" band** (the phase bands
  1xx–3xx and the PreP3 25x/26x band are full). Several items **extend existing open tickets**
  (QE-262/263/265/266) rather than duplicating them — noted per ticket. Final IDs/placement are
  the maintainer's call.
- **Priority tags** (house convention): **P1** correctness/safety, do before trusting output or
  approaching live capital · **P2** before wider exposure/load · **P3** opportunistic quality.

## Discussion outcomes (what the four agreed)

1. **The run protocol is defined three times** and drifts silently: `qe-cli` emit
   (`crates/cli/src/jobs/mod.rs:22`), `qe-server` parse
   (`crates/server/src/runs/manager.rs:197`), SPA (`web/src/api/runs.ts:88`). Backend, Architect
   and Frontend all flagged their copy. **Agreed:** one contract — a dependency-free
   `qe-run-protocol` leaf crate + a `protocol_version` + a round-trip agreement test, with the TS
   types regenerated/mirrored from it. → **QE-406**.
2. **Catalogue identity is not pinned in the vintage** (QT-2) is the same defect as the already-open
   **QE-262**, and the Architect's schema-registry finding (AR-7) is the umbrella it belongs under.
   **Agreed:** one ticket that delivers QE-262 *and* the load-boundary version discipline. → **QE-402**.
3. **Server run-lifecycle robustness**: graceful shutdown + a supervised-task registry (BE-1),
   "job said done but wrote no result" ⇒ `failed` (BE-5), and taking blocking `std::fs` off the
   executor (BE-2) are the shutdown/startup/IO thirds of one story that **extend QE-263/QE-266**.
   **Agreed:** consolidate as QE-407 (lifecycle) + QE-411 (async IO), explicitly widening the two
   open tickets.
4. **CI has never compiled the feature-gated code** (`http`/`arrow`) — including the real Google
   token verifier and venue transport (AR-1). The panic-freedom / no-unwrap gates the project
   markets do not cover it. Ranked **P1 by unanimous consent** — it is the gate that makes every
   other safety gate real. → **QE-404**.
5. **Sequencing conflict resolved:** splitting the `qe-runtime` god-crate (AR-8, QE-426) churns the
   exact modules the trading-runtime fixes touch (QT-1 breaker seed, QT-7 mark EMA, QT-8 gross cap).
   **Agreed:** land the correctness fixes on the current structure first; do the crate split after,
   as a pure move. QE-426 is explicitly **blocked by** QE-401/417/418.
6. **No genuine disagreements on findings** — all conflicts were duplicate-coverage or ordering.
   The trading P1s (QE-401, QE-403) and QE-402/404/405 are the "before live capital / before
   trusting output" set and should lead.

---

## P1 — correctness / safety (lead the queue)

### QE-401 — Seed the live drawdown breaker with the reconstructed committed-peak equity
`Phase: P2/P3` · `Area: trading / runtime-risk` · `Depends on: QE-210, QE-211, QE-212` · `Effort: M`

**Why.** `boot_state.rs` computes a *true all-time* `committed_peak_equity` precisely because "a
windowed peak would under-anchor the drawdown and mis-fire the breaker"
(`crates/runtime/src/boot_state.rs:10`) — but **nothing consumes it**. `CircuitBreaker` only sets
`peak` to `None` on reset or to observed equity on `observe()`
(`crates/risk/src/breaker.rs:124,148`); there is no seed API, and `BreakerLayer` constructs breakers
with `peak: None` (`crates/runtime/src/live_breakers.rs:63-123`). After every bootstrap/restart the
DD breaker re-anchors on the first live tick, so a book already 20% below its historical peak reports
~0 drawdown and stays silent — defeating the exact mis-anchoring the reconstructed peak exists to
prevent, on the capital-loss path.

**Scope / requirements.**
- Add `CircuitBreaker::with_seed_peak(peak)` and a `BreakerLayer` constructor/param that pre-loads
  the all-time peak.
- Wire `ReconstructedState.strategies[i].committed_peak_equity` into each per-strategy breaker at
  cutover; seed the ensemble breaker from the aggregate committed peak.
- Preserve the seed across `reset()`/rollover unless a genuinely higher peak is observed.

**Out of scope.** Fast-drop window seeding (speed tier is inherently windowed); the live equity feed
(QE-217).

**Acceptance criteria.**
- Restart test: history peaks then declines 15%; after cold-start the first live tick at the declined
  level reports drawdown ≈ 15% (not ≈ 0) and trips the med tier at threshold.
- Seeded peak equals `CommittedPeak::from_series` of the replayed equity path, bit-for-bit.

**Cross-domain.** Cockpit (QE-304) should surface the seeded peak. Related to QE-416 (calibration at seal).

---

### QE-402 — Pin catalogue identity in the vintage & assert exact schema match on load (extends QE-262)
`Phase: PreP3` · `Area: trading / reproducibility` · `Depends on: QE-129, QE-260` · `Effort: S–M`

**Why.** The vintage persists neither `CATALOGUE_VERSION` nor `states`; the schema is rebuilt from
`CatalogueConfig::default()` at load and `check_schema` only **bounds-checks** clause indices
(`crates/cli/src/jobs/features.rs:14-22,52-63`). A catalogue reorder or a same-width version bump is
undetectable — a sealed genome's `feature = 7` silently addresses a *different indicator* at
backtest/live time → wrong signals, wrong PnL, broken reproducibility guarantee. This is QE-262 (P1,
still open). The Architect (AR-7) notes it is one instance of a broader gap: `VINTAGE_FORMAT_VERSION`,
`CATALOGUE_VERSION`, genome `REP_VERSION`, and the LMDB store schema are versioned independently with
no compatibility assertion at load seams.

**Scope / requirements.**
- Persist `catalogue_version` + `states` (and an ordered indicator-id list hash) inside
  `VintageContent`; bump `VINTAGE_FORMAT_VERSION`.
- On load/backtest/live, assert an **exact** schema match, not just bounds; fail closed on mismatch
  with a loud `SchemaMismatch`.
- Introduce one small artefact-schema module enumerating every persisted format version and the load
  boundary that must assert it (vintage↔catalogue, vintage↔genome rep, market/synthetic store).

**Out of scope.** Migration tooling for old vintages; redesigning the catalogue.

**Acceptance criteria.**
- Loading a vintage sealed under catalogue vN against a build at vN+1 (same shape) returns a hard error.
- Reordering two indicators changes the persisted catalogue hash and is rejected on load (test).
- Every persisted artefact type has a declared version asserted at its load boundary.

**Cross-domain.** Backend read APIs (QE-257/264) should expose the catalogue version. Supersedes QE-262.

---

### QE-403 — Enforce net-of-cost truth: funding-coverage gate + non-zero size-impact in selection
`Phase: P1` · `Area: trading / net-of-cost (QE-109)` · `Depends on: QE-103, QE-109, QE-128` · `Effort: M`

**Why.** Funding accrues only on bars where `funding_rate` is `Some` (`crates/wfo/src/backtest.rs:224`),
attached by exact open-time equality with no coverage check
(`crates/cli/src/jobs/features.rs:78-110`), and the train job never asserts funding is present/complete
(`crates/cli/src/jobs/train.rs:191-208`). A sparse/empty funding series (a common ingest gap) means every
strategy is selected, DSR/SPA-assessed, and G1-gated on **funding-free** returns — exactly the
funding-negative trend strategies QE-109 exists to reject. Separately the slippage size-impact
coefficient defaults to `ZERO` (`crates/wfo/src/friction.rs:73`), so selection ignores size-dependent
slippage; large-`size_bps`, high-turnover genomes pay only a fixed half-spread.

**Scope / requirements.**
- Require funding coverage over the training window (assert a minimum fraction of expected 8h stamps);
  fail or loudly flag otherwise.
- Record realised funding as a fraction of net PnL in the result sidecar so a funding-free run is visible.
- Set a non-zero default impact coefficient (or require it) for selection friction, or route selection
  through the QE-128 capacity/impact model.

**Out of scope.** The historical funding downloader (QE-102).

**Acceptance criteria.**
- A run over a window with no funding data errors or emits an explicit "funding coverage 0%" gate
  failure rather than sealing.
- A high-turnover genome's fitness strictly drops when impact > 0 (test).

**Cross-domain.** Depends on ingest funding coverage (QE-103); interacts with capacity (QE-128).

---

### QE-404 — CI must build, lint and test the feature-gated code (`http` / `arrow`)
`Phase: cross-cutting` · `Area: architecture / ci-gates` · `Depends on: —` · `Effort: S`

**Why.** `ci.yml` runs clippy/test `--workspace --all-targets --locked` but **never `--all-features`
or `--features http`**. Every crate parks real I/O behind a default-off feature: `qe-venue`/`qe-ingest`/
`qe-cli` `http` (REST fetchers), `qe-ingest` `arrow` (QE-104 artefact serialisation — the actual
training output), and `qe-server` `http` (the **real Google ID-token verifier**,
`crates/server/src/auth/google.rs`). So `fmt`, `clippy -D warnings`, `clippy::unwrap_used = "deny"`,
and `cargo test` have never seen the live-network or artefact-emission code — the most safety-relevant
code is excluded from the panic-freedom gates the project markets.

**Scope / requirements.**
- Add a CI matrix that builds + clippies + tests each feature-gated crate with its feature on
  (`--features http` for `qe-venue`/`qe-ingest`/`qe-server`/`qe-cli`; `--features arrow` for
  `qe-ingest`), still `-D warnings`, with `unwrap_used`/`unsafe_code` denies applying.
- Keep the offline default job as the fast path; the feature job may run network-free unit tests only
  (compile + lint is the main win).

**Out of scope.** New live-network integration tests; real Binance calls.

**Acceptance criteria.**
- A bare `unwrap()` or a warning inside any `#[cfg(feature="http")]`/`arrow` block fails CI.
- `cargo build --workspace --all-features --locked` is green in CI.

**Cross-domain.** Backend (auth verifier), Trading (venue/ingest transport lint-covered for the first time).

---

### QE-405 — Extend the firewall guard: `qe-runtime` / `qe-vintage` must not depend on the training crates
`Phase: cross-cutting` · `Area: architecture / firewall` · `Depends on: QE-132` · `Effort: S`

**Why.** `firewall_rules()` (`crates/architecture/src/lib.rs:202-217`) constrains `qe-wfo`,
`qe-ensemble` and `qe-server`, but **has no rule stopping `qe-runtime`/`qe-vintage` from depending on
`qe-wfo`/`qe-ensemble`**. The train/live decoupling is asserted only in prose and Cargo comments
(`crates/runtime/Cargo.toml`, `crates/vintage/Cargo.toml`). An engineer adding `qe-wfo.workspace = true`
to `qe-runtime` — pulling the whole search tree (and `rayon`) into the live binary — passes
`cargo test --workspace`. The invariant most load-bearing for live determinism/footprint is the one the
executable guard omits.

**Scope / requirements.**
- Add rules `qe-runtime ⊬ {qe-wfo, qe-ensemble}` and `qe-vintage ⊬ {qe-wfo, qe-ensemble}` (the vintage
  reaches genome logic only via `qe-signal`).
- Add a non-vacuity assertion in `tests/firewall.rs` (mirroring the `qe-runtime → qe-venue` check) that
  `qe-vintage → qe-signal` is parsed.
- Document that `qe-cli` is the only crate legitimately linking both sides, and that the firewall is a
  library-level (not process-level) guarantee.

**Out of scope.** Process isolation of the CLI; changing existing rules.

**Acceptance criteria.** Adding `qe-wfo` to `qe-runtime`'s deps fails `cargo test --workspace`; the clean
workspace still passes.

**Cross-domain.** Trading (keeps the live binary free of the search tree).

---

### QE-406 — Single-source the CLI ↔ server ↔ SPA run protocol (`qe-run-protocol` + version + agreement test)
`Phase: PreP3` · `Area: architecture / composition-roots + backend + frontend` · `Depends on: QE-255, QE-261` · `Effort: M`

**Why (three copies, no shared schema).** The JSON-line progress protocol and run-param DTOs are defined
independently in three places: emit `qe_cli::jobs::ProgressLine` (`crates/cli/src/jobs/mod.rs:22`), parse
`qe_server::runs::manager::ProgressLine` with `Option<f64>` fields
(`crates/server/src/runs/manager.rs:197`), and the SPA (`web/src/api/runs.ts:88`, where `RunMeta.type` is
`string` and `params` is always `BacktestParams` — a train run is statically mistyped). The server also
re-declares param defaults (`crates/server/src/runs/model.rs:38-100`). Avoiding a `qe-server → qe-cli`
dependency is correct (firewall), but the result is an **unversioned contract with no agreement test**: a
field rename or new tag breaks live monitoring and the SPA with zero compile-time or CI signal.

**Scope / requirements.**
- Extract the wire types (progress lines + run-param DTOs) into a dependency-free `qe-run-protocol` leaf
  crate (serde only, no engine deps, firewall-neutral); both `qe-cli` and `qe-server` depend on it; delete
  the two Rust copies. Preserve the server's tolerance of non-finite floats-as-null on the shared type.
- Add a `protocol_version` field on the handshake/terminal line; the server checks it.
- Add a CI round-trip test: emit(sample) → parse(sample) lossless across the boundary.
- Frontend: model `RunMeta` as a discriminated union on `type`
  (`{type:'backtest';params:BacktestParams}` | `{type:'train';params:TrainParams;train?:TrainProgress}`),
  mirrored from / kept in lockstep with the protocol crate; narrow at each consumer instead of casting.

**Out of scope.** Changing the wire format itself; the polling model (QE-410).

**Acceptance criteria.**
- One `ProgressLine` definition; cli + server compile against it; firewall test green.
- Renaming a progress field in one place fails a test; the protocol carries a version the server checks.
- Accessing a backtest-only field on a narrowed `train` run is a TS compile error.

**Cross-domain.** Tri-team contract (Backend + Architecture + Frontend) — the flagship "agreed" ticket.

---

### QE-407 — Server run-lifecycle robustness: graceful shutdown, supervised-task registry, honest success (extends QE-263)
`Phase: PreP3` · `Area: backend / orchestration` · `Depends on: QE-255` · `Effort: M`

**Why.** `main.rs:81` serves with **no** `with_graceful_shutdown` and no SIGTERM/SIGINT handler; on
restart every detached supervisor is dropped and — because the spawner sets `kill_on_drop(true)`
(`spawn.rs:59`) — the child is SIGKILLed mid-run while its `meta.json` stays `running` forever.
`RunManager::create` fires the supervisor via a bare `tokio::spawn` whose handle is discarded
(`manager.rs:112`), so the manager holds **no registry of in-flight runs** and cannot drain/cancel/count
at shutdown — the root enabler of the QE-263 orphan problem. Separately, a job that prints `done`, exits 0
but writes no `result.json` is marked `succeeded` (the in-code `TODO(QE-follow-up)` at `manager.rs:295`),
then `GET /runs/{id}/result` 409s on a run the UI shows green.

**Scope / requirements.**
- Install a shutdown signal future (`ctrl_c` + unix SIGTERM); pass to `axum::serve(...).with_graceful_shutdown`.
- Track live supervisors (`JoinSet` / `HashMap<run_id, JoinHandle>`); on shutdown stop accepting runs,
  terminally-mark remaining `running` runs, and await a bounded drain.
- Pair with the QE-263 startup reconciler: any `running` run with no live supervisor after a hard kill ⇒ `failed`.
- After `done`+exit-0, if `store.result_path(&id)` is absent, mark `failed` ("job reported done but wrote no
  result.json"); remove the TODO.

**Out of scope.** Distributed coordination; re-queueing (fail is acceptable v1); result.json schema validation.

**Acceptance criteria.**
- SIGTERM stops the listener, drains or terminally-marks in-flight runs (no `running` meta left for a clean
  shutdown), returns success; a hard-killed run is reconciled on next boot (test).
- A fake job that prints `done`/exits 0/writes nothing ⇒ `failed` with the reason; happy path still `succeeded`.

**Cross-domain.** Widens QE-263. Minor frontend copy for the new failure reason.

---

## P2 — before wider exposure / load

### QE-408 — Backtests list must filter to backtest runs (client filter + backend `?type=`)
`Phase: PreP3` · `Area: frontend / data-fetching + backend-contract` · `Depends on: QE-259` · `Effort: S`

**Why.** `BacktestsList` renders `listRuns()` **unfiltered** (`web/src/app/backtest/BacktestsList.tsx:53`)
while `TrainingList` correctly filters `type === 'train'` (`web/src/app/training/TrainingList.tsx:54`).
Training rows leak into the Backtests table with `undefined` params masked by `|| '—'`; clicking one routes
to `BacktestResult` → `getRunResult(id)` 409/404s → a permanently-erroring detail screen.

**Scope / requirements.** Filter `BacktestsList` to `run.type === 'backtest'` (confirm the discriminator
against QE-406); add a regression test asserting a mixed payload renders only backtest rows; add a server
`?type=` filter so the client stops over-fetching.

**Acceptance criteria.** A mixed `listRuns` response renders only backtest rows; no training run is reachable
via the Backtests table.

**Cross-domain.** Depends on the QE-406 `type` discriminator; server filter coordinates with QE-410.

---

### QE-409 — Auth completeness: 401 → re-auth in the SPA, logout endpoint, dev-safe cookies, fail-closed secret (adjacent QE-265)
`Phase: PreP3` · `Area: frontend + backend / auth` · `Depends on: QE-256` · `Effort: M`

**Why.** The run/vintage/coverage client has **no 401 branch** — `getJson` throws a generic `ApiError` for any
non-OK (`web/src/api/runs.ts:196-200`), so an expired cookie mid-session renders "Backtest failed" and burns
the polling retry budget instead of returning to Login; nothing flips the app back to the unauth shell.
Server-side: there is **no `/auth/logout`** (`auth/mod.rs:241-370`) so a session can't be cleared; session +
OAuth-state cookies are minted `Secure` unconditionally (`auth/mod.rs:253,319`) while the default bind is
`http://127.0.0.1:8080` (browsers don't send `Secure` cookies over `http://127.0.0.1`), so default-address dev
login silently fails to persist. The server also falls back to an ephemeral session secret (safe only on
loopback — AR-9 slice).

**Scope / requirements.**
- Centralise 401 handling in `runs.ts` (typed `UnauthorizedError`); an app-level listener resets `status` to
  `'unauth'` and remounts `Login` on any 401; polling treats 401 as terminal-auth (stops, does not retry).
- Add `GET|POST /api/auth/logout` clearing the session cookie (`Max-Age=0`) + a logout control in the UI.
- Make cookie `Secure` conditional on scheme/deployment (keep `HttpOnly` + `SameSite=Lax`).
- Server refuses to boot when bound non-loopback without `QE_SESSION_SECRET`.

**Out of scope.** Silent token refresh; OIDC `nonce` + local JWKS/RS256 (that's QE-265).

**Acceptance criteria.** A 401 routes the SPA to Login without reload and stops polling; logout ⇒ subsequent
`/api/me` is 401; default-loopback dev login persists; a non-loopback bind without a secret refuses to start.

**Cross-domain.** Frontend + Backend; adjacent to QE-265 — fold in if scheduled together.

---

### QE-410 — Run-list read path: shared polling hook, live list refresh, server pagination/projection/filter
`Phase: PreP3` · `Area: frontend + backend / read-path` · `Depends on: QE-255, QE-259` · `Effort: M`

**Why.** Lists fetch once on mount with no interval (`BacktestsList.tsx:50-62`, `TrainingList.tsx:50-62`) yet
render live `RUNNING {pct}%` — a running run shows a frozen percent until you navigate away. The bounded-retry
polling `useEffect` is duplicated near-verbatim between `BacktestResult.tsx:170-228` and
`TrainingMonitor.tsx:128-172` (plus copied `statusBadge`/`statusVariant`/`fmtDate`), so QE-409's 401 fix would
otherwise be applied four times. Server-side, `index.json` is append-only forever and `list_runs` returns the
**entire** history, re-reading each run's full `meta.json` every call (`api.rs:56-64`) with no pagination,
limit, or filter.

**Scope / requirements.**
- Frontend: `usePollingRun(runId,{pollMs})` hook with shared retry/terminal logic; promote one
  `StatusBadge`/`RunProgress`/date util to the design layer; poll `listRuns()` while any row is
  queued/running, stop when all terminal (guard overlapping requests).
- Backend: `?limit=&offset=` (or cursor) + `?status=`/`?type=` on `list_runs`, newest-first with next-cursor; a
  slim list projection (id/type/status/progress/created), deferring heavy `params`/`train` to `GET /runs/{id}`.

**Out of scope.** Websocket/SSE push; React Query adoption (note, don't require); the metrics-summary column (QE-264).

**Acceptance criteria.** A running run's list status/percent updates without navigation and stops when terminal;
default `list_runs` caps result size and paginates stably under concurrent creates; the two detail screens
consume one hook (duplicated blocks deleted); tests green.

**Cross-domain.** Frontend + Backend; coordinate response shape with QE-264 and the QE-408 `?type=` filter.

---

### QE-411 — Take run-store / read blocking `std::fs` off the async executor (extends QE-266)
`Phase: PreP3` · `Area: backend / async correctness` · `Depends on: QE-255` · `Effort: M`

**Why.** Run handlers do synchronous `std::fs` on tokio workers: `list_runs` reads `index.json` then loops
`read_meta()` for **every** run (`api.rs:52-64`), `get_run`/`get_result` read files (`api.rs:70,93`), `create`
does index read/rewrite under an async mutex (`manager.rs:92-105`). Each blocks the executor; `list_runs`
scales it to O(runs) blocking reads. The QE-257 read handlers already model the fix
(`read.rs:73,88` use `spawn_blocking`) — the runs path wasn't given the same treatment.

**Scope / requirements.** Wrap run-store fs ops (index read/write, meta read/write, result read) in
`spawn_blocking` (or `tokio::fs`), mirroring `read.rs`; batch `list_runs`' per-run reads into a single
`spawn_blocking` closure.

**Out of scope.** Changing the on-disk format / atomic-write strategy.

**Acceptance criteria.** No blocking `std::fs` remains on an async handler body; `list_runs`/`get_result`
behaviour unchanged; green gate.

**Cross-domain.** Directly widens QE-266. Pairs naturally with QE-410's list projection.

---

### QE-412 — Coverage query without full `Bar` decode (key-only LMDB cursor)
`Phase: PreP3` · `Area: backend / storage efficiency` · `Depends on: QE-253, QE-257` · `Effort: M`

**Why.** `GET /api/market-data/coverage` → `coverage_all` (`coverage.rs:81`) calls `scan_bars(i64::MIN..i64::MAX)`
per instrument × per `Resolution::ALL`, and `scan_bars` decodes **every bar** through `SerdeJson<Bar>` into a
`Vec<Bar>` (`store.rs:124-139`) only to read first/last/len — potentially millions of JSON deserialisations per
request, all discarded. heed can answer first-key/last-key/count over a prefix without decoding values (key-only
cursor / `DecodeIgnore`), the trick `bar_instruments` already uses.

**Scope / requirements.** Add a storage method returning `(first_open_time, last_open_time, count)` per
`(instrument, resolution)` prefix via a key-only cursor (timestamps from the key, never the value); reimplement
`coverage`/`coverage_all` on it; keep the exact `CoverageRow` shape/ordering.

**Out of scope.** LMDB schema changes; a persisted coverage index.

**Acceptance criteria.** Coverage output byte-identical on fixtures; no `Bar` value decode on the coverage path;
green gate.

**Cross-domain.** `qe-storage` is shared by CLI ingest + server — keep the `coverage` signature stable for the
CLI re-export.

---

### QE-413 — Observability: env-driven telemetry, per-request tracing, CLI telemetry init
`Phase: cross-cutting` · `Area: backend / observability` · `Depends on: QE-003, QE-254` · `Effort: M`

**Why.** `qe-server` installs `TelemetryConfig::default()` (`main.rs:16`) with **no env override**
(`telemetry/src/lib.rs:66`) — an operator can't change log level/format without recompiling. The router
(`lib.rs:304-320`) has **no** `TraceLayer`/request-id, so there is zero structured per-request logging
(method/path/status/latency). And `qe-cli` never calls `init_telemetry` (`cli/src/main.rs`), so the job
pipeline's `tracing` spans are dropped on the floor.

**Scope / requirements.** Add `TelemetryConfig::from_env` (level via `RUST_LOG`/`QE_LOG`, format toggle) used in
both composition roots; add `tower_http::trace::TraceLayer` (+ request-id) to `/api` with request/response spans
(keep `/api/health` quiet); initialise telemetry in `qe-cli`.

**Out of scope.** Metrics / OpenTelemetry export.

**Acceptance criteria.** Env var changes server log level without rebuild; each API request emits one structured
span with status+latency; a CLI run emits its stage spans; green gate.

**Cross-domain.** None.

---

### QE-414 — Deflated-Sharpe trial variance from the full trial population, not the selected top-N
`Phase: P1` · `Area: trading / statistical validation` · `Depends on: QE-131` · `Effort: M`

**Why.** DSR's `E[max SR]` scales with cross-trial Sharpe dispersion, but the train job estimates that dispersion
from `pool` = the top `MAX_POOL = 10` elites by fitness (`crates/cli/src/jobs/train.rs:252-260,472-492`) passed to
`trial_sharpe_variance` (`crates/validation/src/dsr.rs:79-85`). Survivors' Sharpes are a censored, tightly
clustered sample → variance under-estimated → deflation bar too low → **DSR inflated**, while `n_trials` counts
all occupied cells × generations × windows. G1 requires `DSR > 0.95` (`crates/gate/src/lib.rs:22`), biasing
promotion toward over-fit vintages.

**Scope / requirements.** Estimate `trial_variance` from the Sharpe distribution of **all** evaluated trials (or a
representative uncensored sample); keep `n_trials` and `trial_variance` from the same population; record both in
the report.

**Out of scope.** Changing the DSR formula.

**Acceptance criteria.** On a fixed archive, DSR from full-trial variance ≤ DSR from top-10 variance (regression),
and the gap is reported.

**Cross-domain.** Needs the archive to expose per-cell return series/Sharpes to the CLI job.

---

### QE-415 — Wire the purged/embargoed CV into selection fitness (it exists but is unused)
`Phase: P1` · `Area: trading / leakage controls` · `Depends on: QE-117, QE-113` · `Effort: L`

**Why.** `PurgedKFold` (`crates/wfo/src/cv.rs`) and `WalkForward` (`crates/wfo/src/walkforward.rs`) are correct and
tested, but the search evaluates `elite_fitness = backtest(g, train_bars, cfg)` over the **entire** train window
(`crates/cli/src/jobs/train.rs:228`), and the "noise-robust" windows are contiguous chunks of that same series
(`crates/wfo/src/backtest.rs:116-134`) — not OOS folds. MAP-Elites selects and the DE scores on in-sample
performance with only the terminal G1 holdout as OOS. The leakage-free CV objects are unused in selection, leaving
more over-fit exposure than the design advertises.

**Scope / requirements.** Fold `WalkForward`/`PurgedKFold` into the fitness path so an elite's fitness includes
purged OOS validation; wire the noise-robust windows to CV folds rather than adjacent slices.

**Out of scope.** The G1 terminal holdout (already present).

**Acceptance criteria.** An in-sample-overfit genome (great on train, poor on purged OOS) ranks below a robust one
under the new fitness; CV folds satisfy `windows_disjoint(lookback, label_horizon)`.

**Cross-domain.** Adds evaluation cost per genome; interacts with the determinism harness.

---

### QE-416 — Seal capacity-weighted allocation + worst-case-loss + real breaker calibration (not equal weights / defaults)
`Phase: P1` · `Area: trading / portfolio construction` · `Depends on: QE-128, QE-130, QE-116` · `Effort: M`

**Why.** DE selects members but sealed weights are overwritten to equal `1/k` (`crates/cli/src/jobs/train.rs:280`),
discarding capacity-aware allocation; the capacity model (`crates/ensemble/src/capacity.rs`) and worst-case-loss
stress set (`crates/ensemble/src/stress.rs`) are never invoked at seal — `worst_case_loss: None` and a constant
calibration are written (`train.rs:325-333`). So the runtime `BreakerLayer` runs on **un-calibrated** thresholds,
and `from_calibration` would in fact **pre-gate every strategy** if the profile lacks per-strategy entries
(`crates/runtime/src/live_breakers.rs:99-123`) — a vintage sealed this way trades nothing live (or on defaults).

**Scope / requirements.** Compute + persist capacity-capped weights (QE-128); run the QE-130 stress set and persist
`worst_case_loss`; produce a per-strategy `CalibrationProfile` from replayed equity behaviour (QE-116), not a
constant.

**Out of scope.** G3 sign-off logic.

**Acceptance criteria.** Sealed `weights` differ from equal-weight when capacity binds; `worst_case_loss` is `Some`;
`from_calibration` finds every sealed strategy (no unintended pre-gating).

**Cross-domain.** Expands the vintage format (coordinate with QE-402); feeds G3 (QE-308). Related to QE-401.

---

### QE-417 — Time-aware mark EMA (gap-aware) for the drawdown-breaker feed
`Phase: P2` · `Area: trading / runtime-risk` · `Depends on: QE-202, QE-208` · `Effort: S`

**Why.** `MarkEma` fixes `alpha` from an assumed 1s tick spacing (`crates/risk/src/breaker.rs:35-45`) and
`MarkEmaLoop` carries `event_time_ms` but never uses it (`crates/runtime/src/live_mark.rs:66-73`). markPrice@1s
streams gap on wss reconnects; after a multi-minute gap the EMA treats the first post-gap sample as a single 1s
step, so the smoothed mark lags real price by minutes and the DD probe runs on stale equity exactly during
disconnect-and-recovery windows when risk is highest.

**Scope / requirements.** Make the EMA time-aware: derive per-tick `alpha` from actual `Δt`, or detect a gap beyond
N ticks and re-seed; surface a "mark stream stale" health signal to breaker/cockpit.

**Out of scope.** wss reconnection plumbing.

**Acceptance criteria.** A 300s gap then a step yields a value consistent with 300s elapsed (not one 1s step); a
configurable staleness bound trips a health/halt signal.

**Cross-domain.** Interacts with the connection registry (QE-202) and cockpit (QE-304).

---

### QE-418 — Pre-trade gross cap checked against true gross exposure, not net notional
`Phase: P2` · `Area: trading / pre-trade risk` · `Depends on: QE-213, QE-215` · `Effort: S`

**Why.** The netter exposes true `long`/`short`/`gross` (`crates/runtime/src/live_netter.rs:97-103`) but only the
net `TargetPosition` reaches the governor, which checks **both** `MaxGrossExposure` and `MaxNetExposure` against
`mag = |net|` (`crates/runtime/src/pretrade.rs:125-142`). Correct today for a single instrument (gross == |net|),
but structurally wrong: once the universe grows (config is explicitly count-agnostic) or any hedged book exists,
real gross exceeds net and the gross cap silently passes oversized books.

**Scope / requirements.** Pass the netter's `long`/`short`/`gross` to the governor; check `MaxGrossExposure`
against `gross`, `MaxNetExposure` against `net`.

**Out of scope.** Multi-instrument netting itself.

**Acceptance criteria.** A book with `long = short = X` (net 0, gross 2X) breaches a `MaxGross < 2X` cap while
passing the net cap.

**Cross-domain.** Minor signature change at the netting→pretrade→hedger boundary.

---

### QE-419 — Unify config: single source of truth for storage dirs across `qe-server` and the spawned CLI
`Phase: PreP3` · `Area: architecture / config` · `Depends on: QE-002, QE-254` · `Effort: M`

**Why.** `qe-config` is the 12-factor system (figment, layered TOML, `QE_`+`__`, feeds the vintage hash), but
`qe-server` bypasses it with a parallel `QE_SERVER_*` namespace and bespoke parsing (`crates/server/src/lib.rs:160-263`).
The same physical dirs are configured **twice with no cross-check**: `storage.artifacts_dir`/`storage.market_dir`
in `config.example.toml` **and** `QE_SERVER_ARTIFACTS_DIR`/`QE_SERVER_MARKET_DIR` (`lib.rs:127-136`). Since the
server spawns `qe-cli` (which reads `config.toml`), a mismatch means backtests/coverage read a different store than
training wrote — a silent misconfiguration with no guard.

**Scope / requirements.** Either load `qe-config` for the shared storage dirs in `qe-server` (server-only
transport/auth knobs stay separate but reuse the figment env convention), or pass the server's resolved dirs to the
spawned CLI explicitly; add a boot-time assertion/log that server and spawned-CLI dirs match.

**Out of scope.** Per-environment overlay files (separate deployment ticket).

**Acceptance criteria.** One source of truth for artifacts/market dirs across server + CLI; a mismatch is detected
at boot, not at query time.

**Cross-domain.** Backend (server boot), Ops (deploy env).

---

### QE-420 — Real code-commit provenance in vintage lineage (build-time git SHA)
`Phase: cross-cutting` · `Area: architecture / determinism` · `Depends on: QE-006, QE-013` · `Effort: S`

**Why.** `Lineage` binds `code_commit` as one of the four inputs that determine a stage's output
(`crates/determinism/src/lineage.rs`), but the CLI resolves it as
`env::var("QE_CODE_COMMIT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION"))` = **`"0.1.0"`** when unset
(`crates/cli/src/main.rs:17-19`), and the Dockerfile never sets it. So the default/containerised path stamps every
vintage from every source tree with the same constant commit — two different code states get identical
`code_commit` and can collide in lineage provenance. Reproducibility is real for config+seeds but a no-op on the
code axis exactly where it's least visible.

**Scope / requirements.** Inject the git SHA at build (build script / vergen-style) so the fallback is the real
commit; keep `QE_CODE_COMMIT` as override; set it in the Dockerfile; optionally refuse to seal with a placeholder
commit unless `--allow-dirty`.

**Out of scope.** Full source-hash provenance; signing.

**Acceptance criteria.** Vintages sealed from two different commits carry different `code_commit`/lineage ids with
no env var set; the container image stamps its build SHA.

**Cross-domain.** Backend/Ops (build pipeline threads the SHA).

---

## P3 — opportunistic quality

### QE-421 — Adopt the `qe-error` recoverability taxonomy on the runtime order path
`Phase: cross-cutting` · `Area: architecture / error-strategy` · `Depends on: QE-268` · `Effort: L`

**Why.** `qe-error` defines `ErrorClass`/`Disposition` (retry/skip/halt, "never panic") and ARCHITECTURE.md sells it
as the cross-cutting strategy, but only `qe-clock` and `qe-risk` depend on it; `qe-runtime` has **zero**
`Disposition`/`ErrorClass` usage despite owning the bootstrap/live/edge order path. Every crate rolls its own
`thiserror` enum with no recoverability dimension, so the supervisor can't uniformly decide halt-vs-retry-vs-skip —
the exact decision the taxonomy exists for, on the exact path where "halt not panic" matters most.

**Scope / requirements.** Add a `Classified` trait in `qe-error` (`fn class(&self) -> ErrorClass`); implement for the
runtime + venue + risk error types so the live loop routes every error through `disposition()`; optionally lint/test
that runtime-path error types are `Classified`.

**Out of scope.** Rewriting training-side error handling; changing error text.

**Acceptance criteria.** Every error reachable on the order-emission path maps to a `Disposition`; a synthetic fatal
error drives to `Halt` (test).

**Cross-domain.** Trading (halt/kill semantics). Complementary to QE-268.

---

### QE-422 — Keyboard / screen-reader access for clickable table rows and universe chips
`Phase: PreP3` · `Area: frontend / accessibility` · `Depends on: QE-258` · `Effort: M`

**Why.** `DataTable` puts `onClick` on a bare `<tr>` with only a cursor style
(`web/src/design/DataTable.tsx:97-101`) — no role/tabIndex/keydown, so opening a run by keyboard is impossible even
though row-click is the primary navigation. The universe selector gives `role="checkbox"`+`aria-checked` to a `Tag`
that renders a non-focusable `<span>` (`web/src/design/Tag.tsx:38`, `NewBacktest.tsx:210-220`) — advertising
interactivity the element can't deliver.

**Scope / requirements.** Make clickable rows keyboard-operable (role/tabIndex/Enter-Space) when `onRowClick` is set;
make universe chips real focusable checkboxes; add keyboard-activation test assertions.

**Acceptance criteria.** A run opens from the list via keyboard alone; universe symbols toggle via keyboard and
announce as checkboxes.

**Cross-domain.** None.

---

### QE-423 — `DataTable` generic typing — drop the `Record<string, unknown>` casts
`Phase: PreP3` · `Area: frontend / type-safety` · `Depends on: QE-258` · `Effort: M`

**Why.** `DataTable`/`Column` are generic over `Row extends Record<string, unknown>` with `key: string` and
`render(value: unknown)` (`web/src/design/DataTable.tsx:38-55`), so every caller casts
`rows as (RunMeta & Record<string, unknown>)[]` (`BacktestsList.tsx:141`, `TrainingList.tsx:157`, `MarketData.tsx:80`,
`BacktestResult.tsx:455`) — defeating the generic and swallowing key typos. (Dead `is-sortable`/`qe-table__sort` CSS
exists at `DataTable.tsx:19-21` with no sort feature — remove or implement.)

**Scope / requirements.** Constrain `Column.key` to `keyof Row & string`, drop the `Record` bound (render value
`Row[key]`); remove the intersection casts at all four call sites.

**Acceptance criteria.** No call site casts to `& Record<string, unknown>`; a column referencing a non-existent key is
a compile error.

**Cross-domain.** None.

---

### QE-424 — Frontend resilience: error boundary + tests for the list/401/deep-link seams
`Phase: PreP3` · `Area: frontend / testing + resilience` · `Depends on: QE-259, QE-261` · `Effort: M`

**Why.** No React error boundary anywhere (`web/src/app/App.tsx:87-112`) — a render throw blanks the whole SPA. The
highest-risk seams are untested: list type-filtering (the QE-408 bug), 401/session-expiry (QE-409), list auto-refresh
of a running run (QE-410), and the bespoke router-less Training→Backtest deep-link (`App.tsx:36-39`).

**Scope / requirements.** Add a top-level error boundary with a recoverable fallback around the authed shell; add
tests for mixed-type list filtering, 401→Login, list refresh of a running run, and the vintage deep-link end-to-end.

**Acceptance criteria.** A thrown render error shows the fallback, not a blank page; new tests cover the four seams and
fail against today's behaviour where a bug exists.

**Cross-domain.** Encodes the run-lifecycle/auth contract — keep aligned with QE-406/409/410.

---

### QE-425 — Harden the axum router: request timeout, body cap, concurrency limit
`Phase: PreP3` · `Area: backend / robustness` · `Depends on: QE-254` · `Effort: S`

**Why.** The `/api` router (`lib.rs:304-320`) has no `TimeoutLayer`, no `ConcurrencyLimit`/load-shed, and no explicit
`DefaultBodyLimit`. A slow/stuck handler (a blocking read, a large coverage scan) has no server-side deadline; no
backstop on concurrent load independent of the run-pool semaphore.

**Scope / requirements.** Add per-request timeout, an explicit body-size limit on `POST /api/runs`, optionally a global
concurrency limit; return proper 408/413/503; leave health/static unaffected.

**Acceptance criteria.** Over-limit body ⇒ 413; a handler exceeding the deadline ⇒ 408/503; green gate.

**Cross-domain.** None. Complements QE-411/412.

---

### QE-426 — Split the `qe-runtime` god-crate along the spec's process seams (BLOCKED by trading fixes)
`Phase: Phase 3 prep` · `Area: architecture / crate-boundaries` · `Depends on: QE-401, QE-417, QE-418` · `Effort: L`

**Why.** `qe-runtime` is ~6.6k LOC / 18 flat modules re-exporting ~50 types (`crates/runtime/src/lib.rs:53-96`), fusing
four concerns the spec separates: Bootstrap ③, Live pipeline ④, Hedge Planning ⑤, and the Edge gateway ⑥ — where ⑤/⑥
are described as separate colocated processes over gRPC (QE-218's transport already lives at
`crates/runtime/src/transport.rs` but connects two halves of the *same* crate). The eventual deployment/security
boundary (the edge gateway is the only thing that submits orders) is not a compile boundary, blocking independent
panic-freedom scoping (QE-268) and independent deployment.

**Sequencing (agreed).** Do the trading-runtime correctness fixes (QE-401/417/418) **first**; this ticket is a pure
move afterward — hence the `Depends on`.

**Scope / requirements.** Split into e.g. `qe-hedger` (⑤ + live evaluation) and `qe-edge` (⑥ venue adapter / position
keeper / kill gate / order submission), plus a shared `qe-runtime-core`; make the gRPC boundary a crate boundary so the
order path is independently lint-scoped and deployable; extend the firewall rules (QE-405) to each split crate.

**Out of scope.** Changing runtime behaviour/wire format; doing this before G2.

**Acceptance criteria.** The order-submitting code compiles as its own crate; firewall rules cover each split crate;
existing runtime tests pass unchanged.

**Cross-domain.** Trading (future two-process live topology); coordinate before QE-427/QE-311.

---

### QE-427 — Container/deploy path for the admin server + SPA; fail closed
`Phase: cross-cutting` · `Area: architecture / deployment` · `Depends on: QE-013, QE-254, QE-258` · `Effort: M`

**Why.** The sole Dockerfile builds only `-p qe-cli` with `ENTRYPOINT ["qe"]` / `CMD ["train", …]` — it packages a
**batch job**, not the long-lived `qe-server` HTTP service, and never builds the SPA (`web/dist`). There is no image/
compose/manifest that runs the admin UI, so "deploy the admin UI" has no reproducible artefact and the server's own
gaps (no TLS/CORS/rate-limit, ephemeral session-secret fallback) surface only at deploy time.

**Scope / requirements.** Add a server image target (multi-stage: build SPA → `web/dist`, build `qe-server`, run with
`QE_SERVER_STATIC_DIR=web/dist`) distinct from the CLI batch image; make the server fail closed (refuse to boot) when
bound non-loopback without `QE_SESSION_SECRET` (shared with QE-409); keep relative volume paths (QE-013) so QE-311
stays mechanical.

**Out of scope.** Choosing/committing a specific PaaS; TLS termination (assume fronting proxy, document it).

**Acceptance criteria.** A documented image runs the authenticated server + SPA end-to-end; a non-loopback bind without
a session secret refuses to start.

**Cross-domain.** Backend/Ops + Frontend (SPA build in the server image). Precedes QE-311.

---

## Also noted (below the ticket bar, recorded for context)

- **PBO resolution / G1 gating (P3, trading):** `CSCV_BLOCKS = 2` (`crates/cli/src/jobs/train.rs:42`) gives PBO only
  `{0, 0.5, 1}` resolution, and PBO is reported but **not** a G1 promotion criterion
  (`crates/gate/src/lib.rs:143-174`). Consider making blocks configurable and adding a PBO ceiling to G1.
- **Equity-curve axis label (cosmetic, frontend):** the panel is labelled "Equity curve (log)" but `AreaChart` plots
  linearly (`BacktestResult.tsx:409` vs `:92-100`).
- **Reviewed and found sound** (no ticket): friction/average-cost accounting and funding sign convention
  (`crates/wfo/src/friction.rs`), next-bar-fill no-look-ahead discipline (`crates/wfo/src/backtest.rs:170-257`), the
  reconciliation debounce (`crates/runtime/src/reconciliation.rs`), the clock-skew overflow-safe halt
  (`crates/clock/src/skew.rs`), the cutover duplicate-drop/gap-detect continuity, the correlation-penalised
  ensemble objective, and the purged-CV/WFO leakage invariants themselves (correct — QE-415 is that they are *unused*
  in selection). The venue REST/rate-limit path and runtime transport are firewall-isolated, deterministic and
  panic-free on their hot paths — no high-value defect found.

## Suggested first slice

Lead with the "before trusting output / before live capital" set: **QE-402, QE-403, QE-404, QE-405** (small,
high-leverage guards) and the trading correctness P1s **QE-401, QE-414, QE-415, QE-416**; land the tri-team
contract **QE-406** early because QE-408/409/410/424 all depend on its shape; defer **QE-426** until after the
runtime correctness fixes.
