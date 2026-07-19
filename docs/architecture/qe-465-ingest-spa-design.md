# QE-465 — SPA ingest-trigger screen + provenance column (evidence note)

`Phase: Research-flow (R3)` · `Area: frontend` · `Depends on: QE-464`

Design refs: [QE-455 research-flow design](./qe-455-research-flow-design.md) §8.3 (the SPA), §8.2
(provenance visible so nobody trains on synthetic as real). Ticket: `docs/mds/tickets/QE-465.md`.

## 1. Current state (real paths + contracts read)

### The MarketData view + coverage fetch
- `web/src/app/MarketData.tsx` — the read-only coverage table (nav `data`). Fetches via
  `getCoverage()` → `GET /api/market-data/coverage`, renders a `DataTable<CoverageRow>` with
  columns `symbol | resolution | from | to | bars`. **No provenance column today.**
- `web/src/app/MarketData.test.tsx` — component test stubbing `fetch` for the coverage endpoint.
- Wired into the shell at `web/src/app/App.tsx` (`active === 'data' ? <MarketData /> : …`).

### The coverage server contract (QE-464 — the provenance the column renders)
- Route: `crates/server/src/read.rs::market_data_coverage` → `qe_storage::coverage_all(&store)`.
- Row struct: `crates/storage/src/coverage.rs::CoverageRow` — fields
  `symbol, resolution, from (i64 ms), to (i64 ms), bars (usize)` **plus QE-464**
  `provenance: String` (`"real" | "synthetic" | "unknown"`, serde default `"unknown"`) and
  `calibrated: bool` (serde default `false`).
- **Mixed provenance is already split server-side.** `coverage()` calls
  `store.provenance_segments(instrument, resolution)` and emits **one contiguous `CoverageRow` per
  provenance run** — a real+synthetic mix is *never* blended into one row (coverage.rs lines 73–102).
  Zero segments ⇒ a single `unknown` row (legacy untagged bars).
- The SPA mirror `web/src/api/runs.ts::CoverageRow` is **stale** — it lacks `provenance`/`calibrated`.
  Must be extended in lockstep.

### How other run-kinds submit a create + show the run monitor
- Form→monitor view-state machine: `web/src/app/evolve/EvolveArea.tsx` (list | new | monitor) and
  `web/src/app/training/*` are the templates. The closest form is
  `web/src/app/training/NewFlow.tsx` (window `start`/`end` + `resolution` `Select` from `RESOLUTIONS`,
  a review step showing the exact request body, then a single POST).
- Submit choke point: `web/src/api/runs.ts::postRun(type, params)` → `POST /api/runs`
  `{type, params}`; typed wrappers `createTrainRun`/`createFlowRun`/`createEvolveRun`. Error handling
  funnels through `throwForResponse` (401 → `UnauthorizedError` + `emitUnauthorized`).
- Monitor: `web/src/app/evolve/CampaignMonitor.tsx` — uses the shared `usePollingRun(runId)`
  (`web/src/api/usePollingRun.ts`, bounded-retry, terminal-stop) + `<RunProgress status progress />`
  (the coarse standard bar) + `<StatusBadge>`, and a **Halt** button calling `haltRun(runId)`.

### The ingest endpoint (QE-464) — the EXACT accepted shape
- `crates/run-protocol/src/lib.rs::IngestParams` (the wire source of truth):
  `instruments: Vec<String>`, `fetch_all: bool`, `start: String`, `end: String`,
  `resolution: String`, `synthetic: bool` — **every field `#[serde(default)]`** (lenient parse).
- Enforcement (`crates/server/src/runs/manager.rs::validate_ingest`): `start`/`end`/`resolution`
  required (non-empty), and **either** a non-empty `instruments` list **or** `fetch_all: true` —
  never neither. Uniform `400`.
- **Endpoint: `POST /api/ingest`** (`crates/server/src/runs/api.rs::create_ingest`). Its body is the
  **ingest params object DIRECTLY** (`{instruments,fetch_all,start,end,resolution,synthetic}`), NOT
  the `{type, params}` wrapper that `POST /api/runs` takes. The handler wraps it as a
  `type:"ingest"` create-run internally, so it flows through the identical validate→create→spawn path
  and terminates with the standard `ingest` `done` line. `201 {id}` on success.

### Cancel (halt) — run-type-agnostic
- `POST /api/runs/{id}/halt` → `RunManager::halt` (manager.rs) operates off the **live-supervisor
  registry**, independent of run type: a live supervisor is aborted; a non-terminal run with no
  supervisor is marked halted (`Failed` + `HALT_REASON`). **Ingest runs are haltable via the same
  endpoint** (confirmed — not run-type-gated). SPA client: `haltRun(id)` in `runs.ts`.

## 2. Implementation decisions

### Submit path
- Add `IngestParams` interface + `createIngestRun(params)` to `web/src/api/runs.ts`. Because
  `/api/ingest` takes the **raw params object** (not `{type,params}`), `createIngestRun` cannot use
  `postRun` verbatim; instead it **reuses the same response choke point** — a new tiny
  `postJson(url, body)` helper wraps `fetch` + `throwForResponse`, and both `postRun` and
  `createIngestRun` call it (shared 401/error handling, no divergence).
- Add an `IngestRunMeta` variant (`type:"ingest"`, `params: IngestParams`) to the `RunMeta` union +
  an `isIngestRun` predicate, so the polled monitor can narrow an ingest run's meta honestly.

### Screen structure
- Convert the `data` destination into a small view-state machine `MarketDataArea`
  (`coverage | new | monitor`), mirroring `EvolveArea`:
  - `coverage` = the existing `MarketData` table (extended with the provenance column) + an
    **"Ingest data"** button.
  - `new` = `NewIngest` — the trigger form.
  - `monitor` = `IngestMonitor` — the standard run monitor + cancel.
- `App.tsx`: `active === 'data' ? <MarketDataArea /> : …`.

### The trigger form (`NewIngest`)
- Fields: **instruments** (comma/space-separated text → `string[]`), **fetch-all** toggle
  (a checkbox; when on, the instruments field is disabled/ignored — mirrors `validate_ingest`'s
  "either/or"), **start**/**end** (`type="date"`), **resolution** (`Select` over `RESOLUTIONS`).
  `synthetic` is **not** an operator control on this screen (the ticket scope is the real-ingest
  trigger; synthetic is the offline `qe ingest --synthetic` path). The body always sends
  `synthetic:false` so provenance is unambiguous.
- Client hints only (empty instruments AND not fetch-all ⇒ inline warn; start ≥ end ⇒ warn). The
  server's `validate_ingest` stays the enforcement point; a `400` is surfaced inline verbatim.
- Submits `createIngestRun(body)` → on `201` opens the monitor.

### Progress source + the flagged per-page follow-up
- **Decision: use the STANDARD run monitor progress** — `usePollingRun` + `<RunProgress>` (the coarse
  `RunProgress` bar every other run-kind shows), driven by the `meta.progress` `{pct, stage, msg}`
  the ingest subprocess emits. **No fabricated percentage.**
- **FLAGGED FOLLOW-UP (dependency risk):** the ticket's per-page / fine-grained percentage progress
  needs the `HistoricalSource::fetch() → one window` seam to stream/page and emit incremental
  `progress` lines. That is a **server/engine change out of scope here**; today the ingest job emits
  the standard coarse stages. This screen renders whatever `meta.progress` the server provides — it
  will automatically get finer as/when the streaming seam lands. Tracked as a follow-up.

### Cancel affordance
- A **"Cancel ingest"** danger button in `IngestMonitor` (shown while `queued|running`) calls
  `haltRun(runId)`; the poller picks up the terminal state on its next tick. Mirrors
  `CampaignMonitor`'s Halt verbatim (same endpoint, run-type-agnostic).

### Provenance column — rendering choice: **row-per-provenance-run**
- **Chosen: one row per provenance run** (NOT a `mixed` badge + drill-down). Rationale: the server
  **already** emits one `CoverageRow` per contiguous provenance run, so the honest, lowest-risk
  rendering is a **provenance badge column on each row** — the interleaved case is inherently
  multiple explicitly-marked rows, and the SPA does **no** client-side merging that could ever hide a
  provenance boundary. This satisfies "never a single unmarked range" by construction.
- Badge mapping (§8.2 — synthetic must be unmistakable): `real` → `up` (green) "REAL",
  `synthetic` → `warn` (amber) "SYNTHETIC", `unknown` → `neutral` "UNKNOWN". `calibrated` shown as a
  muted secondary tag on `real` rows (tradability inputs measured vs klines-only).
- A short caption notes that an instrument with mixed provenance appears as multiple marked rows
  (one per run), never one blended range.

## 3. Test plan (component tests, vitest + RTL)

1. `NewIngest.test.tsx` — the trigger form:
   - Fills instruments + start/end/resolution, clicks launch, asserts **exactly one POST to
     `/api/ingest`** whose body is the `IngestParams` shape:
     `instruments:["BTCUSDT","ETHUSDT"], fetch_all:false, start, end, resolution, synthetic:false`.
   - Fetch-all path: toggling fetch-all sends `fetch_all:true` with `instruments:[]`.
   - Client hint: neither instruments nor fetch-all ⇒ inline warn, no POST.
2. `IngestMonitor.test.tsx` — progress + cancel:
   - Polls a `running` ingest meta, asserts the `RunProgress` bar renders the coarse
     `{pct,stage,msg}`; clicking **Cancel ingest** fires `POST /api/runs/{id}/halt` (the halt path).
3. `MarketData.test.tsx` (extended) — provenance column incl. the interleaved case:
   - A `real` row and a `synthetic` row render their respective badges.
   - **Interleaved:** BTCUSDT `1h` with three rows `real → synthetic → real` (same symbol+resolution)
     renders **three** provenance-badged rows; assert both `REAL` and `SYNTHETIC` are present and
     **no** coverage row is unmarked (every row carries a provenance badge) — never one blended range.

## 4. Risks

- **Per-page progress dependency (flagged above):** fine-grained percentage needs the
  `HistoricalSource::fetch()` streaming seam (server/engine, out of scope). Mitigation: render the
  standard coarse `meta.progress`; no fabricated numbers; follow-up tracked.
- **Wire-mirror drift:** `runs.ts::CoverageRow`/`IngestParams` are hand-kept in lockstep with the
  Rust source of truth (`storage/src/coverage.rs`, `run-protocol/src/lib.rs`). Extended together
  here; the Rust `coverage_row_serialises_expected_shape` test pins the field names.
- **`unknown` provenance:** legacy untagged stores render `UNKNOWN` (neutral) — not softened to
  `real`, consistent with §8.2.

## 5. Green gate (run at step 4)

Frontend: `cd web && npm ci && npm run lint && npm run build && npm test`.
Rust insurance: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo build --workspace --locked`, `cargo test -p qe-architecture --test firewall --locked`,
`cargo deny check` (full `cargo test --workspace` if feasible).
