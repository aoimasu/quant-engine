# QE-207 — Evaluator session (shared replay + live modes) — design note

`Phase: P2` · `Area: ③/④` · `Depends on: QE-206, QE-129` · `Branch: qe-207/evaluator-session`

## Goal (from backlog)

One evaluator session runs through bootstrap (replay) and live (wss continuation) — **no new object, no
state copy**.

- A single evaluator session supporting replay and live modes; bar-evaluation on close; loads vintage
  (chromosomes / ensemble / calibration) read-only.

**Acceptance criteria.**
- [ ] The same session instance transitions replay→live without state copy; decisions are identical across
  the boundary on a fixture.

**Out of scope.** Cutover orchestration (QE-211); order submission / sizing (the session emits per-genome
*decisions*, not orders).

## Current-state evidence & placement

- The pieces compose cleanly now: QE-206's `LiveFactorJoin` turns a bar (+ context) into a factor row; the
  vintage (QE-129 `qe_vintage::Vintage`) carries the read-only `chromosomes: Vec<Genome>`, ensemble
  `weights`, and `calibration`; and `qe_wfo::Genome::decide(&FeatureVector, PositionState) -> Decision` is
  the per-bar signal. The evaluator session wires these into one stateful object.
- **Placement: `qe-runtime`** (the bootstrap/live pipeline, Area ③/④).
- **The decoupling constraint forces a prerequisite refactor.** The QE-132 firewall is directional, but a
  *second*, stronger guard — `qe-cli/tests/dependency_topology.rs` (QE-001) — is **bidirectional**:
  `qe-runtime` must not transitively reach `qe-wfo`/`qe-ensemble`, and "the only code shared between the
  training and runtime sides crosses through `signal`/`domain`." The genome (`Genome` + `Genome::decide`)
  lived in `qe-wfo`, and `qe-vintage` embedded it — so a naïve `qe-runtime → qe-vintage → qe-wfo` edge
  breaks the invariant. The fix is the architecturally-correct one: **the genome is a pure feature-vector →
  decision mapping — signal logic — so it moves to `qe-signal`** (the shared crate both sides already
  depend on). `qe-wfo` re-exports `qe_signal::genome` so the search side's API/`crate::genome::*` are
  unchanged; `qe-vintage` embeds the genome via `qe-signal` (dropping its `qe-wfo` dep); and `qe-runtime`
  reaches the genome + vintage with **no** edge to the training side. Both guards stay green, and the
  "shared via signal/domain" rule is now *real* for strategy evaluation, not just indicators.

## Design

### D1 — One session, two modes, no state boundary

`SessionMode { Replay, Live }` is a **label**, not a state partition. The session holds all evaluation
state — the factor join (indicator warm-up) and one `PositionState` per chromosome — and `go_live()` flips
the label **without touching any of it**. So a bar evaluated as the last Replay bar and the first Live bar
see byte-identical state; the decision stream cannot change at the boundary. This *is* the AC ("no state
copy; decisions identical across the boundary").

### D2 — Bar evaluation on close

`on_bar(bar)` (a closed base bar):
1. factor row `fv = join.on_bar(bar)` (QE-206) — the same assembly offline used.
2. for each chromosome `i`: `decision = genome.decide(&fv, positions[i])`; then advance `positions[i]`:
   - `Enter(dir) → held(dir, 0)` (entered this bar);
   - `Exit → flat`;
   - `Hold → held(dir, bars_held+1)` if held, else stays flat.
   This mirrors the backtest's position bookkeeping (`bars_held` = bars since entry; `decide` exits at
   `bars_held ≥ max_holding_bars`), so the live decisions match how the genome was evaluated in search.
3. return `EvalOutput { time_ms, mode, decisions: Vec<ChromosomeDecision{index, decision}> }`.

Scalar context (funding/OI/premium) is forwarded to the join via `observe_*`, identical to QE-206.

### D3 — Read-only vintage load

The session **owns** the `Vintage` and never mutates its content. Construction verifies nothing extra
(the vintage was sealed/verified on load, QE-129); the session derives its per-chromosome positions (all
flat) from `chromosomes.len()`. Read-only accessors: `vintage_id()`, `weights()` (ensemble),
`calibration()`, `chromosome_count()`.

## Module / API plan

New module `crates/runtime/src/evaluator.rs`, re-exported from `lib.rs`:
- `SessionMode { Replay, Live }`.
- `ChromosomeDecision { index: usize, decision: qe_wfo::Decision }`.
- `EvalOutput { time_ms: i64, mode: SessionMode, decisions: Vec<ChromosomeDecision> }`.
- `EvaluatorSession { vintage, join, positions, mode }` with:
  `new(vintage, cfg) -> Self` (mode = Replay, positions flat); `mode()`, `go_live()`;
  `observe_funding/open_interest/premium(Decimal)`; `on_bar(&Bar) -> EvalOutput`;
  `weights()/calibration()/vintage_id()/chromosome_count()`.
- New deps: `qe-vintage` (+ existing `qe-signal` now also supplies `Genome`/`Decision`/`PositionState`;
  `rust_decimal`). **No `qe-wfo` dep** — that is the whole point.

### Prerequisite — relocate the genome to `qe-signal`

- Move `crates/wfo/src/genome.rs` → `crates/signal/src/genome.rs` (it imports only `qe_domain::Direction`
  + `qe_signal` feature types + serde — zero `qe-wfo` internals, so the move is mechanical).
- `qe-signal`: add `pub mod genome` + re-export `{Genome, Decision, PositionState, RuleSet, Clause,
  ExitParams, RiskParams, …}`; add `serde` dep (+ `serde_json` dev-dep for the genome's round-trip test).
- `qe-wfo`: `pub use qe_signal::genome;` + flat re-exports, so `crate::genome::*` and `qe_wfo::Genome`
  resolve unchanged (its 8 internal users + all external callers compile untouched).
- `qe-vintage`: import `qe_signal::Genome`; swap its `qe-wfo` dep for `qe-signal`.

## Test plan (TDD)

1. **Replay→live boundary has no effect (AC).** Build a vintage with a few genomes that genuinely trade.
   Run the whole bar fixture through one session in **Replay** the entire way → reference decisions. Run
   the *same* fixture through another session, calling `go_live()` at bar k → boundary decisions. Assert
   the two decision streams are **identical** bar-for-bar, and that the `mode` label is `Replay` before k
   and `Live` from k on. (If `go_live` reset any state, the post-k factor rows would be un-warm / positions
   would diverge — identical decisions prove continuity.)
2. **Decisions are non-trivial.** The fixture must contain at least one `Enter` and one `Exit` (else the
   parity is vacuous) — assert the decision stream is not all `Hold`.
3. **`go_live` is idempotent / one-way.** After `go_live`, `mode()==Live`; a second `go_live` is a no-op;
   decisions continue unaffected.
4. **Read-only load.** `chromosome_count()`, `weights()`, `vintage_id()` reflect the vintage; `weights`
   length == chromosome count.
5. **Position bookkeeping.** A single-genome session entering then holding exits at `max_holding_bars`
   (the `bars_held` advance is correct), matching `Genome::decide`.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-runtime`,
`cargo test --workspace`, `cargo test -p qe-architecture --test firewall` (must stay green with the new
`qe-runtime → qe-wfo/qe-vintage` edges), `cargo deny check`.

## Risks

- **Firewall is the headline risk** and is handled by the rule's directionality (see Placement). I will
  re-run the firewall guard explicitly after adding the deps and confirm it is green (and prove it would
  still fail on a *real* breach — e.g. a `qe-wfo → qe-runtime` edge — unchanged from QE-132).
- **"No state copy" is enforced by construction**, not convention: `go_live` only assigns the mode field;
  there is no second evaluator object and no reset path. The test makes a reset observable (it would change
  decisions) and asserts it does not happen.
- **Position semantics** match the backtest's so live decisions equal search-time decisions for the same
  factor rows; the per-genome `PositionState` advance is the only bookkeeping and is unit-tested.
- **Catalogue config.** The session takes the `CatalogueConfig` the vintage's genomes were evolved against
  (feature indices must line up). A mismatching width would surface via `Genome::decide` reading a
  clamped/empty schema — out of scope to validate here (lineage guarantees it), but the constructor could
  later assert `schema` vs a vintage-recorded schema.
```
