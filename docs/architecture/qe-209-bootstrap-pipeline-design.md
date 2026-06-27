# QE-209 — Bootstrap pipeline — design note

`Phase: P2` · `Area: ③ Bootstrap` · `Depends on: QE-201, QE-207` · `Branch: qe-209/bootstrap-pipeline`

## Goal (from backlog)

On startup, replay the lookback window through the evaluator to reconstruct state **to where a
continuously-running planner would hold it**.

**Scope / requirements.** REST fetchers (paginated + retried) → ephemeral cache → multi-resolution replay
→ factor merge + markPrice replay (1-min cadence) → evaluator in replay mode.

**Acceptance criteria.**
- [ ] A cold start reconstructs per-strategy state deterministically from REST historicals.

**Out of scope.** Reconstructed-state object (QE-210); cutover (QE-211); the venue-specific Binance JSON
schema decoders + request builders (assembled in the CLI/runtime wiring, QE-219).

## Current-state evidence & placement

Every piece the bootstrap composes already exists and is independently parity-/determinism-proven:

- **Paginated + retried + cached REST** — `qe_venue::VenueRestClient::{fetch, paginate}` (QE-201): weighted
  rate-limit back-off, retry budget, and an ephemeral **closed-window cache**, all behind the
  `RestTransport`/`Clock` seams (offline-testable). `paginate(first, next)` drives page-by-page fetch.
- **Multi-resolution replay** — `qe_runtime::LiveKlineSource` (QE-205): primes from closed historical base
  bars, stitches/dedups on a monotonic open-time marker, and fans each base bar out to a per-tier
  `BarReconstructor` (the shared QE-106 roll-up), so coarse bars match batch reconstruction exactly.
- **Factor merge** — `qe_runtime::LiveFactorJoin` (QE-206): as-of join of funding/OI/premium onto base
  bars driving the shared catalogue; live factor rows equal offline feature vectors bar-for-bar. The
  catalogue indicators roll base bars up to 30m/4h **internally**, so the feature path consumes the base
  bar stream — multi-resolution feature state needs no separately-fed coarse bars.
- **Evaluator in replay mode** — `qe_runtime::EvaluatorSession` (QE-207): one session runs a sealed
  vintage's chromosomes bar-by-bar in `Replay`, owns the factor join + one `PositionState` per chromosome,
  and `go_live()` flips only the mode label (no state copy) — the continuity QE-209 depends on.

The missing piece is the **orchestration**: fetch the lookback window over REST, merge bars + scalar
context into one timestamp-ordered replay, drive the evaluator in `Replay`, and hand back a **warmed**
session whose per-strategy state equals a continuously-running planner's. Placement: **`qe-runtime`**
(Area ③ Bootstrap; already deps `qe-venue`/`qe-signal`/`qe-vintage` — no new crate edge, firewall +
QE-001 decoupling unaffected).

## Design

### D1 — The REST seam: `HistoricalSource` → `HistoricalWindow`

`HistoricalWindow` is the typed cold-start input — what the lookback fetch yields, already decoded:

```
HistoricalWindow {
    base: Resolution,                 // base kline resolution (e.g. M5)
    bars: Vec<Bar>,                   // base-resolution klines, any order / possibly page-overlapping
    funding:       Vec<(i64, Decimal)>,   // (ts_ms, value) observations
    open_interest: Vec<(i64, Decimal)>,
    premium:       Vec<(i64, Decimal)>,
    mark_price:    Vec<(i64, Decimal)>,   // 1-min cadence
}
```

`HistoricalSource::fetch(&mut self) -> Result<HistoricalWindow, BootstrapError>` is the network seam. The
**real** implementation paginates each series through `VenueRestClient::paginate` (retried + cached) and
decodes each page; tests use an in-memory window. To make the paginated path real and tested **without**
hardcoding the Binance JSON schema in the runtime crate (that belongs to the venue/CLI wiring, QE-219),
QE-209 ships two reusable primitives over QE-201:

- `paginate_klines(client, first, next, decode) -> Result<Vec<Bar>, BootstrapError>`
- `paginate_series(client, first, next, decode) -> Result<Vec<(i64, Decimal)>, BootstrapError>`

each mapping every `RestResponse` page through an injected `decode` closure and concatenating — so the
caller supplies the venue schema, the bootstrap owns the paginate-decode-collect loop (rate-limit/retry/
cache come for free from `paginate`). A venue-bound `HistoricalSource` is then a thin struct over these.

### D2 — Deterministic replay (`BootstrapPipeline::replay`)

`BootstrapPipeline { cfg: CatalogueConfig, tiers: Vec<Resolution> }`; `replay(&self, window, vintage) ->
Reconstructed` is a **pure function of its inputs** (no clock, no RNG):

1. **Stitch/dedup** the base bars: sort by `open_time`, drop any with `open_time ≤` the previous kept one
   (the QE-205 marker rule), yielding a gap-free, strictly-increasing base sequence — so overlapping REST
   pages cannot double-count.
2. **Multi-resolution replay**: prime a `LiveKlineSource::new(base, tiers)` with the deduped bars and
   `finish()` it → `coarse_bars` (the reconstructed 30m/4h tiers, surfaced for QE-210/persistence; equals
   `reconstruct_batch` of the base bars by construction).
3. **Factor merge timeline**: fold the base bars and every scalar-context observation into one event list
   sorted by `(ts, kind-ordinal)` with **context ordered before a bar at equal ts** — exactly the as-of
   `value.ts ≤ bar.open_time` rule QE-206 proved. (markPrice replays at its own 1-min cadence in the same
   timeline.)
4. **Evaluate in replay**: `EvaluatorSession::new(vintage, cfg)` (starts in `Replay`); walk the timeline —
   a context event calls the matching `observe_*` (funding/OI/premium feed the factor join; **markPrice**
   updates a tracked `last_mark_price` for the risk/cutover layer, not a feature input), a bar event calls
   `on_bar` and its `EvalOutput` is collected.
5. Return `Reconstructed { session, decisions, coarse_bars, bars_replayed, last_mark_price }` — the
   **warmed** session (per-chromosome state reconstructed), ready for `go_live()` cutover (QE-211).

`cold_start(&self, source, vintage)` = `fetch` then `replay`.

### D3 — "State equals a continuously-running planner"

The reconstructed session **is** the same object type the live planner runs (QE-207), warmed by the same
`on_bar` path the live loop uses — so the boundary is a no-op label flip. The test pins this: a session
bootstrapped over bars `[0..n)` then `go_live()`-d and fed bar `n` decides **identically** to one
continuous session fed `[0..n]` — i.e. cold-start state == continuously-running state (the ticket's "Why").

## Module / API plan

New module `crates/runtime/src/bootstrap.rs`, re-exported from `lib.rs`:

- `HistoricalWindow { base, bars, funding, open_interest, premium, mark_price }`.
- `HistoricalSource` (trait) with `fetch`.
- `BootstrapError` — `Recon(ReconError)` | `Rest(RestError)` | `Decode(String)`.
- `Reconstructed { session: EvaluatorSession, decisions: Vec<EvalOutput>, coarse_bars: Vec<Bar>,
  bars_replayed: usize, last_mark_price: Option<Decimal> }`.
- `BootstrapPipeline::{ new(cfg, tiers), replay(&window, vintage), cold_start(&mut source, vintage) }`.
- `paginate_klines` / `paginate_series` — paginated+retried+cached fetch-and-decode over
  `VenueRestClient::paginate` (generic over `RestTransport`/`Clock`).

No new crate dependencies (`qe-venue`, `qe-signal`, `qe-vintage`, `qe-domain`, `rust_decimal` already
present); firewall + QE-001 decoupling unchanged.

## Test plan (TDD)

1. **Deterministic reconstruction (AC).** A window of ~40 base bars + funding/OI/premium/mark observations;
   `replay` twice → identical `decisions` and identical `session` observables. Non-vacuous: the decision
   stream contains ≥1 `Enter` and ≥1 `Exit`.
2. **State-handoff == continuous run (the "Why").** Bootstrap over `[0..n)`, `go_live()`, feed bar `n`;
   assert its decisions equal a single continuous `EvaluatorSession` fed `[0..n]`.
3. **Pagination invariance + retry (QE-201 path).** A fake `RestTransport`/`Clock` serving the same klines
   as 1 page vs 3 pages (with a transient error to exercise retry/back-off and the cache); `paginate_klines`
   → identical `Vec<Bar>`, and both feed `replay` to identical reconstructions.
4. **As-of merge matches a hand-driven evaluator.** The same bars+context driven directly through an
   `EvaluatorSession` (interleaved in ts order) → identical decisions to `replay`; a funding update strictly
   between two bars applies to the later bar.
5. **Multi-resolution parity.** `Reconstructed.coarse_bars` equals `reconstruct_batch(base, tier, deduped)`
   for each tier.
6. **Stitch/dedup.** A window with duplicated boundary bars (overlapping pages) reconstructs identically to
   the deduped window; `bars_replayed` == unique count.
7. **Cold start with thin history.** A window too short to warm any indicator → all-`Hold` decisions (no
   spurious entries), `replay` succeeds (no panic).

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-runtime`,
`cargo test --workspace`, `cargo test -p qe-cli --test dependency_topology` (decoupling unchanged),
`cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Determinism is the AC** and holds because every composed piece is deterministic and `replay` adds only
  a pure sort/merge — no clock/RNG. Pinned by run-twice equality and pagination-invariance.
- **As-of tie-break** (context at `ts == bar.open_time` applies to that bar) must match QE-206's `≤` rule;
  pinned by test 4 against a hand-driven evaluator, the QE-206 reference semantics.
- **markPrice semantics**: markPrice is fetched and replayed at 1-min cadence but is **not** a catalogue
  feature input (the feature context is funding/OI/premium); it is surfaced as `last_mark_price` for the
  risk/cutover layer (QE-210/211). Documented to avoid fabricating a feature meaning here.
- **No new crate edge** → firewall + QE-001 decoupling guards stay green (re-run to confirm).
