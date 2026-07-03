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

# Phase 0 — Foundations

## QE-001 — Cargo workspace & crate topology
`Phase: P0` · `Area: cross-cutting` · `Depends on: —`

**Why.** A single Rust workspace with clear crate boundaries keeps the training and
runtime pipelines decoupled (per spec) while sharing the indicator catalogue and domain
types.

**Scope / requirements.**
- Cargo workspace with crates split by responsibility, at minimum: `domain` (shared
  types), `storage`, `ingest`, `signal` (indicator catalogue + bars), `wfo`, `ensemble`,
  `runtime`, `venue`, `cli`/`bins`. Training and runtime depend on `signal`/`domain` only,
  never on each other.
- Workspace-level lints, shared dependency versions, release/dev profiles.
- `rust-toolchain.toml` pinning the toolchain.

**Out of scope.** Any business logic; storage schemas (QE-010/011).

**Acceptance criteria.**
- `cargo build --workspace` and `cargo test --workspace` succeed on a clean checkout.
- A dependency check proves `runtime` does not depend on `wfo`/`ensemble` and vice-versa
  (shared code only via `signal`/`domain`).

`Spec ref: Platform — "intentionally decoupled… aside from shared indicators and strategy logic".`

## QE-002 — Configuration system
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Why.** Every stage is parameterised (windows, archive resolution, thresholds, venue,
instrument universe). Config must be layered and reproducible.

**Scope / requirements.**
- Typed, layered config (file + env override), validated at load with clear errors.
- Config is hashable and recorded into vintage lineage (feeds QE-006/QE-129).
- Separate profiles for `train`, `runtime-sim`, `runtime-live`.

**Out of scope.** Secrets management beyond reading from env/secret store references.

**Acceptance criteria.**
- Invalid config fails fast with a field-level message.
- The same config file produces the same config hash across runs/machines.

`Spec ref: Platform — per-vintage artefacts; Training vs Runtime profiles.`

## QE-003 — Structured logging & tracing
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Why.** Long offline runs and a latency-sensitive runtime both need structured,
low-overhead observability.

**Scope / requirements.**
- `tracing`-based structured logs with spans; JSON output option; per-module levels.
- Correlation fields: vintage hash, instrument, window id, run id.
- Hot-path logging must be allocation-light and non-blocking.

**Out of scope.** Metrics dashboards / cockpit (QE-304); alerting SLAs (QE-305).

**Acceptance criteria.**
- A training run emits spans for each stage with the correlation fields populated.
- Logging the runtime hot path adds no blocking I/O on the order-emission path.

`Spec ref: Runtime notes — observability; "no database on the critical path".`

## QE-004 — Error model & result conventions
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Why.** Consistent error handling distinguishes recoverable (retry/skip) from fatal
(halt) conditions — load-bearing for the runtime's halt/kill semantics.

**Scope / requirements.**
- Error taxonomy: `Transient` (retryable), `Data` (skip/quarantine), `Fatal` (halt).
- Conventions for surfacing fatal runtime errors to the kill-switch contract (QE-009).
- No `unwrap`/`panic` on hot paths; CI lint enforces.

**Out of scope.** Specific retry policies (defined per-ticket where used).

**Acceptance criteria.**
- A Fatal error on the runtime path is routed to a halt, not a panic.
- Clippy gate rejects `unwrap`/`expect`/`panic` in designated hot-path modules.

`Spec ref: Robustness — layered circuit breakers; halt semantics.`

## QE-005 — CI pipeline
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-001`

**Why.** Greenfield discipline: every change is formatted, linted, tested, and
dependency-audited.

**Scope / requirements.**
- CI runs `fmt --check`, `clippy -D warnings`, `test --workspace`, and a dependency/licence
  audit (`cargo-deny` or equivalent).
- Caches builds; fails the merge on any gate.
- Hooks for the firewall guard (QE-132) and determinism harness (QE-006) once they exist.

**Out of scope.** Deployment/release automation (local packaging QE-013; Railway/CD QE-311).

**Acceptance criteria.**
- A PR with a clippy warning or failing test cannot be merged.
- CI completes deterministically (no flaky-by-design steps).

`Spec ref: cross-cutting engineering quality.`

## QE-006 — Determinism & reproducibility harness
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-002`

**Why.** Vintages must be auditable months later; reproducibility is the foundation of the
information firewall and of trustworthy validation.

**Scope / requirements.**
- Seeded RNG plumbed through all stochastic stages (QD operators, DE, Thompson sampling).
- A harness that re-runs a stage twice and asserts byte-identical artefacts.
- Vintage lineage record: config hash + input data snapshot id + code/commit + seeds.

**Out of scope.** Storage of artefacts (QE-010/011/129).

**Acceptance criteria.**
- Two runs of the same stage with the same lineage produce byte-identical outputs
  **independent of core/thread count** (deterministic reductions + fixed task ordering).
- Every produced artefact carries a resolvable lineage record.

`Spec ref: Platform — per-vintage chromosomes/ensembles; Theory — reproducible research.`

## QE-007 — Shared domain types
`Phase: P0` · `Area: domain` · `Depends on: QE-001`

**Why.** One vocabulary for instruments, time, bars, money, and direction prevents
divergence between training and runtime and underpins batch/streaming parity.

**Scope / requirements.**
- Types for: instrument id, venue, bar resolution, OHLCVT bar, funding rate sample,
  timestamp/interval (UTC, explicit precision), price/qty/notional (fixed-point decimal,
  no float money), side/direction, vintage hash.
- Conversions are total and tested; no silent precision loss on money.

**Out of scope.** Indicator/feature types (QE-107/108); strategy genome (QE-110).

**Acceptance criteria.**
- Money arithmetic is exact (property tests for associativity/rounding policy).
- Bar/resolution types are shared by both pipelines (single definition).

`Spec ref: Signal generation — bars, indicators; Runtime — equity/margin.`

## QE-008 — Clock-skew / time-sync guard
`Phase: P0` · `Area: cross-cutting` · `Depends on: QE-004`

**Why.** Funding timestamps, the 60s mark EMA, bar-close evaluation, and signed-request
windows all assume a trustworthy clock; skew on a leveraged venue causes wrong funding
accrual, mis-timed breakers, and rejected requests. *(Reviewer-added.)*

**Scope / requirements.**
- Monitor local-vs-reference (NTP / venue server time) skew.
- Hard halt (via QE-009 kill path) when skew exceeds a configured threshold.
- Surface skew as a health signal to the cockpit (QE-304).

**Out of scope.** Time-series alignment in fusion (QE-104).

**Acceptance criteria.**
- Simulated skew beyond threshold triggers a halt, not a silent continue.
- Skew is logged with the correlation fields and exposed as health state.

`Spec ref: Runtime — markPrice@1s, funding stamps, rate-limit windows.`

## QE-009 — Risk-limit & kill-switch contract (shared types)
`Phase: P0` · `Area: domain / risk` · `Depends on: QE-004, QE-007`

**Why.** Reviewers: the order path must be *born* with hard caps and an out-of-band halt,
not have them retrofitted at runtime. Defining the contract in P0 forces every downstream
component to honour it. *(Reviewer-added.)*

**Scope / requirements.**
- First-class types for limit kinds: max notional, max leverage, max gross/net exposure,
  liquidation-distance floor, margin-utilisation ceiling, per-vintage drawdown caps.
- A kill-switch contract: out-of-band (independent of cockpit and Hedge Planner), acts at
  the order-submission layer, deterministically flattens-and-halts, independently testable.
- Limit-violation outcomes: clamp vs reject vs halt, defined per limit kind.

**Out of scope.** Enforcement implementation (QE-215, QE-216) — this ticket is the contract.

**Acceptance criteria.**
- The contract compiles and is referenced by the runtime crates' interfaces.
- A conformance test asserts any order-submitting component must accept a kill handle.

`Spec ref: Robustness — layered circuit breakers; Runtime — cockpit manual controls.`

## QE-010 — LMDB market-data store
`Phase: P0` · `Area: ③ Storage` · `Depends on: QE-007`

**Why.** The fused market corpus (bars, funding, spread-to-underlier, futures metrics)
needs a fast, embedded, deterministic key-value store.

**Scope / requirements.**
- LMDB schema for OHLCVT bars, funding rates, premium/spread-to-underlier, futures metrics
  (top-trader L/S, OI, taker), keyed by instrument + resolution + time.
- Versioned schema; read/write APIs with range scans; concurrency-safe reads.

**Out of scope.** Synthetic/indicator cache (QE-011); fusion logic (QE-104).

**Acceptance criteria.**
- Round-trip + range-scan tests pass for each record kind.
- Schema version is recorded and mismatches are detected on open.

`Spec ref: ③ Storage — "LMDB · market data".`

## QE-011 — LMDB synthetic-data store
`Phase: P0` · `Area: ③ Storage` · `Depends on: QE-007`

**Why.** Indicator caches and multi-resolution bars are derived artefacts read heavily by
WFO/DE; they belong in a separate store with its own lifecycle.

**Scope / requirements.**
- LMDB schema for indicator-state cache and multi-resolution bars keyed by instrument +
  resolution + indicator id + lookback + time.
- Cache invalidation tied to source lineage (QE-006).

**Out of scope.** Indicator computation (QE-107).

**Acceptance criteria.**
- Cached indicator states are byte-identical to freshly computed ones (parity test).
- Stale-source detection invalidates dependent cache entries.

`Spec ref: ③ Storage — "LMDB · synthetic data".`

## QE-012 — Instrument-universe configuration & point-in-time membership
`Phase: P0` · `Area: cross-cutting / data` · `Depends on: QE-002, QE-007`

**Why.** Universe was deferred to a ticket; reviewers add that membership must be
point-in-time to avoid survivorship bias (training only on coins that survived inflates
results).

**Scope / requirements.**
- Universe is config-driven and instrument-count-agnostic (single → many). Standing P1
  dev/test universe = **BTCUSDT + ETHUSDT** (USDT-M perps).
- **Point-in-time membership:** listing/delisting dates respected; a backtest as-of date
  only sees instruments tradable then.
- Explicit policy for delisted/blown-up symbols (included historically, not silently dropped).

**Out of scope.** Per-instrument archive sharding decisions (handled in QE-118).

**Acceptance criteria.**
- A backtest window excludes instruments not yet listed / already delisted at that time.
- Changing the universe size requires only config, no code change.

`Spec ref: Overview — "linear markets"; reviewer: survivorship / point-in-time.`

## QE-013 — Local run & deployment-agnostic packaging
`Phase: P0` · `Area: cross-cutting / ops` · `Depends on: QE-002`

**Why.** Per decision, the platform runs locally for now and Railway is deferred; keeping
services deployment-agnostic from day one makes the later move mechanical and preserves
dev/prod parity.

**Scope / requirements.**
- A documented one-command local run for each runnable (the training pipeline now; runtime
  services in sim mode once they exist).
- **12-factor config:** all settings env-overridable (via QE-002); **no hard-coded absolute
  paths**; all persistent state (LMDB stores, vintage artefacts, ephemeral REST cache,
  allocation journal) lives under **configurable, volume-friendly directories**.
- Optional Dockerfile(s) for dev/prod parity; the image builds the workspace and runs a
  chosen binary identically to the local run.
- No platform-specific assumptions (no Railway/AWS lock-in).

**Out of scope.** Railway provisioning, CD, platform volumes/secrets (QE-311, deferred).

**Acceptance criteria.**
- A clean checkout runs the training pipeline locally from the documented steps and produces
  a vintage.
- Every persistent-state location is configurable; no absolute paths are hard-coded.
- If a Dockerfile is provided, the image runs the same binary as the local run.

`Spec ref: Platform — two decoupled pipelines; decision: run local, defer Railway.`

---

# Phase 1 — Training → backtest validation

## QE-101 — Binance public-dumps downloader
`Phase: P1` · `Area: ① External sources` · `Depends on: QE-010, QE-012`

**Why.** `data.binance.vision` provides the bulk long-range history (vendor == venue).

**Scope / requirements.**
- Download daily/monthly CSV ZIPs for klines, funding, premium index, and `/futures/data/*`
  metrics for the configured universe (default BTCUSDT + ETHUSDT) over **max-available**
  point-in-time history (configurable window).
- Checksum verification, resumable, idempotent; raw files cached locally.
- Schema-drift detection across months.

**Out of scope.** Month-to-date gap (QE-102); fusion (QE-104).

**Acceptance criteria.**
- Re-running the downloader fetches nothing already present and verified.
- Corrupt/checksum-mismatched files are rejected and re-fetched.

`Spec ref: ① Historical CSVs (market-data vendor).`

## QE-102 — Venue REST month-to-date backfill client
`Phase: P1` · `Area: ① External sources` · `Depends on: QE-101`

**Why.** Closes the gap between the vendor's latest published snapshot and "now" so the
training corpus's right edge does not drift stale.

**Scope / requirements.**
- Binance REST fetch for klines/continuousKlines, markPriceKlines, premiumIndexKlines,
  fundingRate/fundingInfo, `/futures/data/*`, covering the vendor-to-now window.
- Paginated, retried, rate-limit-aware (shares the handler later formalised in QE-201).
- Overlap with vendor data is captured for reconciliation (QE-103).

**Out of scope.** Live streaming (QE-202).

**Acceptance criteria.**
- The fused corpus's latest bar is within one bar-interval of "now" at run time.
- Vendor/REST overlap region is retained for diffing.

`Spec ref: ① Venue REST (month-to-date backfill); note on shared venue surface.`

## QE-103 — Data-integrity & source reconciliation validation
`Phase: P1` · `Area: ② Import & fusion` · `Depends on: QE-101, QE-102`

**Why.** *(Reviewer-added, high priority.)* Binance dumps have gaps, dups, out-of-order
rows, schema drift, and shorter coverage for funding/premium/OI. Silent NaN/forward-fill
creates leakage and phantom edge.

**Scope / requirements.**
- Gap detection, monotonic-timestamp checks, duplicate detection per series.
- Coverage maps for funding / premium index / `/futures/data/*` (shorter history flagged).
- Explicit, leakage-safe NaN/forward-fill policy (no fill across gaps beyond a bound).
- Vendor-vs-REST overlap diffing with tolerance; divergence reported.
- Per-vintage data-quality report artefact.

**Out of scope.** Fusion output format (QE-104).

**Acceptance criteria.**
- No silent forward-fill across a gap larger than the configured bound.
- A data-quality report is produced per vintage and fails the run on configured hard violations.

`Spec ref: ② Import & fusion; reviewer: data integrity / coverage.`

## QE-104 — Fusion, normalisation & Arrow serialisation
`Phase: P1` · `Area: ② Import & fusion` · `Depends on: QE-103`

**Why.** Two ingress paths must be coalesced into one normalised, temporally-aligned corpus.

**Scope / requirements.**
- **This ticket fixes the canonical series set** (perps klines, funding, premium index,
  spot klines, futures metrics, spread-to-underlier); fetchers (QE-101/102) stay
  source-abstract and do not hard-code it.
- Daily→monthly coalescence; derived fields (VWAP, split/contract adjustments);
  temporal alignment across series; Arrow record-batch output.
- Deterministic given inputs (QE-006).

**Out of scope.** Persistence (QE-105); indicators (QE-107).

**Acceptance criteria.**
- Fusion is byte-reproducible for fixed inputs.
- Derived fields match hand-computed references on a fixture window.

`Spec ref: ② "Fuse, normalise, serialise… Arrow record-batch output".`

## QE-105 — Persist fused market data to LMDB
`Phase: P1` · `Area: ②→③` · `Depends on: QE-104, QE-010`

**Why.** Downstream signal/WFO/DE stages read the market store, not raw files.

**Scope / requirements.**
- Write fused Arrow batches into the QE-010 schema; idempotent upserts keyed by lineage.

**Out of scope.** Parquet/DuckDB export (QE-135).

**Acceptance criteria.**
- A full ingest→fuse→persist run is reproducible and range-queryable.

`Spec ref: fuse → lmdb_mkt.`

## QE-106 — Multi-resolution bar reconstruction (batch)
`Phase: P1` · `Area: ④ Signal generation` · `Depends on: QE-105, QE-011`

**Why.** Strategies operate across resolutions (e.g. 5m/30m/4h); bars must be reconstructed
deterministically with the same code that runtime will stream.

**Scope / requirements.**
- Base bar = **5m**; deterministically reconstruct **30m + 4h** (reconstructed tier set
  configurable) with deterministic boundaries.
- Output cached to synthetic LMDB; designed for batch + streaming parity (QE-206).

**Out of scope.** Streaming reconstruction (QE-205).

**Acceptance criteria.**
- Batch-reconstructed bars equal streaming reconstruction on the same input (parity fixture).

`Spec ref: ④ "Multi-resolution bar reconstruction · Batch".`

## QE-107 — Indicator catalogue (quantised, deterministic, parity-ready)
`Phase: P1` · `Area: ④ Signal generation (shared)` · `Depends on: QE-106`

**Why.** The catalogue is the single shared module used offline and online; quantised states
are the substrate the strategy genome reasons over.

**Scope / requirements.**
- A **broad starter set (~20+ indicators)** — e.g. MA/EMA, RSI, MACD, ATR, Bollinger, ADX,
  Stochastics, OBV, momentum/returns, plus funding/OI/premium-derived factors — each
  producing **quantised states** with **deterministic lookback** and a **configurable number
  of states** per indicator.
- Built batch+streaming compatible from day one (same code path drives both).
- Catalogue is versioned; each indicator declares its max lookback (feeds purge/embargo).

**Out of scope.** Feature assembly (QE-108); genome (QE-110).

**Acceptance criteria.**
- Each indicator's batch output equals its streaming output bar-for-bar.
- Declared lookback matches actual data dependency (verified).

`Spec ref: ④ / Runtime shared_lib — "Indicator catalogue · Quantised states… Batch + streaming compatible".`

## QE-108 — Feature vector assembly → synthetic store
`Phase: P1` · `Area: ④ Signal generation` · `Depends on: QE-107`

**Why.** WFO/DE consume feature vectors, not raw indicators.

**Scope / requirements.**
- Assemble per-bar feature vectors from quantised indicator states; cache to synthetic LMDB.

**Out of scope.** Strategy evaluation (QE-120).

**Acceptance criteria.**
- Feature vectors are reproducible and parity-safe (batch == streaming).

`Spec ref: ④ "Feature vectors"; features → lmdb_syn.`

## QE-109 — Execution-friction & funding model
`Phase: P1` · `Area: ⑤ WFO (backtest realism)` · `Depends on: QE-105, QE-107`

**Why.** *(Reviewer-added; BLOCKS the backtester.)* For linear perps, fees and funding are
first-order P&L. A frictionless backtest structurally biases the archive toward
high-turnover fee-losers and trend strategies that are net-negative after funding.

**Scope / requirements.**
- Fee schedule (taker/maker by tier), **default Binance USDT-M VIP0: taker 0.05% /
  maker 0.02%**; funding accrual applied to held positions at venue stamps (8h) **from the
  actual historical funding series, not a constant**; spread-cross + size-dependent slippage;
  next-bar-open fill convention. All parameters configurable.
- A cost-sensitivity sweep utility (e.g. 1×/2× assumed costs) used in reporting.

**Out of scope.** Live execution mechanics (QE-217).

**Acceptance criteria.**
- Backtest P&L is net-of-cost and funding-adjusted; a turnover-1 strategy shows fee drag.
- A held-through-funding directional strategy shows the correct funding sign in P&L.
- Cost-sensitivity sweep is available to the validation report (QE-133).

`Spec ref: ⑤ backtesting; spec ingests funding rates; reviewer: frictions first-order.`

## QE-110 — SPIKE: Strategy genome representation
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-107` · **Blocks: QE-118, QE-119, QE-120**

**Why.** Genome was left an open design decision; it must be fixed before archive/operator
work since everything mutates it.

**Scope / requirements (produce a design + decision record).**
- Decide representation: rule sets over quantised indicator states vs fixed-structure
  parameter vector (entry/exit/position conditions + risk + holding params), with rationale.
- Define mutation/crossover surface, validity constraints, and serialisation.
- Provide a reference fixture genome + hand-traced expected decisions.

**Out of scope.** Operator tuning (QE-112).

**Acceptance criteria.**
- A written decision record fixes the genome; a fixture genome evaluates to the documented
  decisions; the representation supports the operators QE-119 will implement.

`Spec ref: Architecture — strategies classified along behavioural dimensions; "quantised states".`

## QE-111 — SPIKE: QD/MAP-Elites archive & behaviour descriptors
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-110` · **Blocks: QE-118**

**Why.** The archive's behaviour descriptors and resolution determine diversity quality and
stability across walk-forward windows.

**Scope / requirements.**
- Choose behaviour descriptors that are **structural/genotype-derived** where possible
  (indicator family, parameterised timescale, max-holding cap) to keep a genome's niche
  stable across windows; justify any outcome-derived descriptor.
- Decide archive resolution tied to a **minimum trades-per-cell** target; adopt Deep-Grid
  sub-populations (Flageat & Cully 2020) for noise robustness.
- Define per-direction archives and how the final ensemble avoids being net-long by construction.

**Out of scope.** Operator selection (QE-112).

**Acceptance criteria.**
- Decision record specifies descriptors, resolution, and sub-population size with rationale.
- A **descriptor-stability** metric is defined: cell-reassignment rate under re-evaluation on
  a different window is below a stated threshold.

`Spec ref: Theory — MAP-Elites, Deep-Grid; per-direction archives.`

## QE-112 — SPIKE: Adaptive operator selection
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-110` · **Blocks: QE-119**

**Why.** Operators (local refine → aggressive explore → fresh random) compete for budget;
the credit-assignment scheme must not leak out-of-sample performance.

**Scope / requirements.**
- Define operator set and a bandit/credit-assignment scheme (multi-emitter MAP-Elites,
  Colas 2020), favouring exploration when sparse, exploitation when dense.
- Credit signal = in-training improvement / archive novelty — **never** OOS/validation reward.

**Out of scope.** Parent selection (QE-121).

**Acceptance criteria.**
- Decision record fixes operators and credit signal; a simulated sparse archive shows the
  scheme shifting budget toward exploration.

`Spec ref: Architecture — Adaptive Operator Selection; Theory — multi-emitter MAP-Elites.`

## QE-113 — SPIKE: Geometric fitness, noise-robust eval & purged/embargoed CV
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-109` · **Blocks: QE-120, QE-117**

**Why.** Fitness must be net-of-cost, robust to the fat-tailed noise of financial series,
and validated without leakage; standard k-fold CV is invalid on autocorrelated series.

**Scope / requirements.**
- Define geometric (time-average growth) fitness on **net-of-cost** equity; document
  sensitivity to near-ruin periods and the compounding resolution.
- Define noise-robust evaluation (multi-window / bootstrap resampling) and how elite
  replacement accounts for standard error (don't replace on a noisy single improvement).
- Specify **purged + embargoed** cross-validation (purge = max indicator lookback + label
  horizon; embargo after each test fold). Standard k-fold is explicitly rejected.

**Out of scope.** Statistical deflation suite (QE-131).

**Acceptance criteria.**
- Decision record defines fitness + CV scheme; a fixture proves train/test bar sets are
  provably disjoint including lookback; documented embargo length.

`Spec ref: ⑤ "Geometric fitness · Cross-validation"; reviewer: purge/embargo.`

## QE-114 — SPIKE: Phased-lifecycle quality gate
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-113` · **Blocks: QE-123**

**Why.** The lifecycle must distinguish exploration vs exploitation and persist only
survivors above a quality threshold.

**Scope / requirements.**
- Define exploration→exploitation transition and survivor persistence rules (exceed
  threshold + survive exploitation phase).
- **Baseline (spec, A1):** the quality threshold is **derived from the full validation
  distribution** per `docs/specs.md` Robustness.
- **Documented alternative (reviewer):** a stricter **train/CV-only** threshold that avoids
  selection leaking the validation distribution into the criterion — recorded as an option to
  revisit if leakage is evidenced; not the baseline.

**Out of scope.** Holdout gate G1 (QE-134).

**Acceptance criteria.**
- Decision record implements the spec's full-validation-distribution threshold as baseline
  and documents the train/CV-only alternative with its rationale.
- A test shows early "lucky" candidates are not persisted.

`Spec ref: Robustness — "quality threshold derived from the full validation distribution".`

## QE-115 — SPIKE: Ensemble discrete differential evolution
`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-113` · **Blocks: QE-126, QE-127**

**Why.** Portfolio search must be tail-aware, wide-basin, and explicitly de-correlated;
behavioural diversity ≠ return-correlation diversity.

**Scope / requirements.**
- Define discrete DE over the strategy pool; tail-aware objective (specify CVaR/CDaR
  estimator and its standard-error caveat); wide-basin (robust plateau) preference.
- Define a correlation/covariance penalty on **net-of-cost** returns and fold CV usage.
- Define a synthetic/stress tail overlay (not empirical tails alone).

**Out of scope.** Capacity analysis (QE-128).

**Acceptance criteria.**
- Decision record fixes objective, estimator, correlation term; a fixture shows two highly
  P&L-correlated strategies are penalised despite behavioural difference.

`Spec ref: ⑥ "Tail-aware returns · Wide-basin optimisation · Discrete differential-evolution".`

## QE-116 — SPIKE: Calibration profile & circuit-breaker model
`Phase: P1` · `Area: ⑥/⑦ + risk` · `Depends on: QE-113` · **Blocks: QE-129, QE-212**

**Why.** The breaker thresholds are calibrated per-vintage before deployment; the breaker
model must be backtestable on history (not first seen live).

**Scope / requirements.**
- Define per-vintage calibration profile contents: per-strategy / per-cohort (slow + fast
  DD) / ensemble fast-drop thresholds.
- **Baseline (spec, A2):** thresholds **calibrated prior to deployment based on observed
  behaviour** (the per-vintage sidecar) per `docs/specs.md` Robustness.
- **Documented alternative (reviewer):** calibrating on an **OOS/stressed** distribution with
  an explicit safety margin — recorded as an option to revisit; not the baseline.
- Define the smoothed-mark (EMA τ½=60s) tick observer driving the equity stream per spec.
  **Documented alternative (reviewer, A3):** an additional unsmoothed **raw-mark fast tier**
  so smoothing can't blind the fast breaker to gap events — recorded as an option, not baseline.
- Make the breaker model runnable inside the WFO harness on history.

**Out of scope.** Runtime breaker wiring (QE-212).

**Acceptance criteria.**
- Decision record specifies the spec-baseline calibration (observed behaviour / per-vintage
  sidecar) and documents the OOS/stressed and raw-mark-fast-tier alternatives.
- A historical replay shows slow/med/fast breakers firing across distinct regimes.

`Spec ref: Robustness — "thresholds calibrated prior to deployment based on observed behaviour"; Runtime — smoothed-mark tick observer.`

## QE-117 — Walk-forward window manager
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-113`

**Why.** Rolling train/validate windows with purge+embargo are the backbone of WFO and
continuous adaptation without catastrophic forgetting.

**Scope / requirements.**
- Generate anchored/rolling train→validate windows; apply purge gap and embargo per QE-113.
- Carry the archive across window transitions (persistence, not reset).

**Out of scope.** Archive internals (QE-118).

**Acceptance criteria.**
- For every window, train and test bar sets are disjoint including lookback.
- The archive persists across transitions; degraded strategies are displaced, not forgotten
  wholesale.

`Spec ref: Architecture — Walk-Forward Validation; "archive persists across window transitions".`

## QE-118 — QD MAP-Elites archive implementation
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-111, QE-117`

**Why.** The archive maintains behavioural diversity by construction (per-direction,
Deep-Grid sub-populations).

**Scope / requirements.**
- Implement per-direction archives with the descriptors/resolution from QE-111 and
  sub-populations for noise robustness; niche sampling for parents.
- Embarrassingly-parallel evaluation across cores (rayon/threads), preserving determinism.

**Out of scope.** Operator credit assignment (QE-119); persistence/quality gate (QE-123).

**Acceptance criteria.**
- Archive fills distinct niches; sub-populations bound per-cell.
- Parallel runs remain deterministic under the seeded harness (QE-006).

`Spec ref: Architecture — QD optimisation; Scalability — embarrassingly parallel.`

## QE-119 — Variation operators + adaptive selection
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-112, QE-118`

**Why.** Operators generate offspring; adaptive selection allocates budget by productivity.

**Scope / requirements.**
- Implement the operator set (local refine, explore, fresh random) over the genome (QE-110)
  and the credit-assignment scheme (QE-112).

**Out of scope.** Backtest evaluation (QE-120).

**Acceptance criteria.**
- Operator budget shifts toward exploration on a sparse archive and exploitation on a dense
  one (matches QE-112 design).

`Spec ref: Architecture — Adaptive Operator Selection.`

## QE-120 — Strategy backtester
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-109, QE-113, QE-118`

**Why.** The fitness engine: evaluates genomes net-of-cost with noise-robust geometric
fitness and a minimum-trade-count floor.

**Scope / requirements.**
- Evaluate a genome over features (QE-108) with frictions/funding (QE-109); compute
  net-of-cost geometric fitness (QE-113); reject elites below a minimum trade count.
- Noise-robust multi-window/bootstrap evaluation feeding archive replacement decisions.

**Out of scope.** Elite robustness gates (QE-124).

**Acceptance criteria.**
- Fitness is net-of-cost; a <N-trade genome is rejected as noise.
- Replacement respects standard error (no replace-on-noise).

`Spec ref: ⑤ "Strategy backtesting · Geometric fitness".`

## QE-121 — Thompson-sampling parent selection
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-118`

**Why.** Bayesian parent selection under fitness uncertainty; reward must avoid OOS leakage.

**Scope / requirements.**
- Thompson sampling over parent niches; reward = in-training improvement / novelty, never
  validation performance.

**Out of scope.** Operator selection (QE-119).

**Acceptance criteria.**
- Parent selection demonstrably uses no held-out validation signal (leakage test).

`Spec ref: Theory — Thompson Sampling; reviewer: no OOS in the bandit reward.`

## QE-122 — Behavioural regularisation
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-118`

**Why.** Keeps the archive behaviourally regular/diverse and counters degenerate crowding.

**Scope / requirements.**
- Implement the regularisation defined in QE-111 (e.g. niche penalties / novelty pressure).

**Out of scope.** Persistence (QE-123).

**Acceptance criteria.**
- Archive diversity metric improves vs an ablation without regularisation on a fixture run.

`Spec ref: ⑤ "Behavioural regularisation".`

## QE-123 — Phased recording → Strategy repository
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-114, QE-120`

**Why.** Persist only exploitation-phase survivors above the train/CV-derived quality
threshold, into the strategy repository.

**Scope / requirements.**
- Implement the lifecycle/quality gate (QE-114); write survivors to the strategy repository
  with lineage.

**Out of scope.** Ensemble construction (QE-126).

**Acceptance criteria.**
- Early lucky candidates are not persisted; persisted strategies carry lineage.

`Spec ref: Robustness — phased lifecycle; ⑤ strategy_repo.`

## QE-124 — Elite robustness gates
`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-120`

**Why.** *(Reviewer-added.)* Evolution overfits efficiently; elites must survive perturbation
and re-evaluation to be trusted.

**Scope / requirements.**
- Reject/flag elites failing: minimum-trade-count, parameter-perturbation robustness
  (survive ±ε genome jitter), descriptor-stability-under-reevaluation (QE-111 metric).

**Out of scope.** Statistical deflation (QE-131).

**Acceptance criteria.**
- An elite that collapses under ±ε jitter or has unstable descriptors is rejected/flagged.

`Spec ref: reviewer: overfitting defences.`

## QE-125 — Regime labelling
`Phase: P1` · `Area: ⑤/⑥ support` · `Depends on: QE-106`

**Why.** *(Reviewer-added.)* "Regime-sensitive" optimisation and reporting are aspirational
without regime tags; needed so the ensemble is required to work across regimes, not just on
blended history.

**Scope / requirements.**
- Produce regime labels (e.g. vol state / trend-vs-chop, or a simple HMM) over history.
- Expose labels to the DE objective (QE-127) and validation reporting (QE-133).

**Out of scope.** Strategy genome conditioning on regimes.

**Acceptance criteria.**
- A per-regime expectancy table can be produced for any strategy/ensemble.

`Spec ref: Overview — regime change; reviewer: regime labelling.`

## QE-126 — Discrete DE portfolio search
`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-115, QE-123`

**Why.** Assembles ensembles from the strategy pool with a tail-aware, wide-basin objective.

**Scope / requirements.**
- Implement discrete DE per QE-115; fold cross-validation; net-of-cost candidate scoring.

**Out of scope.** Correlation/regime constraints (QE-127); capacity (QE-128).

**Acceptance criteria.**
- DE converges to robust-basin portfolios on a fixture; scoring is net-of-cost.

`Spec ref: ⑥ "Portfolio search · Discrete differential-evolution".`

## QE-127 — Correlation penalty + per-regime expectancy constraint
`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-126, QE-125`

**Why.** *(Reviewer-added.)* Enforce return-correlation diversity and require the ensemble to
be net-positive in each labelled regime, not only on blended history.

**Scope / requirements.**
- Add the correlation/covariance penalty (QE-115) to the DE objective.
- Constrain/score on per-regime expectancy using QE-125 labels.

**Out of scope.** Capacity gating (QE-128).

**Acceptance criteria.**
- Highly P&L-correlated combinations are penalised; a per-regime expectancy table is part of
  the ensemble's score; a regime-fragile ensemble is rejected/penalised.

`Spec ref: Robustness — tail risk and regime sensitivity; reviewer: correlation/regime.`

## QE-128 — Capacity analysis gating ensemble weights
`Phase: P1` · `Area: ⑥ Ensemble` · `Depends on: QE-126, QE-109`

**Why.** *(Reviewer-added.)* Weights are fiction at size if per-strategy capacity is ignored
— a high-turnover scalper may have edge at $10k and none at $1M.

**Scope / requirements.**
- Estimate per-strategy capacity (impact model × turnover × target AUM) and cap weights to
  respect capacity at the configured AUM.

**Out of scope.** Live impact measurement.

**Acceptance criteria.**
- A high-turnover strategy's ensemble weight is capped by its modelled capacity at target AUM.

`Spec ref: reviewer: capacity analysis.`

## QE-129 — Ensemble repository, calibration profile & vintage artefact format
`Phase: P1` · `Area: ⑦ Vintage outputs` · `Depends on: QE-127, QE-116, QE-006`

**Why.** The vintage (chromosomes + ensemble + calibration) is the unit handed to runtime;
its format and versioning underpin reproducibility and rollover.

**Scope / requirements.**
- Define and write the ensemble repository + per-vintage calibration profile (QE-116) +
  vintage artefact format with a content hash and full lineage.
- Format is read-only-loadable by runtime (QE-219).

**Out of scope.** Runtime consumption (QE-219).

**Acceptance criteria.**
- A vintage round-trips (write → load) and its hash is stable and verifiable.

`Spec ref: ⑦ Ensemble repository + Calibration profile; Platform — per-vintage artefacts.`

## QE-130 — Stress / worst-case-loss scenarios
`Phase: P1` · `Area: ⑥/risk` · `Depends on: QE-127`

**Why.** *(Reviewer-added.)* "Tail-aware returns" optimises the distribution; it does not
bound worst-case capital loss. A pre-declared max-loss needs evidence.

**Scope / requirements.**
- Run candidate ensembles through historical crash windows + synthetic shocks (gap,
  funding-spike, ADL); produce a worst-case-loss figure per vintage.

**Out of scope.** Live margin enforcement (QE-215).

**Acceptance criteria.**
- Each vintage carries a worst-case-loss figure under the stated stress set, feeding G3 (QE-308).

`Spec ref: reviewer: worst-case-loss / liquidation tail.`

## QE-131 — Statistical robustness suite
`Phase: P1` · `Area: ⑤/⑥ validation` · `Depends on: QE-120, QE-126`

**Why.** *(Reviewer-added; part of the milestone definition-of-done.)* A large QD archive is
a multiple-testing machine; an undeflated OOS Sharpe is the expected output of the search,
not evidence of edge.

**Scope / requirements.**
- Deflated Sharpe Ratio with effective trials = archive cells × generations × windows.
- PBO via CSCV; White's Reality Check / Hansen's SPA vs a best-of-N null.
- Benchmark/null comparison: BTC-HODL and turnover-matched random-entry nulls.

**Out of scope.** Reporting layout (QE-133).

**Acceptance criteria.**
- The suite computes DSR/PBO/SPA for a vintage; results gate G1 (QE-134).

`Spec ref: Theory — published QD/AOS methods; reviewer: deflation / data-snooping.`

## QE-132 — Information-firewall CI guard
`Phase: P1` · `Area: cross-cutting` · `Depends on: QE-123, QE-126`

**Why.** The firewall (search ⟂ portfolio ⟂ live) is an architectural invariant; make it a
test, not a convention.

**Scope / requirements.**
- A CI/architectural test asserting WFO cannot read portfolio/live outcomes and ensemble
  cannot read live outcomes (no forbidden dependencies / data paths).

**Out of scope.** Runtime firewall direction (covered by crate topology QE-001).

**Acceptance criteria.**
- Introducing a forbidden dependency fails CI.

`Spec ref: Robustness — "strict information barrier… firewall extends downstream".`

## QE-133 — Validation reporting
`Phase: P1` · `Area: ⑤/⑥/⑧ support` · `Depends on: QE-131, QE-125, QE-128, QE-109`

**Why.** A human-readable, per-vintage evidence pack for the G1 decision.

**Scope / requirements.**
- Report: net-of-cost performance, cost-sensitivity (1×/2×) sweep, DSR/PBO/SPA, per-regime
  expectancy, pairwise return-correlation distribution, capacity at target AUM, worst-case loss.

**Out of scope.** Interactive viewer (QE-136).

**Acceptance criteria.**
- A single per-vintage report contains all the above and is reproducible.

`Spec ref: ⑧ diagnostics; reviewer: cost sensitivity / nulls / regimes.`

## QE-134 — GATE G1: Holdout embargo & over-fit acceptance
`Phase: P1` · `Area: gate` · `Depends on: QE-133` · **Blocks: all of Phase 2**

**Why.** Phase 1 is "validated" only when a vintage clears an **untouched** holdout that no
prior P1 ticket (gating, parent selection, operator credit) was allowed to read.

**Scope / requirements.**
- Maintain a final time-blocked OOS slice never touched by training/selection.
- Promotion requires: net-of-cost edge persists; DSR > deflated threshold; SPA beats the
  best-of-N null at the stated significance; OOS metrics within pre-registered tolerance of
  in-sample.

**Out of scope.** Live trust gates (QE-222, QE-308).

**Acceptance criteria.**
- A vintage failing any G1 criterion is not promoted; pass/fail is recorded with evidence.

`Spec ref: Overview — "honest estimate of live-deployment performance"; reviewer: G1.`

## QE-135 — Parquet export + DuckDB analytics  *(deferred)*
`Phase: P1 (deferred)` · `Area: ③/⑧` · `Depends on: QE-105`

**Why.** Ad-hoc analytics and diagnostic export; useful, not on the validity path.
Reviewers: defer until the methodology core works.

**Scope / requirements.**
- Parquet export of fused/synthetic data; DuckDB views (incl. JSONND) for ad-hoc analytics.

**Out of scope.** Required validation reporting (QE-133 stands alone).

**Acceptance criteria.**
- Parquet export round-trips; DuckDB views query the export.

`Spec ref: ③ fs_parquet / duckdb; ⑧ viewer feeds.`

## QE-136 — Signal viewer  *(deferred)*
`Phase: P1 (deferred)` · `Area: ⑧ Diagnostic tooling` · `Depends on: QE-135, QE-107`

**Why.** Operator/researcher diagnostics; reconstructs signals online for inspection.

**Scope / requirements.**
- Viewer reading DuckDB and reconstructing features online via the shared catalogue.

**Out of scope.** Cockpit (QE-304).

**Acceptance criteria.**
- The viewer reproduces a stored signal online and matches the cached value.

`Spec ref: ⑧ sig_viewer.`

---

# Phase 2 — Runtime → paper/sim   *(gated by G1)*

## QE-201 — Venue-aware REST client (rate-limit + ephemeral cache)
`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-004`

**Why.** All REST ingress flows through a venue-aware rate-limit handler; closed-window
historical responses are immutable and cacheable.

**Scope / requirements.**
- Rate-limit handler honouring venue weights; paginated + retried fetchers.
- Ephemeral REST cache (read-through + write-back) for closed-window historical responses,
  sitting below the rate-limit handler.

**Out of scope.** wss (QE-202/203).

**Acceptance criteria.**
- Rate-limit pressure backs off without dropping requests; closed-window responses are
  served from cache on repeat.

`Spec ref: Runtime — "venue-aware rate-limit handler"; ephemeral REST cache note.`

## QE-202 — wss Market-tier streams + connection registry
`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-004`

**Why.** The Hedge-Planner data path consumes Market-tier streams (kline + markPrice@1s)
via a tier-partitioned connection registry.

**Scope / requirements.**
- Subscribe kline (5m/30m/4h) + markPrice@1s; tier-partitioned websocket connection registry;
  reconnection/resubscribe with gap handling.

**Out of scope.** Realtime-tier (QE-203).

**Acceptance criteria.**
- Disconnect/reconnect resubscribes and reports any gap; Market and Realtime tiers are
  partitioned in the registry.

`Spec ref: ② wss Market tier; "tier-partitioned websocket connection registry".`

## QE-203 — wss Realtime-tier streams
`Phase: P2` · `Area: ② Market observables` · `Depends on: QE-202`

**Why.** The Edge gateway's execution mechanics consume Realtime-tier streams (bookTicker,
depth20@100ms) plus aggTrade.

**Scope / requirements.**
- Subscribe bookTicker, depth20@100ms, aggTrade via the registry; disjoint from the
  Hedge-Planner data path (shared no upstream path with QE-202 by construction).

**Out of scope.** Order submission (QE-217).

**Acceptance criteria.**
- Edge-side and Planner-side streams share no upstream data path (verified).

`Spec ref: Runtime — disjoint sets; ② wss Realtime.`

## QE-204 — User-data stream subscription
`Phase: P2` · `Area: ②/⑥ private` · `Depends on: QE-201`

**Why.** Fills, position reports, and heartbeat are the authoritative ground-truth feed for
the Position keeper.

**Scope / requirements.**
- Subscribe the subaccount-scoped private user-data stream (fills + positions + heartbeat);
  reconnect with listen-key renewal; simulator equivalent for sim mode.

**Out of scope.** Position keeper logic (QE-217).

**Acceptance criteria.**
- Fills/positions/heartbeat are delivered in order; a dropped stream reconnects without losing
  position truth (re-snapshot on reconnect).

`Spec ref: Runtime — "subscribes to user-data stream (fills + positions + heartbeat)".`

## QE-205 — Streaming bar reconstruction + live kline source
`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-202, QE-106`

**Why.** Live multi-resolution bars must be reconstructed by streaming, primed by REST and
stitched to wss, using the same reconstruction as batch.

**Scope / requirements.**
- Live kline source: REST prime + wss stitch; streaming multi-resolution reconstruction.

**Out of scope.** Factor join (QE-206).

**Acceptance criteria.**
- Streaming bars equal batch reconstruction on replayed data (parity with QE-106).

`Spec ref: ④ live_kline / live_resolve.`

## QE-206 — Factor join + batch/streaming parity tests
`Phase: P2` · `Area: ④ / shared` · `Depends on: QE-205, QE-107`

**Why.** Parity by construction is the whole point of the shared catalogue; this proves it
end-to-end on the live path.

**Scope / requirements.**
- Live factor join producing factor rows via the shared catalogue; a parity test suite
  comparing batch vs streaming factor rows.

**Out of scope.** Evaluator session (QE-207).

**Acceptance criteria.**
- Live factor rows equal offline feature vectors bar-for-bar on shared fixtures.

`Spec ref: Runtime — "guaranteeing batch / streaming parity by construction".`

## QE-207 — Evaluator session (shared replay + live modes)
`Phase: P2` · `Area: ③/④` · `Depends on: QE-206, QE-129`

**Why.** One evaluator session runs through bootstrap (replay) and live (wss continuation) —
no new object, no state copy.

**Scope / requirements.**
- A single evaluator session supporting replay and live modes; bar-evaluation on close;
  loads vintage (chromosomes/ensemble/calibration) read-only.

**Out of scope.** Cutover orchestration (QE-211).

**Acceptance criteria.**
- The same session instance transitions replay→live without state copy; decisions are
  identical across the boundary on a fixture.

`Spec ref: Runtime notes — "same evaluator session runs through Bootstrap and Live".`

## QE-208 — Mark EMA loop + tick observer
`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-202`

**Why.** Slow-DD probing rides a smoothed mark (EMA τ½=60s); the tick observer feeds the
breaker layer.

**Scope / requirements.**
- EMA loop (τ½=60s) on markPrice@1s; tick observer on smoothed mark for the slow-DD probe
  (spec baseline). A raw-mark fast-tier tick is a documented alternative per QE-116 (build
  only if that spike adopts it).

**Out of scope.** Breaker logic (QE-212).

**Acceptance criteria.**
- EMA half-life is correct; both smoothed and raw mark ticks are available to breakers.

`Spec ref: ④ live_mark "Mark EMA loop · τ½ = 60s"; tick observer.`

## QE-209 — Bootstrap pipeline
`Phase: P2` · `Area: ③ Bootstrap` · `Depends on: QE-201, QE-207`

**Why.** On startup, replay the lookback window through the evaluator to reconstruct state
to where a continuously-running planner would hold it.

**Scope / requirements.**
- REST fetchers (paginated + retried) → ephemeral cache → multi-resolution replay → factor
  merge + markPrice replay (1-min cadence) → evaluator in replay mode.

**Out of scope.** Reconstructed-state object (QE-210); cutover (QE-211).

**Acceptance criteria.**
- A cold start reconstructs per-strategy state deterministically from REST historicals.

`Spec ref: ③ Bootstrap pipeline; Runtime — "replays the lookback window through the same evaluator".`

## QE-210 — Reconstructed state
`Phase: P2` · `Area: ③ Bootstrap` · `Depends on: QE-209`

**Why.** Bootstrap output: per-strategy positions, dormancy latches, and **committed peak
equity** — the last is load-bearing for drawdown breakers.

**Scope / requirements.**
- Produce per-strategy positions, dormancy latches, committed peak equity; peak must be the
  **true** committed peak, not a windowed peak (else breakers are mis-anchored).

**Out of scope.** Restart parity test (QE-220).

**Acceptance criteria.**
- Reconstructed committed peak equals the true historical peak on a fixture longer than the
  bootstrap window.

`Spec ref: ③ boot_state; reviewer: true vs windowed peak.`

## QE-211 — Bootstrap→live in-process cutover
`Phase: P2` · `Area: ③→④` · `Depends on: QE-210, QE-207`

**Why.** The handoff from replay to live is in-process — bootstrap and live share state.

**Scope / requirements.**
- Switch the evaluator session in place from replay to wss continuation at cutover; no state
  copy, no gap/overlap in evaluated bars.

**Out of scope.** Netting (QE-213).

**Acceptance criteria.**
- Cutover produces no duplicated or skipped bar; post-cutover decisions match a
  continuously-running reference on a fixture.

`Spec ref: Runtime notes — "in-process handoff at cutover".`

## QE-212 — Circuit-breaker layer
`Phase: P2` · `Area: ④ Live pipeline / risk` · `Depends on: QE-116, QE-208, QE-210`

**Why.** Per-strategy/direction/ensemble limits + slow/med/fast DD thresholds clamp gated
strategies to flat before netting.

**Scope / requirements.**
- Implement the breaker model (QE-116) consuming the calibration profile and the equity
  stream (smoothed mark per spec; raw-mark fast tier only if QE-116 adopts it); clamp gated
  strategies to flat before netting.

**Out of scope.** Pre-trade caps (QE-215); kill-switch (QE-216).

**Acceptance criteria.**
- A breach clamps the affected scope to flat before netting; behaviour matches the historical
  backtest (QE-116) on replay (raw-mark fast-tier behaviour only if QE-116 adopted it).

`Spec ref: ④ live_breakers; Robustness — layered circuit breakers.`

## QE-213 — Position netting
`Phase: P2` · `Area: ④ Live pipeline` · `Depends on: QE-212`

**Why.** Per-bar decisions net into a single aggregate target position.

**Scope / requirements.**
- Net per-strategy (post-breaker) decisions into one aggregate target per instrument.

**Out of scope.** Hedge Planner (QE-214).

**Acceptance criteria.**
- Netting equals the sum of post-breaker per-strategy targets; gated strategies contribute zero.

`Spec ref: ④ live_netter "Position netting (per-bar evaluation)".`

## QE-214 — Hedge Planner (target-position)
`Phase: P2` · `Area: ⑤ Hedge Planning` · `Depends on: QE-213`

**Why.** Emits absolute target positions; stateless with respect to current position; tracks
equity and buying power.

**Scope / requirements.**
- Emit absolute target positions from netted targets; maintain an independent equity +
  available-margin view (capital allocation) sourced from the position keeper; surface to cockpit.
- Stateless wrt current position (the architectural benefit of target-based hedging).

**Out of scope.** Venue delta translation (QE-217).

**Acceptance criteria.**
- The planner emits identical targets regardless of current venue position (statelessness test).
- Equity/margin view matches keeper truth.

`Spec ref: ⑤ hedger; Runtime — "stateless with respect to current position".`

## QE-215 — Pre-trade risk check
`Phase: P2` · `Area: risk (netting→hedger boundary)` · `Depends on: QE-009, QE-214, QE-130`

**Why.** *(Reviewer-added.)* Leveraged perps need hard pre-trade caps and a
liquidation-distance floor; "tail-aware" optimisation does not bound live worst-case loss.

**Scope / requirements.**
- Enforce QE-009 limits before targets leave the planner: max notional, max leverage, gross/net
  caps, **liquidation-distance floor**, margin-utilisation ceiling. Clamp or halt per contract.

**Out of scope.** Out-of-band kill (QE-216).

**Acceptance criteria.**
- A target implying an unsafe liquidation distance or breaching a cap is clamped/halted, not sent.

`Spec ref: Robustness — circuit breakers; reviewer: pre-trade margin/leverage governor.`

## QE-216 — Out-of-band kill-switch at venue adapter
`Phase: P2` · `Area: ⑥ Edge gateway / risk` · `Depends on: QE-009, QE-217`

**Why.** *(Reviewer-added.)* A cockpit button dependent on the cockpit process is not a
kill-switch; the halt must be out-of-band, at the order-submission layer, deterministic.

**Scope / requirements.**
- Implement the QE-009 kill contract at the venue adapter: flatten-and-halt, independent of
  cockpit and Hedge Planner; independently testable trigger.

**Out of scope.** Alerting (QE-305).

**Acceptance criteria.**
- Triggering the kill flattens positions and halts submission even with the cockpit/planner down.

`Spec ref: Runtime — Edge gateway submits orders; reviewer: out-of-band kill.`

## QE-217 — Venue adapter / Position keeper / order lifecycle + simulator
`Phase: P2` · `Area: ⑥ Edge gateway` · `Depends on: QE-203, QE-204, QE-007`

**Why.** Translates absolute targets into venue-native deltas against kept position; the
keeper is fed by the authoritative user-data stream; a simulator enables paper/sim mode.

**Scope / requirements.**
- Translate targets → venue-native order deltas; track order lifecycle; Position keeper
  absorbs fills/position reports as ground truth (never infers position).
- Simulator mode (in-memory ledger reserved for sim; live cash is venue-side).

**Out of scope.** gRPC wiring (QE-218).

**Acceptance criteria.**
- Targets become correct venue deltas vs kept position; keeper state tracks venue reports;
  sim mode runs the full loop with no real orders.

`Spec ref: ⑥ router "Venue adapter · Position keeper"; Runtime — position reports authoritative.`

## QE-218 — gRPC transport (Hedge Planner ↔ Edge gateway)
`Phase: P2` · `Area: ⑤↔⑥` · `Depends on: QE-214, QE-217`

**Why.** Decisions flow planner→adapter over gRPC; fills/positions/heartbeat flow back.

**Scope / requirements.**
- gRPC service: planner emits target revisions; adapter returns fills + position reports +
  heartbeat/venue-health. Backpressure and reconnection handled.

**Out of scope.** Journal append (QE-301).

**Acceptance criteria.**
- A target revision reaches the adapter and fills/positions return; the append path (QE-301)
  never gates this dispatch.

`Spec ref: Runtime — "flow into the Hedge Planner → Venue adapter chain over gRPC".`

## QE-219 — Vintage load (read-only) + rollover
`Phase: P2` · `Area: ① Vintage inputs` · `Depends on: QE-129, QE-207`

**Why.** Runtime loads the ensemble repo + calibration profile read-only at startup; periodic
rollover replaces the vintage in place.

**Scope / requirements.**
- Read-only vintage load at startup; in-place rollover that swaps repo + calibration when
  training emits a new vintage, without violating the firewall.

**Out of scope.** Training emission (QE-129).

**Acceptance criteria.**
- Startup loads a vintage read-only; a rollover swaps it atomically with lineage recorded.

`Spec ref: Runtime — ① Vintage inputs; "Vintage rollover" dashed edge.`

## QE-220 — Bootstrap/restart parity test
`Phase: P2` · `Area: ③ risk` · `Depends on: QE-210, QE-211`

**Why.** *(Reviewer-added.)* If reconstructed peak/state diverges from continuous state, every
drawdown breaker is mis-anchored — a capital-risk event, not just a feature.

**Scope / requirements.**
- A test asserting bootstrap-reconstructed state equals continuously-running state on the
  breaker-relevant fields (committed peak, dormancy latches, positions).

**Out of scope.** Breaker logic (QE-212).

**Acceptance criteria.**
- Reconstructed vs continuous state match bit-for-bit on breaker-relevant fields.

`Spec ref: Runtime — stateless critical path / reconstruct on restart; reviewer: restart parity.`

## QE-221 — Real-time reconciliation divergence alarm
`Phase: P2` · `Area: ⑨ + risk` · `Depends on: QE-217`

**Why.** *(Reviewer-added.)* Reconciliation should not be post-hoc only; a live journal-vs-venue
mismatch beyond tolerance should be a fast safety check that can trip the kill-switch.

**Scope / requirements.**
- Periodically compare kept position / expected vs venue truth; on divergence beyond tolerance,
  alarm and optionally trip QE-216.

**Out of scope.** Cold-path attribution (QE-302).

**Acceptance criteria.**
- An injected position desync beyond tolerance raises an alarm and can halt.

`Spec ref: Runtime — position reports authoritative; reviewer: real-time divergence guard.`

## QE-222 — GATE G2: Live shadow / dry-run
`Phase: P2` · `Area: gate` · `Depends on: QE-218, QE-221` · **Blocks: Phase 3 live capital**

**Why.** *(Reviewer-added.)* Before any capital, run the full loop against live data computing
**would-be** orders with no submission, reconciled vs simulator — catching wss-stitch,
mark-EMA, netting, and cutover bugs.

**Scope / requirements.**
- Shadow mode: full pipeline on live data; Edge gateway logs would-be orders without
  submitting; reconcile against simulator expectations.

**Out of scope.** Go/no-go sign-off (QE-308).

**Acceptance criteria.**
- A shadow run over a defined live period produces would-be orders that reconcile with the
  simulator within tolerance; no orders are submitted.

`Spec ref: reviewer: live shadow / dry-run gate.`

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

## QE-251 — `qe-cli backtest` job (run a vintage over a window)
`Phase: PreP3` · `Area: runnable jobs / cli` · `Depends on: QE-129, QE-120, QE-107, QE-108, QE-105`

**Why.** The pipeline is not yet runnable end-to-end from a command; the admin UI needs a real
backtest to trigger, stream, and display. This wires the existing backtester into a deterministic
CLI job (decision D1: backtest a sealed **vintage**, not hand-typed parameters).

**Scope / requirements.**
- New `qe-cli backtest --vintage <id> [--strategy <chromosome>] --start --end --resolution
  [--universe] [--taker-fee-bps] [--slippage-model] --run-dir [--json]`.
- Load+`verify()` the vintage (`qe-vintage`), read OHLCV bars for the window (`qe-storage` `scan_bars`).
- **Feature engineering (required bridge):** `qe_wfo::backtest::backtest` consumes a *decision* bar
  (`qe_wfo::backtest::Bar { features: FeatureVector, price, funding_rate }`), **not** raw
  `qe_domain::Bar`. Build decision bars via `qe_signal::feature::assemble_batch` using the vintage's
  catalogue schema (must match how the genomes were evolved — the genome addresses states by schema
  order); factors from `scan_funding`/`scan_premium`. Then run each chromosome through `backtest` and
  weight-aggregate to ensemble returns.
- Compute the result contract (equity curve, drawdown, CAGR, Sharpe, Sortino, monthly returns) and
  write `result.json` into `--run-dir`.
- Emit **JSON-line progress** on stdout (`load/scan/features/simulate/report` → `done`), deterministic.

**Out of scope.** Trade-level metrics (QE-252); server/subprocess supervision (QE-255).

**Acceptance criteria.**
- A backtest over a committed fixture vintage + sample store produces a deterministic `result.json`
  matching a golden file; the progress stream ends with `{"t":"done"}`.

`Spec ref: admin-ui spec §5.2, §8.1; plan Tasks 1–3, 5.`

## QE-252 — Backtester trade-level recording (trades, win-rate, profit-factor)
`Phase: PreP3` · `Area: runnable jobs / wfo` · `Depends on: QE-120`

**Why.** `BacktestResult` emits per-bar returns and a trade **count** only — the design's Trades
tab and the win-rate / profit-factor / Sortino metrics need per-trade data. Close the gap without
disturbing the existing hot-path result.

**Scope / requirements.**
- Add `qe_wfo::backtest::backtest_with_trades(genome, bars, cfg) -> (BacktestResult, Vec<TradeFill>)`;
  keep `backtest()` delegating and discarding trades (identical `returns`/`net_pnl`).
- `TradeFill { entry_idx, exit_idx, side, entry_px, exit_px, return_frac }` per closed round-trip.
- CLI maps `TradeFill → TradeRow`; add `win_rate` and `profit_factor` (Σgains/|Σlosses|, `∞` on no
  losses, documented).

**Out of scope.** Attribution / per-symbol P&L breakdown (Phase 3, QE-303).

**Acceptance criteria.**
- A known single winning round-trip yields exactly one `TradeFill` with `return_frac > 0`; the
  existing `qe-wfo` suite still passes unchanged; `win_rate`/`profit_factor` match hand-computed values.

`Spec ref: admin-ui spec §8.1 (trades); plan Task 4.`

## QE-253 — `qe-cli ingest` scaffold + sample store fixture + coverage query
`Phase: PreP3` · `Area: runnable jobs / storage` · `Depends on: QE-101, QE-102, QE-105`

**Why.** A backtest needs data in the LMDB store; ingestion isn't wired into any command (decision
D3). Provide a minimal populate path + a read-only coverage query the UI's Market-data view uses.

**Scope / requirements.**
- New `qe-cli ingest --config --start --end --resolution` populating a `MarketStore` from the
  injectable `HistoricalSource` seam (real Binance decoders stay behind the default-off `http`
  feature — out of scope here).
- `coverage(store, instruments) -> Vec<CoverageRow{symbol, resolution, from, to, bars}>`.
- A committed deterministic **sample store fixture** so backtests/tests run offline.

**Out of scope.** Real network ingestion (behind `http`); UI-triggered ingest (Phase 3).

**Acceptance criteria.**
- `coverage()` over the sample store returns the expected symbol/range/bar-count rows; `ingest`
  populates a store from an in-memory source in a test.

`Spec ref: admin-ui spec §5.1, §6.2 (coverage); plan Task 6.`

## QE-254 — `qe-server` crate scaffold (axum + tokio, static SPA, firewall guard)
`Phase: PreP3` · `Area: backend / server` · `Depends on: QE-001`

**Why.** The admin UI needs a backend. A new `qe-server` crate is a **second composition root**
(decision D4a): all-Rust, async isolated to this crate, reusing the engine crates.

**Scope / requirements.**
- New `crates/server` (`qe-server`): axum + tokio; health route; serve built SPA static assets at
  `/` and reserve `/api`.
- Depends only on training-side + shared crates; **must not** depend on `qe-runtime`/`qe-venue`.
- Extend the QE-132 firewall / QE-001 decoupling test to assert `qe-server` pulls in no forbidden
  edge (no `qe-runtime`/`qe-venue`).

**Out of scope.** Run lifecycle (QE-255), auth (QE-256).

**Acceptance criteria.**
- `qe-server` builds and serves a health endpoint + a static index; the firewall/decoupling tests
  pass and now cover `qe-server`.

`Spec ref: admin-ui spec §4, §6; ADR D4a.`

## QE-255 — Run store + run lifecycle API + subprocess supervision
`Phase: PreP3` · `Area: backend / orchestration` · `Depends on: QE-251, QE-254`

**Why.** Trigger runs, track status/progress, serve results (decisions D4b file store, D4c
subprocess supervision).

**Scope / requirements.**
- File-based run store `data/runs/<id>/{meta.json, result.json, stdout.log}` + `index.json`.
- `POST /api/runs` (create+spawn `qe-cli backtest` subprocess), `GET /api/runs`,
  `GET /api/runs/:id` (status+progress), `GET /api/runs/:id/result`.
- Supervise the subprocess: tail JSON-line progress into `meta.json`; nonzero exit ⇒ `failed` with
  captured stderr tail. Bounded worker pool; excess ⇒ `queued`.

**Out of scope.** Auth gating (QE-256) — added on top.

**Acceptance criteria.**
- Creating a run spawns the job, transitions `queued→running→succeeded`, and the result endpoint
  serves the `result.json`; a failing job yields `failed` with an error message.

`Spec ref: admin-ui spec §6.1, §6.2; ADR D4b/D4c.`

## QE-256 — Google OAuth + email allowlist + signed session
`Phase: PreP3` · `Area: backend / auth` · `Depends on: QE-254`

**Why.** Everything must be gated (decision D4d): Google sign-in restricted to an env allowlist.

**Scope / requirements.**
- Authorization-Code flow: `/api/auth/login` → Google → `/api/auth/callback` (verify ID token:
  signature, `aud`, `iss`, expiry, `email_verified`).
- Gate on `email ∈ QE_ADMIN_ALLOWED_EMAILS` (comma-separated, trimmed, case-insensitive) → else 403.
- HTTP-only `Secure` `SameSite=Lax` **signed session cookie**; verified on every `/api` call;
  `GET /api/me` returns the email or 401. Env: client id/secret, redirect uri, session secret.

**Out of scope.** Multi-user roles (Phase 3+).

**Acceptance criteria.**
- No session ⇒ 401; a valid Google login not on the allowlist ⇒ 403; an allowlisted login ⇒ 200 and
  `/api/me` returns the email (tested with a mocked OAuth verifier).

`Spec ref: admin-ui spec §6.3, §6.4; ADR D4d.`

## QE-257 — Vintages + market-data coverage read APIs
`Phase: PreP3` · `Area: backend / api` · `Depends on: QE-253, QE-254`

**Why.** The trigger form needs the list of backtestable vintages; the Market-data view needs
read-only coverage.

**Scope / requirements.**
- `GET /api/vintages` (list sealed vintages from the artifacts dir with id/label/summary).
- `GET /api/market-data/coverage` (from QE-253 `coverage()`).

**Out of scope.** UI-triggered ingest (Phase 3).

**Acceptance criteria.**
- Both endpoints return the fixture vintages / sample-store coverage; both require a valid session.

`Spec ref: admin-ui spec §6.2.`

## QE-258 — Frontend scaffold + design-system port (Vite/React, AppShell, Login)
`Phase: PreP3` · `Area: frontend` · `Depends on: QE-256`

**Why.** Stand up the SPA and port the Claude Design system faithfully (decision: `web/` Vite app).

**Scope / requirements.**
- `web/` Vite + React app; port the design tokens + primitives + `AppShell`; Lucide icons; the
  three brand fonts.
- **Login** screen (net-new): brand lockup, "Sign in with Google" → `/api/auth/login`,
  allowlist-rejection state. App shell with the Research nav group; Trade/Risk items disabled.
- Build into static assets `qe-server` serves.

**Out of scope.** Backtest screens (QE-259).

**Acceptance criteria.**
- The SPA builds; unauthenticated load shows Login; after a mocked session the shell renders with
  the Research nav; the design system matches the Claude Design tokens.

`Spec ref: admin-ui spec §7.1, §7.2 (login/shell); ADR D4a.`

## QE-259 — Backtest screens wired to the API
`Phase: PreP3` · `Area: frontend` · `Depends on: QE-255, QE-257, QE-258`

**Why.** The core user journey: trigger a backtest, watch progress, review results.

**Scope / requirements.**
- **Backtests (list)** from `GET /api/runs`; **New backtest** form (vintage/window/resolution/
  universe/costs) → `POST /api/runs`; **Backtest result** (port `BacktestResearch.jsx`) data-driven
  from `GET /api/runs/:id/result`, with the progress card polling `GET /api/runs/:id` while running;
  **Market-data coverage** (read-only) from `GET /api/market-data/coverage`.
- Genome params render read-only (decision D1); "Re-run" clones params into a new run.

**Out of scope.** Training monitor (QE-261); live Trade/Risk surfaces (Phase 3).

**Acceptance criteria.**
- A backtest can be triggered from the UI, its progress polled to completion, and the full result
  contract rendered (metrics strip, equity/drawdown, monthly heatmap, trades table).

`Spec ref: admin-ui spec §7.2, §7.3, §8.1.`

## QE-260 — Runnable `qe-cli train` search job + rich progress  *(fast-follow)*
`Phase: PreP3` · `Area: runnable jobs / wfo` · `Depends on: QE-118, QE-120, QE-126, QE-134`

**Why.** Extend the platform to **training** (decision D2: after backtest). Wire the WFO/MAP-Elites
search + ensemble + G1 gate into a real runnable job (today `train` only writes a manifest).

**Scope / requirements.**
- `qe-cli train` runs the real search → ensemble → validation → G1, sealing a vintage.
- Emit rich JSON-line progress: generation, MAP-Elites archive coverage
  (`qe_wfo::regularise::coverage`), CV folds, best-so-far fitness, G1 pass/fail.

**Out of scope.** Distributed/parallel search; UI (QE-261).

**Acceptance criteria.**
- A small-budget training run over the sample store produces a sealed vintage and a progress stream
  covering generations → gate result; deterministic for a fixed seed.

`Spec ref: admin-ui spec §10 (spec 4).`

## QE-261 — Training-monitor UI screen  *(fast-follow)*
`Phase: PreP3` · `Area: frontend` · `Depends on: QE-259, QE-260`

**Why.** Visualise a training run (net-new screen — the design has none).

**Scope / requirements.**
- Trigger a training run; live progress (generations, archive-coverage grid, CV folds, best-so-far,
  G1 gate result) via polling; on completion, link to the produced vintage's backtest.

**Out of scope.** Live Trade/Risk surfaces (Phase 3).

**Acceptance criteria.**
- A training run can be triggered and monitored to a G1 verdict from the UI, composed from the
  design system.

`Spec ref: admin-ui spec §10 (spec 4).`

## PreP3 follow-ups (QE-262..QE-266)

> Hardening / completeness items surfaced by the PreP3 code reviews (recorded in
> `docs/mds/reviewed/qe-251.md` … `qe-261.md`). **None blocks PreP3** — it shipped green. Priority tags:
> **P1** = correctness/safety, do before trusting training output for decisions; **P2** = do before wider
> exposure or heavier load; **P3** = opportunistic quality. Same 25x/26x band as PreP3.

## QE-262 — Persist catalogue version + states in the vintage; assert on load  *(P1 — correctness)*
`Phase: PreP3-followup` · `Area: vintage / signal` · `Depends on: QE-260, QE-251` · `Priority: P1`

**Why.** `train` (QE-260) and `backtest` (QE-251) both build decision bars against
`CatalogueConfig::default()` (`{states:5}`) — the only catalogue in the tree today — and `Genome::is_valid`
only **bounds-checks** feature/state indices. It does **not** detect a same-width/same-`num_states` catalogue
**reorder** or a `CATALOGUE_VERSION` bump. `VintageContent` persists neither the catalogue version nor `states`,
so the moment anyone changes `CatalogueConfig`, adds/reorders an indicator, or re-seals an older vintage, a
backtest could **silently score genomes against the wrong schema** and produce wrong numbers with **no error**.
Benign today (single catalogue), a latent trap the moment training is trusted for real decisions.

**Scope / requirements.**
- Add `catalogue_version: u32` (`CATALOGUE_VERSION`) and the catalogue `states` (or a full catalogue fingerprint)
  to `VintageContent`; seal them in `train`.
- On `Vintage::load`/`verify()` (and in the `backtest` feature bridge's `check_schema`), assert the loaded
  vintage's catalogue version/fingerprint matches the runtime `CatalogueConfig` → loud `SchemaMismatch` on drift.
- Soften/replace the now-accurate risk note in `crates/cli/src/jobs/features.rs`.

**Acceptance criteria.**
- A vintage sealed under one catalogue fingerprint and loaded under a different one (version bump or reordered
  catalogue) fails loudly on load/backtest; the matching case still passes; `train`→`backtest` round-trip green.

`Spec ref: reviewed/qe-251.md §follow-up, reviewed/qe-260.md.`

## QE-263 — Run-store startup reconciler for orphaned `running` runs  *(P2)*
`Phase: PreP3-followup` · `Area: backend / orchestration` · `Depends on: QE-255` · `Priority: P2`

**Why.** QE-255's supervisor owns each run in-process; a server restart leaves any in-flight run wedged at
`running`/`queued` in `meta.json` forever (documented limitation). Fine for a single-node dev tool, wrong once
restarts are routine.

**Scope / requirements.**
- On startup, scan `data/runs/*/meta.json`; any non-terminal run whose child is no longer alive (no live
  supervisor, no recent progress) is reconciled to `failed` (or re-queued, if the design prefers) with a clear
  "orphaned by restart" reason. Idempotent; doesn't touch terminal runs or a run a fresh supervisor now owns.

**Acceptance criteria.**
- A `meta.json` left `running` with no live process is transitioned to a terminal state on next startup; a
  genuinely-terminal run is untouched; tested with a fabricated orphaned run dir.

`Spec ref: reviewed/qe-255.md §deferred.`

## QE-264 — Enrich read APIs for the admin UI (vintage symbol roster + run metrics summary)  *(P2 — UX completeness)*
`Phase: PreP3-followup` · `Area: backend / api + frontend` · `Depends on: QE-257, QE-259` · `Priority: P2`

**Why.** Two QE-259 UI deviations stem from thin read APIs: (a) the New-backtest **universe** options are sourced
from `/api/market-data/coverage` because `/api/vintages` exposes **no per-vintage symbol roster**; (b) the
**Backtests list omits a key-metrics column** because `RunMeta` carries no metrics (they live only in
`result.json`). Spec §7.2 lists both.

**Scope / requirements.**
- `/api/vintages`: include each vintage's instrument roster (the symbols its genomes were evolved against) so the
  trigger form offers vintage-scoped universe options; point the UI at it.
- `RunMeta` (or the run index): carry a small metrics summary (e.g. CAGR/Sharpe/max_dd) written on run
  completion, so the list can show a metrics column without fetching every `result.json` (lazy-fetch acceptable
  as an alternative). Wire the QE-259 list column.

**Acceptance criteria.**
- The New-backtest universe options come from the selected vintage's roster; the Backtests list shows key
  metrics for completed runs; both session-gated; frontend + server tests.

`Spec ref: reviewed/qe-259.md §deferred (1,2); admin-ui spec §7.2.`

## QE-265 — Auth hardening: OIDC `nonce` + local JWKS/RS256 verification  *(P2 — security defense-in-depth)*
`Phase: PreP3-followup` · `Area: backend / auth` · `Depends on: QE-256` · `Priority: P2`

**Why.** QE-256 is sound for a trusted single-tenant allowlist, but two hardening gaps were flagged: no OIDC
`nonce` binding (replay defense-in-depth), and ID-token **signature verification is delegated to Google's
`tokeninfo`** endpoint (behind `http`) rather than local JWKS/RS256 — chosen only to avoid `ring`
(license)/`rsa` (RUSTSEC-2023-0071). Do before any wider/less-trusted exposure.

**Scope / requirements.**
- Add a `nonce` to the auth-code request bound to the browser (like the `state` cookie) and verify it in the ID
  token on callback.
- Replace the `tokeninfo` delegation with **local JWKS fetch + RS256 verification** using a license-/advisory-clean
  crate (re-evaluate the ecosystem; keep `cargo deny` green), removing the extra Google network round-trip.

**Acceptance criteria.**
- A replayed/absent-nonce token is rejected; ID tokens are verified locally against Google's JWKS (mocked in
  tests); `cargo deny` stays green; the existing 401/403/200 auth contract is unchanged.

`Spec ref: reviewed/qe-256.md §follow-ups (1,2).`

## QE-266 — qe-server non-blocking I/O + run-supervision robustness nits  *(P3 — quality/scale)*
`Phase: PreP3-followup` · `Area: backend / server` · `Depends on: QE-255, QE-257` · `Priority: P3`

**Why.** Small quality items harmless at admin scale, worth closing before heavier load: the run store /
read handlers do **blocking `std::fs`** on the async runtime (and reopen `stdout.log` per line); a job that emits
`done`+exit 0 but **writes no `result.json`** is classified `succeeded` (then 409s on `/result`).

**Scope / requirements.**
- Move the run-store / read-handler file I/O to `tokio::fs` or `spawn_blocking`; stop reopening `stdout.log` per
  progress line.
- Classify a `done`-without-`result.json` job as `failed` with a clear reason (remove the QE-255 `// TODO`).
- (Opportunistic, only if trivial) de-`O(n²)` the QE-260 `elite_pool` dedup; mark QE-251 `costs.slippage_model`
  as nominal in-contract if not already clear.

**Acceptance criteria.**
- No blocking `std::fs` on the request path (spot-checked); a `done`-without-result job ⇒ `failed`; green gate + no regressions.

`Spec ref: reviewed/qe-255.md §nits, reviewed/qe-260.md §nits.`

# Phase 3 — Live, attribution & ops   *(gated by G2; live capital gated by G3)*

## QE-301 — Strategy Allocation Journal (best-effort, 3-day retry)
`Phase: P3` · `Area: ⑨ Cold path` · `Depends on: QE-218`

**Why.** Carries per-revision allocation/contribution snapshots forward for attribution.
*(Spec design retained as-is; see the divergence note at the top — durability concern recorded,
not actioned.)*

**Scope / requirements.**
- Startup handshake runs in parallel with bootstrap; on handshake failure, ask the operator to
  confirm before the Hedger proceeds.
- Every planner revision is queued for append on a best-effort, fire-and-forget basis; append
  never gates the gRPC dispatch (QE-218); failed appends retry in the background, time-bounded
  to 3 days, then dropped.

**Out of scope.** Reconciliation (QE-302).

**Acceptance criteria.**
- Append never blocks dispatch; handshake failure gates only on operator confirmation; appends
  older than 3 days are dropped as specified.

`Spec ref: Runtime — Strategy Allocation Journal; "best-effort append… 3-day… retry".`

## QE-302 — Reconciliation (journal × venue trade history)
`Phase: P3` · `Area: ⑨ Cold path` · `Depends on: QE-301`

**Why.** Reconstructs per-strategy realized PnL, fees, and funding for arbitrary backward
windows, surviving restarts and gaps in the live observability stream.

**Scope / requirements.**
- Periodically join the journal against venue REST trade history (`/userTrades`, `/income`,
  similar) — a different surface than the order endpoint; out-of-band of the critical path.

**Out of scope.** Real-time divergence alarm (QE-221).

**Acceptance criteria.**
- Per-strategy realized PnL/fees/funding reconstruct for an arbitrary backward window.

`Spec ref: Runtime — Reconciliation joins journal × venue REST trade history.`

## QE-303 — Attribution outputs
`Phase: P3` · `Area: ⑨ Cold path` · `Depends on: QE-302`

**Why.** The durable counterpart to the live cockpit view: PnL decomposition and
modeled-vs-realised audits across arbitrary windows.

**Scope / requirements.**
- Produce PnL decomposition + modeled-vs-realised outputs surviving restarts/reorgs.

**Out of scope.** Cockpit display (QE-304).

**Acceptance criteria.**
- Attribution outputs reconcile to venue totals within tolerance over a test window.

`Spec ref: ⑨ attrib_out "PnL decomposition · Modeled-vs-realised".`

## QE-304 — Cockpit (observability + authenticated manual controls)
`Phase: P3` · `Area: ⑦ Observability` · `Depends on: QE-214, QE-212`

**Why.** The operator surface: per-strategy state/position, breaker status, position + equity
+ health, and manual controls — off the steady-state critical path.

**Scope / requirements.**
- Display per-strategy state/position, circuit-breaker status, position/equity/system health
  (incl. clock-skew from QE-008).
- Manual controls (pause hedging, override target) that are **authenticated and immutably
  logged** (operator accountability).

**Out of scope.** The kill-switch (QE-216) — distinct, out-of-band.

**Acceptance criteria.**
- Manual overrides are authenticated and recorded immutably with operator id + reason.

`Spec ref: ⑦ cockpit; reviewer: operator accountability.`

## QE-305 — Monitoring / alerting SLAs + on-call
`Phase: P3` · `Area: ops` · `Depends on: QE-304` · **Required before G3**

**Why.** *(Reviewer-added.)* Defines what is alerted, to whom, within what latency, and which
conditions auto-halt vs page a human.

**Scope / requirements.**
- Alerting contract: conditions, severities, latency SLAs, auto-halt vs page routing,
  acknowledgement.

**Out of scope.** Runbook content (QE-306).

**Acceptance criteria.**
- Each defined condition routes to the correct action within its SLA in a drill.

`Spec ref: reviewer: monitoring/alerting SLAs.`

## QE-306 — Incident runbooks (pre-go-live)
`Phase: P3` · `Area: ops` · `Depends on: QE-305` · **Required before G3**

**Why.** *(Reviewer-added.)* Runbooks must exist before first live capital — a runbook written
after the incident is a post-mortem.

**Scope / requirements.**
- Runbooks for: venue outage, position desync, runaway position, liquidation approach,
  journal-down, breaker storm, clock-skew halt.

**Out of scope.** Concentration policy (QE-307).

**Acceptance criteria.**
- Each runbook is reviewed and dry-run at least once before G3.

`Spec ref: reviewer: runbooks before go-live.`

## QE-307 — Single-venue concentration cap + outage/withdrawal-halt runbook
`Phase: P3` · `Area: governance` · `Depends on: QE-306` · **Required before G3**

**Why.** *(Reviewer-added.)* Everything (funds, execution, data) sits on Binance — custody,
counterparty, jurisdictional, and SPOF risk must be explicitly owned and capped.

**Scope / requirements.**
- An explicit counterparty exposure cap (max capital ever on-venue); outage/withdrawal-halt
  runbook; acknowledgement of jurisdictional/suitability constraints.

**Out of scope.** Multi-venue support (out of scope for this platform).

**Acceptance criteria.**
- A documented, enforced on-venue capital cap and an outage/withdrawal-halt runbook exist.

`Spec ref: reviewer: single-venue concentration / counterparty risk.`

## QE-308 — GATE G3: Go/No-Go to live capital
`Phase: P3` · `Area: gate / governance` · `Depends on: QE-130, QE-222, QE-305, QE-306, QE-307`
· **Blocks: QE-309**

**Why.** *(Reviewer-added.)* Independent sign-off that the vintage has earned real capital —
not merely that the code compiles.

**Scope / requirements.**
- Independent (non-builder) sign-off against a written checklist requiring: G1 holdout pass,
  G2 shadow pass, a passed **calendar-time paper soak** with pre-registered thresholds
  (live-vs-modeled tracking error, realized drawdown, slippage realism), declared
  max-acceptable-loss (QE-130), frozen vintage hash, alerting SLA (QE-305) and runbooks
  (QE-306) in place, and concentration cap (QE-307).
- **Frozen-vintage rule:** the exact vintage that passed soak is the one promoted — no silent
  re-train between validation and live.

**Out of scope.** The ramp mechanics (QE-309).

**Acceptance criteria.**
- Live capital cannot be enabled without a recorded G3 sign-off referencing each checklist item.

`Spec ref: reviewer: pre-deployment risk sign-off / go-no-go.`

## QE-309 — Staged capital ramp with enforced caps
`Phase: P3` · `Area: governance` · `Depends on: QE-308, QE-215`

**Why.** *(Reviewer-added.)* Capital exposure grows only as evidence accumulates; each stage is
hard-capped with automatic de-escalation.

**Scope / requirements.**
- Staged caps (nominal → small fixed notional → scaled), each gated on realized tracking error
  and max-drawdown staying within a pre-declared envelope; auto-de-escalation on breach.
- Caps enforced in code via the QE-215 pre-trade checks (not advisory).

**Out of scope.** Re-calibration (QE-310).

**Acceptance criteria.**
- The first live ticket runs at the lowest stage; breaching an envelope auto-de-escalates;
  promotion between stages requires the realized envelope to hold.

`Spec ref: reviewer: staged capital ramp.`

## QE-310 — Live-deployment hardening + breaker re-calibration
`Phase: P3` · `Area: ops / risk` · `Depends on: QE-309`

**Why.** Ongoing hardening; breaker thresholds re-calibrated on OOS/stressed distributions as
live data accrues, preserving the firewall (live cannot influence the archive).

**Scope / requirements.**
- Periodic breaker re-calibration on OOS/stressed distributions with explicit safety margin;
  operational hardening from live experience; firewall preserved (no live→archive influence).

**Out of scope.** Strategy re-training (training pipeline, separate vintage).

**Acceptance criteria.**
- Re-calibration uses OOS/stressed data with a margin; no live observation feeds the archive
  (firewall guard QE-132 still green).

`Spec ref: Robustness — calibrated thresholds; firewall "live execution observations cannot influence ensemble construction".`

## QE-311 — Railway deployment & CD  *(deferred)*
`Phase: P3 (deferred)` · `Area: ops / infra` · `Depends on: QE-013, QE-217, QE-305`

**Why.** Productionise onto Railway once the engine runs locally: training as a job, runtime
as always-on services, with persistent volumes and platform secrets. Deferred per decision
(run local for now).

**Scope / requirements.**
- Containerise training (batch/cron job) and the runtime services (Hedge Planner + Edge
  gateway, long-running) from the QE-013 images.
- Provision persistent **volumes** for LMDB stores, vintage artefacts, the ephemeral REST
  cache, and the allocation journal; wire **secrets** (Binance API keys) via Railway.
- **Private networking** for the planner↔gateway gRPC link; CD from CI-green builds (QE-005);
  environment promotion (sim → live) gated by **G3** (QE-308).
- **Document the colocation caveat:** the *live* Edge gateway may require a venue-colocated
  host (not Railway) for low-latency fills — captured as an explicit open decision, not
  silently assumed solved.

**Out of scope.** Selecting the colocated live-execution host (separate decision per the caveat).

**Acceptance criteria.**
- A CI-green commit deploys the runtime services to Railway with volumes + secrets wired.
- The planner↔gateway gRPC link works over Railway private networking in sim mode.
- The colocation caveat is recorded as an open item with its latency rationale.

`Spec ref: Runtime — colocated Edge gateway, gRPC; decision: Railway deferred to P3.`

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
