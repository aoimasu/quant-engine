# quant-engine — Backlog

Detailed, self-contained engineering tickets for the autonomous evolutionary-learning
trading platform described in [`docs/specs.md`](./specs.md). Read the spec first; this
backlog operationalises it.

## How to read this backlog

- **Flat, ordered list.** Tickets are in build order. Each is self-contained — it
  carries its own requirements and acceptance criteria; you should not need another
  ticket open to implement it (beyond the ones named in **Depends on**).
- **Ticket id** `QE-NNN`. The hundreds digit encodes the phase.
- **Phases** are sequenced to reach **offline backtest/research validation first**, then
  paper/sim, then live capital. Full platform is in scope; later phases are real tickets,
  not placeholders.
- **Gates** (`G1`/`G2`/`G3`) are blocking milestones. Work in a later phase may not start
  until the preceding gate passes.

### Ticket fields

`Phase` · `Area` (spec subgraph) · `Depends on` · **Why** · **Scope / requirements** ·
**Out of scope** · **Acceptance criteria** · `Spec ref`.

### Phase map

| Phase | Theme | Exit condition |
|-------|-------|----------------|
| **P0** | Foundations | Workspace, config, storage, shared types, time/risk contracts compile and are tested. |
| **P1** | Training → backtest validation | **G1**: a vintage passes net-of-cost, purged/embargoed, statistically-deflated validation on an untouched holdout. |
| **P2** | Runtime → paper/sim | **G2**: full loop runs in shadow/dry-run against live data with no order submission, reconciled vs simulator. |
| **P3** | Live, attribution & ops | **G3**: independent go/no-go sign-off; staged capital ramp under enforced caps. |

### Cross-cutting principles (apply to every ticket)

- **Determinism / reproducibility.** Any artefact (fused data, features, chromosomes,
  ensembles, calibration) must be reproducible from inputs + a recorded vintage hash.
- **Information firewall.** The evolutionary search has no visibility into portfolio-level
  or live outcomes; ensemble construction cannot see live execution; live cannot influence
  the archive. Enforced architecturally and in CI (QE-132).
- **Net-of-cost truth.** No strategy is ever selected, ranked, or reported on a return
  number that will not exist live (fees + funding + slippage). See QE-109.

> **Note on a deliberate spec divergence reviewed and retained.** A risk reviewer flagged
> the Strategy Allocation Journal's best-effort / 3-day-drop posture (QE-301) as a weak
> audit trail. Per decision, the spec's design is retained as-is; the concern is recorded
> here, not actioned.

### Resolved requirements (P0–P1 interview)

Authoritative for the P0/P1 tickets below; later phases inherit unless a ticket overrides.

- **Dev/test universe:** **BTCUSDT + ETHUSDT** (USDT-M perps) as the standing P1 dev set;
  universe stays config-driven and count-agnostic (QE-012).
- **Market series set:** ingest stays **source-abstract**; the exact series
  (perps / spot / premium index / funding / futures metrics + spread-to-underlier) is fixed
  in the fusion/data-integrity tickets (QE-103/QE-104), not hard-coded in the fetchers.
- **Resolutions:** base bar = **5m**; reconstruct **30m + 4h** (tier set configurable).
  markPrice@1s is handled separately on the runtime side.
- **History depth:** **max available** from each instrument's listing date, point-in-time
  aware; window configurable, default max.
- **Determinism:** **bit-for-bit identical artefacts independent of core/thread count**
  (deterministic reductions, fixed ordering, per-task seeding).
- **Indicator catalogue:** **broad set up front (~20+ indicators)**, each with per-indicator
  quantisation into a configurable number of states.
- **Friction defaults (QE-109):** Binance USDT-M **VIP0** (taker 0.05% / maker 0.02%),
  funding applied from the **actual historical funding series** (not a constant), spread-cross
  + size-dependent slippage; all configurable.
- **Spec-fidelity stance:** where the backlog overrode spec wording (A1 quality threshold,
  A2 breaker calibration, A3 raw-mark fast tier), the spike **implements the spec wording as
  baseline** and records the reviewer's stricter alternative as a documented option to revisit
  with evidence.
- **Deployment:** **run locally for now**; Railway is **deferred to P3 (QE-311)**. Services
  stay **deployment-agnostic** (env-driven config, no hard-coded absolute paths, all state
  under configurable volume-friendly directories) so the later Railway move is mechanical.
  Local run + packaging is **QE-013**.
- **Defaults not separately interviewed:** config format **TOML**; libraries **heed** (LMDB),
  **arrow-rs**, **rust_decimal**, **rayon**; P0/P1 are sync + rayon with **tokio deferred to
  P2**; walk-forward uses **rolling** windows (per spec) with train/validate/step sizes
  config-driven; statistical gates config-driven (default **DSR > 0** at the deflated
  threshold, **SPA p < 0.05**).

---

> **This file is an index.** Each ticket's full detail (Why / Scope / Out-of-scope /
> Acceptance criteria / Spec ref) lives in its own file under
> [`docs/mds/tickets/`](./mds/tickets/). Read this index to locate a ticket, then open its
> file to implement it. ✅ = Approved & merged (detail file links to its review record in
> [`docs/mds/reviewed/`](./mds/reviewed/)); an unmarked ticket is outstanding.

---

# Phase 0 — Foundations

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-001](./mds/tickets/QE-001.md) | Cargo workspace & crate topology | — | ✅ |
| [QE-002](./mds/tickets/QE-002.md) | Configuration system | QE-001 | ✅ |
| [QE-003](./mds/tickets/QE-003.md) | Structured logging & tracing | QE-001 | ✅ |
| [QE-004](./mds/tickets/QE-004.md) | Error model & result conventions | QE-001 | ✅ |
| [QE-005](./mds/tickets/QE-005.md) | CI pipeline | QE-001 | ✅ |
| [QE-006](./mds/tickets/QE-006.md) | Determinism & reproducibility harness | QE-002 | ✅ |
| [QE-007](./mds/tickets/QE-007.md) | Shared domain types | QE-001 | ✅ |
| [QE-008](./mds/tickets/QE-008.md) | Clock-skew / time-sync guard | QE-004 | ✅ |
| [QE-009](./mds/tickets/QE-009.md) | Risk-limit & kill-switch contract (shared types) | QE-004, QE-007 | ✅ |
| [QE-010](./mds/tickets/QE-010.md) | LMDB market-data store | QE-007 | ✅ |
| [QE-011](./mds/tickets/QE-011.md) | LMDB synthetic-data store | QE-007 | ✅ |
| [QE-012](./mds/tickets/QE-012.md) | Instrument-universe configuration & point-in-time membership | QE-002, QE-007 | ✅ |
| [QE-013](./mds/tickets/QE-013.md) | Local run & deployment-agnostic packaging | QE-002 | ✅ |

---

# Phase 1 — Training → backtest validation

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-101](./mds/tickets/QE-101.md) | Binance public-dumps downloader | QE-010, QE-012 | ✅ |
| [QE-102](./mds/tickets/QE-102.md) | Venue REST month-to-date backfill client | QE-101 | ✅ |
| [QE-103](./mds/tickets/QE-103.md) | Data-integrity & source reconciliation validation | QE-101, QE-102 | ✅ |
| [QE-104](./mds/tickets/QE-104.md) | Fusion, normalisation & Arrow serialisation | QE-103 | ✅ |
| [QE-105](./mds/tickets/QE-105.md) | Persist fused market data to LMDB | QE-104, QE-010 | ✅ |
| [QE-106](./mds/tickets/QE-106.md) | Multi-resolution bar reconstruction (batch) | QE-105, QE-011 | ✅ |
| [QE-107](./mds/tickets/QE-107.md) | Indicator catalogue (quantised, deterministic, parity-ready) | QE-106 | ✅ |
| [QE-108](./mds/tickets/QE-108.md) | Feature vector assembly → synthetic store | QE-107 | ✅ |
| [QE-109](./mds/tickets/QE-109.md) | Execution-friction & funding model | QE-105, QE-107 | ✅ |
| [QE-110](./mds/tickets/QE-110.md) | SPIKE: Strategy genome representation | QE-107 | ✅ |
| [QE-111](./mds/tickets/QE-111.md) | SPIKE: QD/MAP-Elites archive & behaviour descriptors | QE-110 | ✅ |
| [QE-112](./mds/tickets/QE-112.md) | SPIKE: Adaptive operator selection | QE-110 | ✅ |
| [QE-113](./mds/tickets/QE-113.md) | SPIKE: Geometric fitness, noise-robust eval & purged/embargoed CV | QE-109 | ✅ |
| [QE-114](./mds/tickets/QE-114.md) | SPIKE: Phased-lifecycle quality gate | QE-113 | ✅ |
| [QE-115](./mds/tickets/QE-115.md) | SPIKE: Ensemble discrete differential evolution | QE-113 | ✅ |
| [QE-116](./mds/tickets/QE-116.md) | SPIKE: Calibration profile & circuit-breaker model | QE-113 | ✅ |
| [QE-117](./mds/tickets/QE-117.md) | Walk-forward window manager | QE-113 | ✅ |
| [QE-118](./mds/tickets/QE-118.md) | QD MAP-Elites archive implementation | QE-111, QE-117 | ✅ |
| [QE-119](./mds/tickets/QE-119.md) | Variation operators + adaptive selection | QE-112, QE-118 | ✅ |
| [QE-120](./mds/tickets/QE-120.md) | Strategy backtester | QE-109, QE-113, QE-118 | ✅ |
| [QE-121](./mds/tickets/QE-121.md) | Thompson-sampling parent selection | QE-118 | ✅ |
| [QE-122](./mds/tickets/QE-122.md) | Behavioural regularisation | QE-118 | ✅ |
| [QE-123](./mds/tickets/QE-123.md) | Phased recording → Strategy repository | QE-114, QE-120 | ✅ |
| [QE-124](./mds/tickets/QE-124.md) | Elite robustness gates | QE-120 | ✅ |
| [QE-125](./mds/tickets/QE-125.md) | Regime labelling | QE-106 | ✅ |
| [QE-126](./mds/tickets/QE-126.md) | Discrete DE portfolio search | QE-115, QE-123 | ✅ |
| [QE-127](./mds/tickets/QE-127.md) | Correlation penalty + per-regime expectancy constraint | QE-126, QE-125 | ✅ |
| [QE-128](./mds/tickets/QE-128.md) | Capacity analysis gating ensemble weights | QE-126, QE-109 | ✅ |
| [QE-129](./mds/tickets/QE-129.md) | Ensemble repository, calibration profile & vintage artefact format | QE-127, QE-116, QE-006 | ✅ |
| [QE-130](./mds/tickets/QE-130.md) | Stress / worst-case-loss scenarios | QE-127 | ✅ |
| [QE-131](./mds/tickets/QE-131.md) | Statistical robustness suite | QE-120, QE-126 | ✅ |
| [QE-132](./mds/tickets/QE-132.md) | Information-firewall CI guard | QE-123, QE-126 | ✅ |
| [QE-133](./mds/tickets/QE-133.md) | Validation reporting | QE-131, QE-125, QE-128, QE-109 | ✅ |
| [QE-134](./mds/tickets/QE-134.md) | GATE G1: Holdout embargo & over-fit acceptance | QE-133 | ✅ |
| [QE-135](./mds/tickets/QE-135.md) | Parquet export + DuckDB analytics  *(deferred)* | QE-105 | — |
| [QE-136](./mds/tickets/QE-136.md) | Signal viewer  *(deferred)* | QE-135, QE-107 | — |

---

# Phase 2 — Runtime → paper/sim   *(gated by G1)*

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-201](./mds/tickets/QE-201.md) | Venue-aware REST client (rate-limit + ephemeral cache) | QE-004 | ✅ |
| [QE-202](./mds/tickets/QE-202.md) | wss Market-tier streams + connection registry | QE-004 | ✅ |
| [QE-203](./mds/tickets/QE-203.md) | wss Realtime-tier streams | QE-202 | ✅ |
| [QE-204](./mds/tickets/QE-204.md) | User-data stream subscription | QE-201 | ✅ |
| [QE-205](./mds/tickets/QE-205.md) | Streaming bar reconstruction + live kline source | QE-202, QE-106 | ✅ |
| [QE-206](./mds/tickets/QE-206.md) | Factor join + batch/streaming parity tests | QE-205, QE-107 | ✅ |
| [QE-207](./mds/tickets/QE-207.md) | Evaluator session (shared replay + live modes) | QE-206, QE-129 | ✅ |
| [QE-208](./mds/tickets/QE-208.md) | Mark EMA loop + tick observer | QE-202 | ✅ |
| [QE-209](./mds/tickets/QE-209.md) | Bootstrap pipeline | QE-201, QE-207 | ✅ |
| [QE-210](./mds/tickets/QE-210.md) | Reconstructed state | QE-209 | ✅ |
| [QE-211](./mds/tickets/QE-211.md) | Bootstrap→live in-process cutover | QE-210, QE-207 | ✅ |
| [QE-212](./mds/tickets/QE-212.md) | Circuit-breaker layer | QE-116, QE-208, QE-210 | ✅ |
| [QE-213](./mds/tickets/QE-213.md) | Position netting | QE-212 | ✅ |
| [QE-214](./mds/tickets/QE-214.md) | Hedge Planner (target-position) | QE-213 | ✅ |
| [QE-215](./mds/tickets/QE-215.md) | Pre-trade risk check | QE-009, QE-214, QE-130 | ✅ |
| [QE-216](./mds/tickets/QE-216.md) | Out-of-band kill-switch at venue adapter | QE-009, QE-217 | ✅ |
| [QE-217](./mds/tickets/QE-217.md) | Venue adapter / Position keeper / order lifecycle + simulator | QE-203, QE-204, QE-007 | ✅ |
| [QE-218](./mds/tickets/QE-218.md) | gRPC transport (Hedge Planner ↔ Edge gateway) | QE-214, QE-217 | ✅ |
| [QE-219](./mds/tickets/QE-219.md) | Vintage load (read-only) + rollover | QE-129, QE-207 | ✅ |
| [QE-220](./mds/tickets/QE-220.md) | Bootstrap/restart parity test | QE-210, QE-211 | ✅ |
| [QE-221](./mds/tickets/QE-221.md) | Real-time reconciliation divergence alarm | QE-217 | ✅ |
| [QE-222](./mds/tickets/QE-222.md) | GATE G2: Live shadow / dry-run | QE-218, QE-221 | ✅ |

---

# Phase PreP3 — Admin UI for training & backtesting   *(built after G2, before live)*

> **Numbering note.** PreP3 sits between Phase 2 and Phase 3 and has no free "hundreds" slot
> (2xx = P2, 3xx = P3), so its tickets use the **25x** band — numerically adjacent to the P2
> runtime/tooling they extend. `Phase: PreP3` tags them explicitly.
>
> **Design of record:** [`docs/superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md`](./superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md)
> · decisions [`docs/architecture/admin-ui-decisions.md`](./architecture/admin-ui-decisions.md)
> · v1 plan [`docs/superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md`](./superpowers/plans/2026-07-03-admin-ui-v1-cli-jobs.md).
> Four accepted decisions: **D1** backtest an existing *vintage* over a window (not parametric);
> **D2** backtest first, training monitor later; **D3** pre-ingested data + a minimal `ingest`;
> **D4** `qe-server` (axum+tokio) supervising `qe-cli` subprocesses, file-based run store, Google
> OAuth + email allowlist. Delivery order: CLI jobs → server+auth → SPA → training monitor.

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-251](./mds/tickets/QE-251.md) | `qe-cli backtest` job (run a vintage over a window) | QE-129, QE-120, QE-107, QE-108, QE-105 | ✅ |
| [QE-252](./mds/tickets/QE-252.md) | Backtester trade-level recording (trades, win-rate, profit-factor) | QE-120 | ✅ |
| [QE-253](./mds/tickets/QE-253.md) | `qe-cli ingest` scaffold + sample store fixture + coverage query | QE-101, QE-102, QE-105 | ✅ |
| [QE-254](./mds/tickets/QE-254.md) | `qe-server` crate scaffold (axum + tokio, static SPA, firewall guard) | QE-001 | ✅ |
| [QE-255](./mds/tickets/QE-255.md) | Run store + run lifecycle API + subprocess supervision | QE-251, QE-254 | ✅ |
| [QE-256](./mds/tickets/QE-256.md) | Google OAuth + email allowlist + signed session | QE-254 | ✅ |
| [QE-257](./mds/tickets/QE-257.md) | Vintages + market-data coverage read APIs | QE-253, QE-254 | ✅ |
| [QE-258](./mds/tickets/QE-258.md) | Frontend scaffold + design-system port (Vite/React, AppShell, Login) | QE-256 | ✅ |
| [QE-259](./mds/tickets/QE-259.md) | Backtest screens wired to the API | QE-255, QE-257, QE-258 | ✅ |
| [QE-260](./mds/tickets/QE-260.md) | Runnable `qe-cli train` search job + rich progress  *(fast-follow)* | QE-118, QE-120, QE-126, QE-134 | ✅ |
| [QE-261](./mds/tickets/QE-261.md) | Training-monitor UI screen  *(fast-follow)* | QE-259, QE-260 | ✅ |

---

## PreP3 follow-ups (QE-262..QE-266)

> Hardening / completeness items surfaced by the PreP3 code reviews (recorded in
> `docs/mds/reviewed/qe-251.md` … `qe-261.md`). **None blocks PreP3** — it shipped green. Priority tags:
> **P1** = correctness/safety, do before trusting training output for decisions; **P2** = do before wider
> exposure or heavier load; **P3** = opportunistic quality. Same 25x/26x band as PreP3.

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-262](./mds/tickets/QE-262.md) | Persist catalogue version + states in the vintage; assert on load  *(P1 — correctness)* · **delivered by [QE-402](#review-r1)** — see [`reviewed/qe-402.md`](./mds/reviewed/qe-402.md) | QE-260, QE-251 | ✅ |
| [QE-263](./mds/tickets/QE-263.md) | Run-store startup reconciler for orphaned `running` runs  *(P2)* · **→ extended by [QE-407](#review-r1)** (adds the graceful-shutdown / task-registry half) | QE-255 | — |
| [QE-264](./mds/tickets/QE-264.md) | Enrich read APIs for the admin UI (vintage symbol roster + run metrics summary)  *(P2 — UX completeness)* · *see [QE-410](#review-r1)* (list projection/pagination — coordinate response shape) | QE-257, QE-259 | — |
| [QE-265](./mds/tickets/QE-265.md) | Auth hardening: OIDC `nonce` + local JWKS/RS256 verification  *(P2 — security defense-in-depth)* · *adjacent to [QE-409](#review-r1)* (SPA 401→re-auth, logout, dev-safe cookies) | QE-256 | — |
| [QE-266](./mds/tickets/QE-266.md) | qe-server non-blocking I/O + run-supervision robustness nits  *(P3 — quality/scale)* · **→ extended by [QE-407](#review-r1) + [QE-411](#review-r1)** (honest success + `list_runs` N-reads) | QE-255, QE-257 | — |
| [QE-267](./mds/tickets/QE-267.md) | Enforce the no-`unwrap` convention with `clippy::unwrap_used = "deny"`  *(P3 — quality/hardening)* | — | ✅ |
| [QE-268](./mds/tickets/QE-268.md) | Extend the panic-freedom `deny` attribute from the demonstrator to the live order-emission path  *(P2 — live-trading safety)* | QE-267 | ✅ |

---

<a id="review-r1"></a>
# Review R1 — Cross-cutting hardening & refactor (QE-4xx)

> **Provenance.** A four-discipline improvement/refactor review (Senior Frontend, Senior Backend,
> Trading Expert, Architect) on 2026-07-15. Findings, evidence (`file:line`), and the facilitated
> synthesis — including the discussion outcomes and per-ticket cross-domain flags — are in
> [`docs/reviews/2026-07-15-team-improvement-review.md`](./reviews/2026-07-15-team-improvement-review.md).
>
> **Band.** The phase bands (1xx–3xx) and the PreP3 25x/26x band are full, so these cross-cutting
> items use a dedicated **QE-4xx "review / hardening"** band. They are not new spec features and do
> not change the P0–P2 gates. Priority tags follow the house convention (**P1** correctness/safety ·
> **P2** before wider exposure/load · **P3** opportunistic quality).
>
> **Numbering reconciliation with the open PreP3 follow-ups** (no duplicate IDs — the 4xx ticket owns
> the widened work, the 26x row is annotated above):
>
> | 4xx | relation | existing open ticket |
> |-----|----------|----------------------|
> | QE-402 | **supersedes** | QE-262 (catalogue version → schema-registry umbrella) |
> | QE-407 | **extends** | QE-263 (startup reconciler → + shutdown/task-registry) & QE-266 (honest success) |
> | QE-411 | **extends** | QE-266 (non-blocking I/O → `list_runs` N-reads) |
> | QE-409 | **adjacent** | QE-265 (auth hardening) |
> | QE-410 | **coordinate** | QE-264 (read-API enrichment / list shape) |
> | QE-421 | **complements** | QE-268 (panic-freedom → error-recoverability taxonomy) |

### R1.a — P1 (lead the queue: before trusting output / before live capital)

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| QE-401 | Seed the live drawdown breaker with the reconstructed committed-peak equity  *(P1 — capital safety)* | QE-210, QE-211, QE-212 | — |
| QE-407 | Server run-lifecycle: graceful shutdown, supervised-task registry, honest success  *(P1)* · **extends QE-263/266** | QE-255 | — |

### R1.b — P2 (before wider exposure / load)

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| QE-408 | Backtests list must filter to backtest runs (client filter + server `?type=`)  *(P2 — correctness)* | QE-259 | — |
| QE-409 | Auth completeness: SPA 401→re-auth, logout endpoint, dev-safe cookies, fail-closed secret  *(P2)* · *adjacent QE-265* | QE-256 | — |
| QE-410 | Run-list read path: shared polling hook, live list refresh, server pagination/projection/filter  *(P2)* · *coordinate QE-264* | QE-255, QE-259 | — |
| QE-411 | Take run-store / read blocking `std::fs` off the async executor  *(P2)* · **extends QE-266** | QE-255 | — |
| QE-412 | Coverage query without full `Bar` decode (key-only LMDB cursor)  *(P2 — efficiency)* | QE-253, QE-257 | — |
| QE-416 | Seal capacity-weighted allocation + worst-case-loss + real breaker calibration  *(P2)* | QE-128, QE-130, QE-116 | — |
| QE-417 | Time-aware mark EMA (gap-aware) for the drawdown-breaker feed  *(P2 — runtime-risk)* | QE-202, QE-208 | — |
| QE-418 | Pre-trade gross cap checked against true gross exposure, not net notional  *(P2 — risk)* | QE-213, QE-215 | — |
| QE-419 | Unify config: single source of truth for storage dirs across server + spawned CLI  *(P2)* | QE-002, QE-254 | — |

| QE-428 | Route reported-backtest impact through the selection cost model / a CLI flag (QE-128) so reporting PnL matches selection  *(P3 — follow-up from QE-403 review)* | QE-403, QE-128 | — |

### R1.c — P3 (opportunistic quality)

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| QE-421 | Adopt the `qe-error` recoverability taxonomy on the runtime order path  *(P3)* · *complements QE-268* | QE-268 | — |
| QE-422 | Keyboard / screen-reader access for clickable table rows and universe chips  *(P3 — a11y)* | QE-258 | — |
| QE-423 | `DataTable` generic typing — drop the `Record<string, unknown>` casts  *(P3 — type-safety)* | QE-258 | — |
| QE-424 | Frontend resilience: error boundary + tests for the list/401/deep-link seams  *(P3)* | QE-259, QE-261 | — |
| QE-425 | Harden the axum router: request timeout, body cap, concurrency limit  *(P3)* | QE-254 | — |
| QE-426 | Split the `qe-runtime` god-crate along the spec's process seams  *(P3 — refactor)* · **blocked by QE-401/417/418** | QE-401, QE-417, QE-418 | — |
| QE-427 | Container/deploy path for the admin server + SPA; fail closed  *(P3 — deployment)* | QE-013, QE-254, QE-258 | — |

> **Suggested first slice:** the small high-leverage guards **QE-402, QE-403, QE-404, QE-405** and the
> trading correctness P1s **QE-401, QE-414, QE-415, QE-416**; land the tri-team contract **QE-406**
> early because QE-408/409/410/424 depend on its shape; defer **QE-426** until after the runtime
> correctness fixes.

---

# Phase 3 — Live, attribution & ops   *(gated by G2; live capital gated by G3)*

| Ticket | Title | Depends on | Status |
|--------|-------|------------|:------:|
| [QE-301](./mds/tickets/QE-301.md) | Strategy Allocation Journal (best-effort, 3-day retry) | QE-218 | — |
| [QE-302](./mds/tickets/QE-302.md) | Reconciliation (journal × venue trade history) | QE-301 | — |
| [QE-303](./mds/tickets/QE-303.md) | Attribution outputs | QE-302 | — |
| [QE-304](./mds/tickets/QE-304.md) | Cockpit (observability + authenticated manual controls) | QE-214, QE-212 | — |
| [QE-305](./mds/tickets/QE-305.md) | Monitoring / alerting SLAs + on-call | QE-304 | — |
| [QE-306](./mds/tickets/QE-306.md) | Incident runbooks (pre-go-live) | QE-305 | — |
| [QE-307](./mds/tickets/QE-307.md) | Single-venue concentration cap + outage/withdrawal-halt runbook | QE-306 | — |
| [QE-308](./mds/tickets/QE-308.md) | GATE G3: Go/No-Go to live capital | QE-130, QE-222, QE-305, QE-306, QE-307 | — |
| [QE-309](./mds/tickets/QE-309.md) | Staged capital ramp with enforced caps | QE-308, QE-215 | — |
| [QE-310](./mds/tickets/QE-310.md) | Live-deployment hardening + breaker re-calibration | QE-309 | — |
| [QE-311](./mds/tickets/QE-311.md) | Railway deployment & CD  *(deferred)* | QE-013, QE-217, QE-305 | — |

---

## Appendix — Review provenance

This backlog incorporates an expert quant-trader review and an expert
financial-advisor/governance review of the original blueprint. Reviewer-driven changes,
all folded into the tickets above:

- **Net-of-cost truth pulled into Phase 1** (QE-109) as a blocker on the backtester.
- **Leakage controls**: purge/embargo (QE-113, QE-117), no-OOS bandit reward (QE-121),
  firewall CI guard (QE-132). *(The train/CV-only quality threshold in QE-114 is documented
  as a reviewer alternative; the spec's full-validation-distribution wording is the baseline —
  see the P0–P1 spec-fidelity decision.)*
- **Statistical deflation** as milestone DoD (QE-131) + holdout gate **G1** (QE-134).
- **Data integrity / point-in-time universe** (QE-012, QE-103).
- **Regime labelling, correlation penalty, capacity, elite-robustness** (QE-124–128).
- **Stress / worst-case-loss** (QE-130).
- **Risk elevated early**: kill-switch/caps contract in P0 (QE-009), clock-skew guard
  (QE-008), breaker backtested in P1 (QE-116), pre-trade checks + out-of-band kill (QE-215,
  QE-216), restart parity (QE-220), real-time reconciliation alarm (QE-221).
- **Trust gates + ramp**: live shadow **G2** (QE-222), go/no-go **G3** (QE-308), staged ramp
  (QE-309), runbooks/alerting/concentration before go-live (QE-305–307).

**Spec-fidelity decisions (P0–P1 interview):** where the backlog overrode spec wording, the
spike tickets now implement the **spec wording as baseline** and document the reviewer's
stricter alternative — A1 quality threshold (QE-114), A2 breaker calibration (QE-116), A3
raw-mark fast tier (QE-116, QE-208, QE-212).

**Retained spec divergence (decision: keep spec):** the Strategy Allocation Journal's
best-effort / 3-day-drop audit posture (QE-301). The governance reviewer recommended an
unbounded durable WAL; per decision the spec's design stands and the concern is recorded
here only.
