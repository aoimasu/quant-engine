# Design Note QE-455 — Research flow: steered search + composite train→backtest + real-data ingest + vintage inspection

*Steer the already-deflated in-run search — do not wrap a best-of-N loop around it*

> **Status:** Design proposal (not yet scheduled). Produced by a five-discipline panel (2 senior quant
> researchers, 1 senior software engineer, 1 trading operator, 1 SRE/ops-safety) interviewing the platform
> owner — independent analysis → debate → chair synthesis, 2026-07-18. Related: the
> [GP indicator-evolution design QE-450](./qe-450-gp-indicator-evolution-design.md) (whose deflation backbone
> this reuses wholesale) and its epics [QE-451..454](../backlog.md#gp-indicator-evolution); the
> [Max Dama panel review QE-430..449](../backlog.md#review-r2); the
> [admin-UI PreP3 design](../superpowers/specs/2026-07-02-admin-ui-training-backtest-design.md).

`Area: server / wfo / signal / ingest / frontend (qe-server, qe-wfo, qe-run-protocol, qe-ingest, web)` ·
`Depends on (hard): QE-260, QE-257, QE-259, QE-451, QE-452, QE-419, QE-407, QE-253`

---

## 1. Recommendation

**Go — as a UI/controls-and-composition layer over the existing engine, not a new search paradigm.** The
owner's stated goal — *"mutate indicator sets to find the best combined vintage"* — is already the engine's
core loop: `train` runs a leakage-safe MAP-Elites/WFO search over strategy genomes into a quality-diversity
archive (`qe-wfo`, `crates/wfo`), `ensemble` selects+weights a subset of elites via discrete DE into a sealed
**vintage** (`crates/vintage`: `VintageContent.chromosomes` + `weights`), and `evolve` (QE-451..454) invents
new indicator formulas under a rigorous deflation backbone. What is *missing* is not more search — it is the
**controls to steer and inspect** that search, the **composition** to run train→backtest as one supervised
lifecycle, a way to **judge "better" on data the search never saw**, and **real market data** to run it on.

> **Headline decision (owner-locked, unanimous):** **Idea #3 is STEER, not LOOP.** We add UI/server controls
> to steer & inspect the existing in-run search (bigger budget, which catalogue/evolved indicators are in
> play, longer/more windows) — all **inside the existing deflation basis**. An **outer genetic loop across
> runs** ("best-of-N vintages") is **explicitly rejected**: it manufactures a fresh, uncounted multiple-testing
> surface on top of the one QE-430..454 spent twenty tickets making honest, and re-selects on the same holdout
> until it leaks. Every feature in this note is constrained by one rule: **no steering knob, composite flow, or
> leaderboard may loosen the G1 gate or escape the deflation/seal path.**

The single load-bearing guardrail, unanimous across the panel: **the set of steerable knobs is a compiled
whitelist proven to be gate-monotone** — no whitelisted knob can move a candidate from *rejected* to *sealed*
under the G1 criteria. Steering changes *what the search explores and how hard*, never *what passes*.

---

## 2. Motivation & the reframe

The platform reached G1 (`crates/gate::evaluate_g1`) and shipped an admin UI (PreP3, QE-251..261) that can
launch a `train` run, watch MAP-Elites progress, and backtest a sealed vintage. Two design panels then added
`evolve` (QE-450) with a full deflation backbone: GP-aware trial basis (QE-439), uncensored PBO (QE-414),
IC+FDR (QE-434), MDL parsimony (QE-436), cost/turnover/capacity gates, and a fail-closed governed seal
(QE-452/454). The engine is now *methodologically* strong. The gaps are **operational**:

1. **You cannot steer the search from the outside.** `TrainParams` (`crates/run-protocol/src/lib.rs:358`)
   already exposes `generations`, `population`, `holdout`, `embargo`, `seed` — but there is **no control over
   which indicators are in play** (catalogue subset, evolved-pool inclusion) and **no window/fold controls**,
   and the SPA New-training form surfaces only a subset. The owner wants to say "search harder, over these
   indicators, over more windows" — and today must edit a config file.

2. **You cannot judge "better" on protected data through the UI.** `train` already carves a G1 holdout
   (`TrainParams.holdout`/`embargo`, consumed by `evaluate_g1`), but the concept is invisible above the CLI and
   there is no composite that *carves it once and protects it across a train→backtest pair.*

3. **train and backtest are two disconnected runs.** `RunSpec` (`crates/server/src/runs/model.rs:50`) is
   `Backtest | Train | Evolve`; there is no composite. An operator trains, waits, copies the sealed vintage id
   into a *second* New-backtest form, and runs it — with no atomicity, no shared seed/holdout, no single
   lifecycle.

4. **Data is synthetic-only.** `qe ingest --synthetic` (`crates/cli/src/jobs/ingest.rs:107`) was just merged;
   the real `http` decoders are a documented future-work seam (`#[cfg(feature = "http")]` in
   `crates/ingest/src/{fetcher,rest}.rs`, off by default). Nobody can train on real exchange data yet, and
   there is no provenance marker to stop someone training on synthetic bars *thinking* they are real.

5. **The vintage is a black box in the UI.** `evolve` got a rich `PoolReview` inspection screen
   (`web/src/app/evolve/PoolReview.tsx`) over its formula pool. The `train` path never got the equivalent: the
   ensemble composition/weights **and** the G1 gate evidence *are* in the sealed vintage
   (`VintageContent.chromosomes`/`weights`/`lineage`) and the run's gate snapshot, but `GET /api/vintages`
   (`crates/server/src/read.rs:68`) returns only a thin list (`{id, label, summary}`), there is **no**
   `GET /api/vintages/{id}` detail endpoint, and the SPA renders a placeholder ("*The strategies browser is on
   the way*", `web/src/app/App.tsx:121`).

**The reframe.** All five gaps are additive UI/server/composition work over an engine that already does the
hard part. The prize is a research operator who can, in one supervised flow, steer the honest search over a
chosen indicator set on real data, watch it, and inspect exactly what sealed and why — **without ever widening
the mouth of the deflation funnel.** The danger, and the reason this note exists, is that every one of these
conveniences is one lazy decision away from re-introducing the overfitting the backbone was built to kill. The
design is therefore organised around **boundaries that are structural, not advisory.**

---

## 3. Scope

### In scope

- **Steerable search parameters** on `RunSpec::Train` + `validate_train`: search budget (generations /
  population), an **indicator subset** (which catalogue indicators and which evolved-pool formulas are in
  play), and window/fold configuration — all behind a **compiled gate-monotone whitelist** (§6).
- **A frozen out-of-sample holdout contract** (§4): the composite carves & protects a holdout the search never
  sees, and records the split in `Lineage`/seal evidence.
- **A new server-owned composite run-kind `RunSpec::Flow`** (§5): configure once → `train`→`backtest` in one
  supervised, atomic, resumable lifecycle with its own concurrency lane and a deterministic seed + vintage
  handoff.
- **A `GET /api/vintages/{id}` detail read endpoint** and a **Vintage Inspector** SPA screen (§7) mirroring the
  `evolve` PoolReview: ensemble composition (chromosomes→indicators), per-chromosome weights, and the G1
  gate/deflation snapshot.
- **A thin real-`http` ingest slice** (§8): one exchange (Binance USDT-M), a few instruments,
  incremental/resumable/idempotent historical download behind the default-off `http` feature, plus an
  **`ingest` run-kind + `POST /api/ingest` trigger**, alongside the existing `--synthetic` path — with
  **real-vs-synthetic provenance** visible on the store/coverage and the SPA MarketData view.
- **A vintage leaderboard/comparison surface** (§9): rank sealed vintages on **net-of-cost** performance,
  **capacity-at-size**, and **cross-vintage correlation** (never gross Sharpe), plus steer/param diffs —
  strictly **informational**.

### Out of scope (explicitly rejected or deferred)

- **An outer best-of-N genetic loop across runs / an auto-selector over the leaderboard.** *Unanimously
  rejected.* Re-selecting the "best" vintage across many runs on the same holdout is an uncounted
  multiple-testing device that escapes the deflation basis. Promotion stays through the existing per-run
  G1 gate and seal; the leaderboard is inspection, not selection (§9).
- **Any steering knob that can relax a G1 threshold** — cost-stress multiplier, turnover cap, DSR/PBO cutoff,
  capacity floor, embargo/purge sizing (§6). These are **not** on the whitelist and `validate_train` rejects a
  request that tries to set them below their compiled floors.
- **Loosening the holdout** — shrinking it below a floor, un-embargoing it, or letting the search read it (§4).
- **A general multi-exchange / multi-asset-class ingest framework.** The real-ingest slice is deliberately
  *one* exchange, *few* instruments, USDT-M perps only — the long pole, scoped down (§8, §11).
- **Changing the engine's search or gate semantics.** `evaluate_g1`, the MAP-Elites archive, the ensemble DE,
  and the seal are untouched — this note adds controls and composition *around* them, not inside.
- **Co-evolution / evolved-pool authoring in the flow.** `evolve` remains its own campaign+seal lifecycle;
  the flow only *consumes* an already-sealed evolved pool as an indicator source (§6).

---

## 4. The frozen out-of-sample holdout contract

**"Better" is judged on data the search never sees.** This is the anchor that makes steering safe: you can
turn every steer knob to the maximum and the verdict still rests on an untouched holdout.

The engine already has the mechanism — `TrainParams.holdout` (final N bars reserved) + `TrainParams.embargo`
(bars purged between the train window and the holdout), consumed by `crates/gate::evaluate_g1` on the holdout
slice. QE-455 makes it a **first-class, protected, recorded contract**:

- **Carved once, by the server, for the whole flow.** `RunSpec::Flow` computes the holdout split from the flow
  spec *before* the train sub-run starts, and hands the *same* split to the backtest sub-run. Neither the
  operator's steer knobs nor a mid-flow resume can move it.
- **The search cannot read it.** The holdout bars are excluded from every fold the MAP-Elites/DE search scores
  on (already true for `train`); the composite additionally asserts the backtest sub-run's evaluation window
  and the training folds are **disjoint** from the holdout, with the embargo enforced on both edges — reusing
  the purge/embargo discipline of QE-113/117.
- **Frozen against steering.** The holdout size/embargo have **compiled floors**; `validate_train` /
  `validate_flow` reject a request that shrinks the holdout below the floor or zeroes the embargo. The holdout
  is **not** a steerable knob (§6).
- **Recorded in lineage + seal evidence.** The `{holdout_bars, embargo_bars, holdout_range, train_range}` split
  is written into `VintageContent.lineage` (`crates/vintage/src/lib.rs`) so the split is reproducible and
  auditable from the sealed artefact — anyone can recompute which bars the verdict rode on. The Vintage
  Inspector (§7) surfaces it.
- **Consultation budgeting (dissent-flagged, §11).** A steered campaign can re-consult the *same* holdout many
  times; that is silent multiple-testing at the campaign level. v1 **records** the holdout range + a
  per-holdout consultation count in lineage so the exposure is visible; escalating the DSR threshold with
  cumulative consultations (as QE-450 §5 "holdout-consultation budgeting" proposes) is a follow-up, not a v1
  blocker — but the leaderboard must **not** be used to shop the holdout (§9).

---

## 5. The composite `RunSpec::Flow` lifecycle

**A new server-owned composite run-kind.** Configure train + backtest + steer **once** → the server sequences
`train`→`backtest` in one supervised lifecycle. This is genuinely new: `RunSpec` today
(`crates/server/src/runs/model.rs:50`) is a flat `Backtest | Train | Evolve` and each run is terminal.

### 5.1 Shape

- **`RunSpec::Flow(FlowParams)`** added to the enum, with a `FlowParams` DTO in the dependency-free
  `qe-run-protocol` leaf crate (`PROTOCOL_VERSION 2→3`; every field `#[serde(default)]` **except** the
  required window + a **required `seed`** — a flow verdict must stay byte-reproducible, mirroring
  `EvolveParams.seed`, `crates/run-protocol/src/lib.rs:446`). `FlowParams` embeds a steer-whitelisted
  `TrainParams`-shaped block (§6) and the backtest window; the instrument universe is config-derived like
  `train`.
- **`run_type() == "flow"`**, **`writes_vintage() == true`** (the train phase seals the vintage;
  `crates/server/src/runs/model.rs:96` gains the `Flow` arm), `label()` = the flow window.
- **`validate_flow`** in `crates/server/src/runs/manager.rs` (alongside `validate_train`/`validate_evolve`,
  `manager.rs:415/428`): required window + seed, steer-whitelist enforcement (§6), holdout/embargo floors (§4),
  uniform `400` on any violation.

### 5.2 Supervision & sequencing

- **Server-owned sequencing.** The flow supervisor runs the `train` sub-job to a sealed vintage, then spawns
  the `backtest` sub-job **over the frozen holdout window** with the just-sealed vintage id — a **deterministic
  vintage handoff** (the vintage id is the content hash, so the handoff is exact and reproducible). No operator
  copy-paste, no second form.
- **Atomic verdict.** The flow succeeds only if *both* sub-runs succeed and the vintage sealed; a train that
  fails G1 fails the flow (no vintage → no backtest). The composite is one row in the run store with one
  status, its sub-run ids recorded in `meta.json`.
- **Its own concurrency lane.** A dedicated **`QE_SERVER_MAX_FLOW_CONCURRENCY` semaphore (default 1)**, mirroring
  the `evolve` semaphore pattern (QE-454 §13.10) so a multi-hour flow never starves interactive backtests, and
  a per-flow wall-clock deadline reusing the QE-425/QE-407 `abort → kill_on_drop → terminally-mark` pattern.
- **Determinism.** The single flow `seed` derives both the train search seed and the (deterministic) backtest;
  re-running a flow from its recorded `seed` + pinned `input_snapshot_id` reproduces the vintage **byte-identically**
  (the `VintageContent.content_hash` contract, QE-006/QE-129).

### 5.3 Resume / halt (the long-composite problem)

Today a run is a terminal 4-state `RunStatus` (`queued → running → succeeded | failed`,
`crates/server/src/runs/model.rs:16`) with **no resume** — fine for a minutes-long backtest, wrong for a
multi-hour composite. QE-455 adds, building on QE-407's supervised-task registry / graceful-shutdown work:

- **Checkpoint at the sub-run boundary.** A flow that has sealed its vintage but not finished the backtest can
  **resume from the backtest phase** on server restart, rather than re-running the expensive search — the sealed
  vintage is the natural checkpoint (content-addressed, already durable).
- **Authorised halt.** A `POST /api/runs/{id}/halt` arm for flows (mirroring the `evolve` halt, QE-454 §13.11)
  → SIGTERM → terminal `halted`, with the partially-sealed vintage (if any) retained and auditable.
- **No new non-terminal run status leaks to the vintage/gate path.** Resume/halt are supervision concerns;
  the seal predicate and G1 gate are unchanged and remain the sole authority for what a vintage *is*.

---

## 6. The steer-knob whitelist + the anti-overfitting guardrail

**This is where the feature is won or lost.** Steering must change *what the search explores and how hard*,
never *what passes the gate.* The mechanism is a **compiled whitelist of gate-monotone knobs.**

### 6.1 The whitelist (mutable — cannot relax G1)

| Knob | Surface | Why it is gate-safe |
|---|---|---|
| **Search budget** — `generations`, `population` | `TrainParams` (exists) | More search = more trials. The GP-aware / effective-trials deflation basis (QE-439) **counts every trial**, so a bigger budget *raises* the deflation bar (`E[maxSharpe]` grows with N) — it can never lower it. Monotone in the safe direction. |
| **Indicator subset** — catalogue-indicator inclusion + evolved-pool-formula inclusion | new `TrainParams` field | Restricts/expands the feature set the search may reference. Fewer indicators = a *smaller* hypothesis space (strictly safer); more = counted in N. The evolved pool is consumed **only if already sealed** (QE-451/452); inclusion cannot un-seal or re-deflate it. |
| **Windows / folds** — number & length of WFO windows/folds | new `TrainParams` field | More/longer windows raise `T_eff` and make CV *harder to pass*, not easier. Purge/embargo sizing is derived from indicator lookback (`cv.rs`), **not** operator-set (§4), so a window knob cannot shrink the embargo. |
| **Seed** | `TrainParams.seed` (exists) | Reproducibility only; changes the RNG stream, not any threshold. |

### 6.2 The blocklist (NOT steerable — compiled floors, `validate_*` rejects)

Everything the G1 gate's own decision rides on is **off the whitelist** and carries a compiled floor that
`validate_train`/`validate_flow` enforce as a `400`:

- **Cost-stress friction multiplier** (`min{1×,2×}` re-cost, QE-450 §4.6) — fixed; cannot be lowered.
- **Max-turnover cap** / capacity floor (`CAPACITY_FLOOR`) — fixed.
- **DSR / uncensored-PBO cutoffs, SPA p, IC/FDR threshold** — fixed in `G1Criteria` (`crates/gate`).
- **Holdout size / embargo / purge** — floored (§4); the holdout is frozen, not tuned.
- **The deflation basis itself** — the `DEFLATION_BASIS_VERSION` compiled const (QE-454) is server-side and
  non-editable; no request field flips it.

### 6.3 The proof obligation

The guardrail is only real if it is **tested**: a whitelisted steer knob, set to any value in its allowed
range, **cannot** move a candidate from *G1-reject* to *G1-seal*. The acceptance test (QE-458) runs a fixed
seed/dataset where the un-steered gate **rejects**, then sweeps every whitelisted knob across its range and
asserts **no steered configuration seals a vintage the un-steered gate would reject** — i.e. steering is
gate-monotone. A knob that fails this test is not admitted to the whitelist. Symmetric to the QE-450 §13.6
"the safe path is the only path" doctrine: **client-side controls are ergonomics; the server-side whitelist +
compiled floors are the control.**

---

## 7. Vintage inspector + `/api/vintages/{id}` surface

**Close the coverage gap: give `train` the inspection `evolve` already has.** The composition and gate evidence
already exist in the sealed artefact — they are simply not exposed.

### 7.1 `GET /api/vintages/{id}` (QE-456 — additive, no engine change)

A new detail route beside the existing list (`crates/server/src/read.rs`; the list at `read.rs:68` returns only
`VintageListItem{id,label,summary}`). It loads the vintage via `VintageRepository::load` (hash-verified,
`crates/vintage/src/lib.rs:256`) off the async worker (`spawn_blocking`, as the list already does) and returns:

- **Ensemble composition** — each `VintageContent.chromosomes[i]` (a `Genome`), decoded to the indicators it
  references (`Genome::referenced_features` → catalogue/evolved indicator ids via `CatalogueIdentity`,
  `crates/vintage/src/lib.rs:82`), with its **per-chromosome weight** `VintageContent.weights[i]`.
- **The G1 gate / deflation snapshot** — the run's gate evidence (DSR, uncensored PBO + population size,
  SPA p, IC/FDR, distinct-trial basis, cost-stress/turnover/capacity results) as recorded by `evaluate_g1` /
  the train run's gate output, plus the **frozen-holdout split** from `VintageContent.lineage` (§4).
- Provenance sidecars already in the content: `slippage` (QE-431), `sizer` (QE-433), `worst_case_loss`
  (QE-130), `calibration` (QE-116), `catalogue` identity (QE-402).

Read-only, session-authed (like `GET /api/vintages`). No `qe-wfo`/`qe-ensemble` code edge is added — the server
already depends on `qe-vintage`; the endpoint only *reads* the sealed artefact.

### 7.2 The Vintage Inspector screen (QE-457 — mirrors PoolReview)

A new SPA screen replacing the `App.tsx:121` placeholder, structured like `evolve/PoolReview.tsx`: a
composition table (chromosome → referenced indicators → weight), a selected-indicators panel, and a
**gate-evidence card** that leads with the *net-of-cost / tradability* numbers and the honest deflation basis
(distinct-trial N vs the `E[maxSharpe]` bar, uncensored PBO with its population size, DSR labelled
"necessary — not sufficient") — the same hierarchy QE-450 §13.5 mandates for PoolReview, so the operator reads
the vintage the same way they read a formula pool. **Inspection only**: no seal/promote affordance (a vintage
is already sealed by the train gate).

---

## 8. Real-`http` ingest thin slice + provenance

**The long pole.** Everything above is additive over an engine that works; real ingest is genuinely new
network + provenance work. It is deliberately scoped to the **minimum that lets a flow train on real data**.

### 8.1 The decoder (QE-463 — one exchange, behind `http`)

The seam already exists: `qe-ingest` has a `Fetcher` transport port with a real `HttpFetcher` over `ureq`
compiled only under the `http` feature (`crates/ingest/src/fetcher.rs:35`, `Cargo.toml` `http = ["dep:ureq"]`),
and the CLI `run_ingest` is written against the injectable `HistoricalSource` seam
(`crates/cli/src/jobs/ingest.rs:50`). QE-463 fills in **one** exchange decoder (Binance USDT-M klines +
funding), reusing the existing planner/reconciliation (QE-101..103):

- **Incremental / resumable** — fetch only the missing periods vs what the store already covers
  (`coverage_bounds`, `crates/storage/src/coverage.rs`); a re-run after interruption continues, not restarts.
- **Idempotent** — re-fetching a covered period is a no-op (same bars, byte-identical), so retries are safe.
- **Default-off** — all real-network code stays behind `#[cfg(feature = "http")]`; the default build and CI
  remain offline (the `--synthetic` path and in-memory test sources are untouched).

### 8.2 The trigger + provenance (QE-464 — run-kind + endpoint)

- **An `ingest` run-kind** (`RunSpec::Ingest(IngestParams)`) + **`POST /api/ingest`** trigger: instruments,
  date range, resolution, and a **"fetch all available instruments"** option resolved from the exchange's
  instrument-list endpoint. Supervised like other runs (run store, subprocess, terminal `done` line via
  `qe-run-protocol` — there is already an `ingest` `done`-line writer, `crates/run-protocol/src/lib.rs:212`).
- **Real-vs-synthetic provenance is the headline requirement.** Today `CoverageRow`
  (`crates/storage/src/coverage.rs:22`) carries `{symbol, resolution, from, to, bars}` with **no source
  marker**, and `--synthetic` only warns at the CLI. QE-464 records a **provenance tag (`real` | `synthetic`)
  on the stored data + coverage** so the store itself knows the origin of every bar — nobody can train on
  synthetic bars believing they are real. Mixed-provenance coverage is surfaced explicitly (interleaved real +
  synthetic is allowed by design, but always *labelled*).

### 8.3 The SPA (QE-465)

An ingest-trigger screen (instruments / date range / fetch-all) and a **provenance column** in the existing
MarketData view (`web/src/app/MarketData.tsx`) so coverage rows show `real` vs `synthetic` at a glance.

### 8.4 Honesty about scope (§11)

Real ingest is the ticket most likely to slip: rate limits, pagination, funding-vs-kline cadence alignment,
exchange API drift, and **data-licence reality** (redistribution terms differ from the public-dumps path
QE-101 already uses) are all real. v1 is **one exchange, few instruments, USDT-M perps, historical only** — no
live streaming (that is the runtime side, QE-202..205), no multi-venue. The provenance marker is the
non-negotiable part; the breadth is explicitly minimal.

---

## 9. The leaderboard / comparison surface

**Informational, not a selection device.** A vintage leaderboard (QE-466) ranks sealed vintages so an operator
can *see* which sealed the best tradable strategy and how steer/params differed — but it must **not** become
the outer best-of-N selector §3 rejects.

- **Ranks on the tradable, deflation-honest metrics only:** **net-of-cost** performance (never gross Sharpe —
  the QE-450 §13.5 inversion), **capacity-at-size** (`VintageContent` capacity/`sizer`, QE-128/433), and
  **cross-vintage correlation** (are these vintages actually diverse, or the same bet re-drawn?). Gross Sharpe
  and in-sample metrics are demoted or absent.
- **Plus steer/param diffs** — which indicator subset, budget, windows produced each vintage, so the operator
  can *understand* the frontier, not *auto-pick* off it.
- **Structurally not a selector.** The leaderboard is a read-only view over already-sealed vintages; it exposes
  **no "promote"/"select-best" action**. Promotion to a runtime vintage stays through the *existing per-run G1
  gate + seal* — each vintage already passed its own honest gate; ranking them does not confer any additional
  blessing. The AC (QE-466) explicitly asserts the leaderboard adds no endpoint that seals/promotes/selects,
  and does not feed any automated re-run loop.
- **Holdout-shopping guard.** Because ranking many vintages on their holdout verdicts is exactly the
  multiple-testing the §4 consultation budget is about, the leaderboard surfaces each vintage's
  **holdout-consultation exposure** rather than hiding it, and carries a standing caveat that cross-vintage
  ranking is *inspection*, and that acting on it by re-running until the top slot improves is the rejected
  best-of-N pattern.

---

## 10. Invariants preserved

| Invariant | How it is preserved |
|---|---|
| **Deflation basis stays authoritative** | Steer knobs are gate-monotone (§6); the blocklist floors + `DEFLATION_BASIS_VERSION` const are compiled and server-enforced; `evaluate_g1`/`G1Criteria` are untouched. A steered run cannot seal a vintage the un-steered gate rejects (QE-458 AC). |
| **Frozen holdout** | Carved once by the server before search, disjoint + embargoed from every fold, floored against shrinking, recorded in `Lineage`/seal evidence, never a steerable knob (§4). |
| **Leaderboard is inspection, not selection** | Read-only over sealed vintages; ranks on net-of-cost / capacity / correlation; exposes no promote/select action; promotion stays via the existing gate/seal (§9). |
| **Search ⊥ portfolio firewall** (QE-001/132) | No new cross-crate code edge: the inspector *reads* `qe-vintage`; the flow supervisor sequences existing CLI sub-jobs; ingest stays in `qe-ingest`/`qe-cli`. The `firewall`/`dependency_topology` test stays green. |
| **Determinism / reproducibility** (QE-006) | Flow `seed` + pinned `input_snapshot_id` reproduce the vintage byte-identically; the deterministic vintage-id (content hash) handoff is exact; ingest is idempotent. |
| **Net-of-cost truth** (QE-109) | Every ranked/inspected number is net-of-cost; the leaderboard forbids gross Sharpe; the inspector leads with cost-stress/turnover/capacity. |
| **Provenance honesty** | Every stored bar is tagged `real`/`synthetic` on the store + coverage; mixed coverage is labelled, never silently blended (§8). |
| **Run lifecycle** | The 4-state `RunStatus` and the seal predicate are unchanged; flow resume/halt are supervision concerns that never leak a new status into the gate/vintage path (§5.3). |

---

## 11. Risks & dissents

The panel did not agree on everything; dissents are recorded honestly.

| # | Risk / dissent | Severity | Mitigation / resolution |
|---|---|:---:|---|
| 1 | **Steering becomes a covert best-of-N.** An operator runs 50 steered flows and picks the top leaderboard slot — the exact uncounted multiple-testing the backbone kills. | **blocker** | The outer loop / auto-selector is rejected in code (§3, §9): no promote/select action, no automated re-run, holdout-consultation exposure surfaced. The panel's *dominant* concern; the boundary is structural, not a guideline. |
| 2 | **A steer knob quietly relaxes G1.** A future knob (e.g. "let me lower the cost multiplier just for research") is added to the whitelist without the monotonicity proof. | **blocker** | The whitelist is compiled + the gate-monotonicity test (QE-458) is a merge gate; the blocklist floors are enforced in `validate_*`. A knob without a passing monotonicity test is not admitted. |
| 3 | **Holdout consultation budget (unresolved dissent).** QR#1 wanted the DSR threshold to escalate with cumulative holdout consultations *in v1*; SRE/SSE argued recording-and-surfacing is enough for a single-operator research tool and escalation is a follow-up. | major | **Resolution:** v1 **records + surfaces** the consultation count (§4, §9); threshold escalation is a documented follow-up (QE-450 §5 "holdout-consultation budgeting"). Dissent preserved. |
| 4 | **Real ingest is a much bigger lift than the rest** — network, pagination, rate limits, funding/kline cadence, API drift, and **data-licence/redistribution reality** differ from the public-dumps path. | major | Scope to **one exchange, few instruments, USDT-M perps, historical only**, behind default-off `http`; provenance marker non-negotiable, breadth explicitly minimal (§8.4). Honestly the long pole — sequence it last (R3). |
| 5 | **Provenance blind spot** — someone trains on synthetic bars thinking they are real. | major | `real`/`synthetic` tag on store + coverage (QE-464); mixed coverage labelled; SPA provenance column (QE-465). The one part of ingest with no scope-down. |
| 6 | **Flow resume corrupts determinism** — a resumed backtest rides a different seed/holdout than the sealed vintage. | major | Resume only from the *sealed-vintage* checkpoint (content-addressed); the holdout split is carved once and recorded in lineage; re-run from seed + snapshot is byte-identical (§5.2/5.3). |
| 7 | **Inspector leaks a promote path** — a "looks good, promote it" button re-introduces selection outside the gate. | minor | Inspector is read-only (§7.2); the leaderboard exposes no select/promote (§9); promotion stays via the existing seal. |
| 8 | **Composite starves interactive work** — a multi-hour flow blocks backtests. | minor | Dedicated flow concurrency lane (semaphore, default 1) + per-flow deadline, reusing the `evolve` supervision pattern (§5.2). |

---

## 12. Phased rollout (mapping to tickets)

Additive-first; the long pole (real ingest) last. Each phase is independently shippable and leaves the engine's
deflation discipline intact.

| Phase | Goal | Tickets | Nature |
|---|---|---|---|
| **R1 — Inspect & steer (no engine change)** | Give `train` the inspection `evolve` has; expose the gate-monotone steer knobs; add the informational leaderboard. | **QE-456** (`/api/vintages/{id}`), **QE-457** (Vintage Inspector), **QE-458** (steer params + whitelist guardrail + monotonicity test), **QE-459** (SPA steering controls), **QE-466** (leaderboard) | Additive. Read endpoints, a validated param block, SPA screens. No engine/gate change. |
| **R2 — Composite flow** | Train→backtest as one supervised, atomic, resumable lifecycle with the frozen holdout carved once. | **QE-460** (`RunSpec::Flow` + holdout carve/record), **QE-461** (flow supervision: lane + resume/halt), **QE-462** (SPA stepped Flow page) | New composite run-kind + supervision; reuses the existing train/backtest sub-jobs and the QE-407 registry. |
| **R3 — Real-data ingest (the long pole)** | Train on real exchange data with visible provenance, alongside synthetic. | **QE-463** (`http` Binance decoder), **QE-464** (`ingest` run-kind + `POST /api/ingest` + provenance), **QE-465** (SPA ingest trigger + provenance column) | The big lift: real network + provenance. Behind default-off `http`; scope-minimal (§8.4). |

Dependency spine (all cited ids verified present in the repo): the inspector rides QE-257/259/260; the steer
work rides QE-260/437/451; the flow rides QE-452/419/407; ingest rides QE-253; every ticket that touches the
search/flow/leaderboard carries the §6/§4/§9 guardrails in its AC.

---

## 13. Acceptance criteria (design-level)

1. **No steered run can seal a vintage the un-steered gate rejects** — the gate-monotonicity sweep (QE-458)
   passes for every whitelisted knob across its full range on a fixed reject-seed dataset.
2. **The blocklist is enforced server-side** — `validate_train`/`validate_flow` reject (`400`) any request that
   sets a cost-stress / turnover / capacity / DSR-PBO / holdout-embargo value below its compiled floor; a
   crafted `POST` cannot relax a gate threshold.
3. **The holdout is frozen and recorded** — the flow carves the split once, asserts train-folds/backtest-window
   disjoint + embargoed from the holdout, floors its size, and writes `{holdout_range, embargo, train_range}`
   into `VintageContent.lineage`; the Inspector renders it.
4. **`GET /api/vintages/{id}`** returns composition (chromosomes→indicators + weights) + the G1 gate/deflation
   snapshot + the holdout split, hash-verified on load, with **no** new `qe-wfo`/`qe-ensemble` code edge; the
   firewall test stays green.
5. **`RunSpec::Flow`** sequences train→backtest atomically with a deterministic (content-hash) vintage handoff;
   a flow with a failed G1 train seals nothing and runs no backtest; re-running from `seed` + pinned snapshot
   reproduces the vintage byte-identically.
6. **Flow supervision** runs in a dedicated concurrency lane and can resume from the sealed-vintage checkpoint
   and be halted (→ terminal `halted`) without leaking a new status into the gate/vintage path.
7. **Provenance is visible** — every stored bar carries a `real`/`synthetic` tag on the store + coverage;
   mixed coverage is labelled; the SPA MarketData view shows the provenance column; real ingest is
   incremental / resumable / idempotent behind the default-off `http` feature, default build stays offline.
8. **The leaderboard adds no selection** — it ranks sealed vintages on net-of-cost / capacity-at-size /
   cross-vintage correlation only, exposes no promote/select/auto-run action, and surfaces each vintage's
   holdout-consultation exposure; promotion still flows through the existing gate/seal.

---

## 14. Open questions

1. **Holdout-consultation escalation** — record-and-surface (v1) vs threshold escalation with cumulative
   consultations (follow-up). *Lean:* record in v1; escalate once campaign-level exposure is observed (dissent §11.3).
2. **Evolved-pool inclusion granularity** — whole sealed pool vs per-formula inclusion in the steer subset.
   *Lean:* whole-pool for R1 (a pool is already a governed unit); per-formula only if the archive shows a need.
3. **Flow resume granularity** — checkpoint only at the sub-run (train/backtest) boundary vs finer intra-search
   checkpoints. *Lean:* sub-run boundary (the sealed vintage is the natural, content-addressed checkpoint);
   intra-search resume is a much larger, engine-touching change, out of scope.
4. **Which exchange first for real ingest** — Binance USDT-M (matches the existing public-dumps path QE-101 and
   the friction defaults) vs another. *Lean:* Binance, for continuity; the `Fetcher`/`HistoricalSource` seams
   keep it swappable.
5. **Leaderboard correlation metric** — realised net-of-cost return correlation vs signal/position correlation
   across vintages. *Lean:* realised net-of-cost return correlation (the tradable diversity question).
6. **Does the composite flow need its own SPA area** (`web/src/app/flow/`) or a stepped page inside Training?
   *Lean:* a stepped Flow page reusing the training/backtest form components (QE-462); a full area only if the
   flow list grows its own lifecycle.

---

*Generated by a five-discipline design panel interviewing the platform owner (2026-07-18). Every claim is
grounded in specific repo files/crates; the dominant concern — the overfitting boundary between "steer the
honest search" and "loop a best-of-N around it" — is preserved as a structural boundary, not a guideline. This
is a design proposal, not a code change.*
