# QE-110 — SPIKE: Strategy genome representation — design / decision record

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-107` · **Blocks: QE-118, QE-119, QE-120**
`Branch: qe-110/strategy-genome-representation`

## Goal (from backlog)

The genome was left an open design decision; it must be **fixed** before archive/operator work
since everything mutates it.

**Scope / requirements (produce a design + decision record).**
- Decide representation: rule sets over quantised indicator states **vs** fixed-structure parameter
  vector (entry/exit/position conditions + risk + holding params), with rationale.
- Define mutation/crossover surface, validity constraints, and serialisation.
- Provide a reference fixture genome + hand-traced expected decisions.

**Acceptance criteria.**
- [ ] A written decision record fixes the genome.
- [ ] A fixture genome evaluates to the documented decisions.
- [ ] The representation supports the operators QE-119 will implement.

**Out of scope.** Operator tuning (QE-112); operator implementation (QE-119); backtest evaluation /
fitness (QE-120); archive descriptors (QE-111, though we make them readable).

## Current-state evidence

- **QE-107/108** give the substrate the genome reasons over:
  - `qe_signal::FeatureVector { time_ms: i64, states: Vec<Option<QState>> }` — one slot per catalogue
    indicator, in **schema order**, `None` until that indicator is warm.
  - `qe_signal::FeatureSchema` — ordered indicator `ids()`, `len()` (== catalogue size), `num_states()`
    (uniform per-indicator state count, ≥ 2), `max_lookback()`. Built from `CatalogueConfig`.
  - `qe_signal::QState` — a discrete state `0..num_states`; `QState::from_index(u16)` / `.index()`.
- **QE-007** gives `qe_domain::Direction { Long, Short }` (and `Side`), the position vocabulary.
- **`qe-wfo`** already depends on `qe-signal`, `qe-domain`, `rust_decimal`. QE-109's `friction`
  module is the cost primitive the backtester (QE-120) will drive; the genome's `Decision` stream is
  the *other* input to that backtester.
- **Downstream consumers that constrain this choice:**
  - **QE-118 (MAP-Elites archive)** + **QE-126/115 (discrete differential evolution)** operate on the
    genome as a **search point** — DE in particular assumes a **fixed-length, position-wise** encoding
    so difference vectors `a + F·(b − c)` are well-defined per-locus.
  - **QE-119 (variation operators: local refine / explore / fresh random)** mutate and recombine the
    genome — they need a **stable, enumerable locus set** and a cheap **repair-to-valid** contract.
  - **QE-111 (behaviour descriptors)** wants **structural/genotype-derived** descriptors (indicator
    family, timescale, max-holding cap) read directly off the genome so a niche is stable across
    windows — i.e. these must be *legible from the genes*, not only from outcomes.

## Decision

### D1 — Representation: a **fixed-structure parameter vector that encodes a bounded rule set** (hybrid)

The two backlog options are a false dichotomy for this platform; we adopt the **hybrid that is the
strict superset both downstreams need**:

- *Fixed-structure parameter vector* — DE/MAP-Elites need fixed loci; a variable-length free rule
  graph makes per-locus difference vectors and stable niching ill-defined.
- *Rule sets over quantised states* — the spec frames strategies as logic over quantised indicator
  states; a flat scalar vector cannot express "indicator X in state-band [lo,hi]".

**Chosen:** a **fixed number of rule slots ("clauses"), each a fixed-structure clause**, grouped into
**per-direction entry banks** plus fixed exit/risk/holding genes. The genome is therefore a
**fixed-length vector of typed genes** (DE/MAP-Elites friendly) whose genes **encode a k-of-n rule set
over quantised states** (expressive). Clauses carry an `enabled` flag so the *effective* rule count is
itself evolvable without changing genome length — operators toggle clauses rather than resizing.

### D2 — Concrete structure

```
Genome {
  version: u16,                 // representation version (REP_VERSION), for lineage / decode safety
  long_entry:  RuleSet,         // fires ⇒ a long is permitted when flat   (per-direction, QE-111)
  short_entry: RuleSet,         // fires ⇒ a short is permitted when flat
  exit:  ExitParams,
  risk:  RiskParams,
}

RuleSet {
  clauses: [Clause; CLAUSES_PER_SET],   // fixed N (CLAUSES_PER_SET = 4)
  min_satisfied: u8,                    // k in "k-of-active"; clamped to 1..=active at eval
}

Clause {
  enabled: bool,     // disabled clauses are ignored (lets effective rule-count evolve)
  feature: u16,      // index into FeatureSchema (which indicator)
  lo: u16,           // inclusive lower state band
  hi: u16,           // inclusive upper state band   (lo ≤ hi < num_states)
}
// Clause satisfied ⇔ enabled ∧ feature is warm ∧ lo ≤ state[feature].index() ≤ hi

ExitParams { max_holding_bars: u16, exit_on_opposite: bool }
RiskParams { size_bps: u16 }    // target notional as basis-points of allowed capital, 1..=10_000
```

### D3 — Decision semantics (the evaluator)

`Genome::decide(features, position) -> Decision`, `Decision ∈ { Hold, Enter(Direction), Exit }`.

- **Flat** (`position.dir == None`):
  - `long = long_entry.fires(features)`, `short = short_entry.fires(features)`.
  - `long ∧ ¬short` ⇒ `Enter(Long)`; `short ∧ ¬long` ⇒ `Enter(Short)`;
  - both or neither ⇒ `Hold` (ambiguous/!signal never enters — no accidental net-long bias).
- **In position** (`dir = d`, `bars_held = h`):
  - `Exit` if `h ≥ exit.max_holding_bars`, **or** (`exit.exit_on_opposite` ∧ the opposite direction's
    entry bank fires); else `Hold`.
- A `RuleSet` **fires** ⇔ `count(satisfied clauses) ≥ min(min_satisfied, active_count)` **and**
  `active_count ≥ 1` (an all-disabled bank never fires). Position **size** is *not* in `Decision`;
  the backtester (QE-120) reads `risk.size_bps` on entry — keeps `Decision` a pure signal.

Determinism: evaluation is a pure function of `(genome, features, position)` — no RNG, no clock, no
hidden state — so it is identical batch vs streaming and reproducible (QE-006).

**Reference fixture trace.** The checked-in fixture genome (`fixture_genome()` in `genome.rs`) over
3 features, state range `0..=4` — long when f0 **and** f1 are high `[3,4]` (`min_satisfied = 2`),
short when f0 **and** f1 are low `[0,1]` (`min_satisfied = 2`), `max_holding_bars = 3`,
`exit_on_opposite = true` — evaluates to these decisions (the AC test asserts each):

| Case | Position | Features `[f0,f1,f2]` | Long fires | Short fires | Decision |
|------|----------|-----------------------|-----------|------------|----------|
| A | flat | `[4,4,0]` | yes (2/2) | no | `Enter(Long)` |
| B | flat | `[4,2,0]` | no (1/2) | no | `Hold` |
| C | flat | `[0,0,0]` | no | yes (2/2) | `Enter(Short)` |
| D | flat | both banks armed on same band* | yes | yes | `Hold` (ambiguous) |
| E | Long, held 3 | any | — | — | `Exit` (max holding) |
| F | Long, held 1 | `[0,0,0]` | — | yes | `Exit` (opposite signal) |
| G | Long, held 1 | `[4,2,0]` | — | no | `Hold` |

\* Case D uses a separate genome whose long and short banks share one clause band, so a single input
fires both — asserted by `both_banks_firing_is_ambiguous_hold`.

### D4 — Mutation / crossover surface + validity (for QE-112/QE-119)

Operators get a **"mutate freely, then repair"** contract — the cheapest possible surface for DE and
random mutation:

- **`Genome::is_valid(schema) -> bool`** — every gene within domain (see D5).
- **`Genome::repair(schema)`** — clamps every gene back into its valid domain **deterministically**
  (idempotent; `repair` twice == once). Operators may produce out-of-domain genes (e.g. DE arithmetic,
  uniform mutation) and call `repair` to land back on the constraint manifold.
- **Locus legibility** — the gene layout is fixed and documented (D2), so QE-119 can enumerate loci
  (per-clause `feature/lo/hi/enabled`, per-set `min_satisfied`, exit/risk genes) for uniform/typed
  mutation and per-locus crossover, and QE-126's discrete DE can treat the integer genes position-wise.
- **Descriptor legibility (QE-111)** — `referenced_features()` returns the set of feature indices used
  by enabled clauses; timescale and `max_holding_bars` are read directly. These are genotype-derived,
  so a genome's niche is stable across re-evaluation windows.

### D5 — Validity constraints (the constraint manifold `repair` enforces)

- `clause.feature < schema.len()` (clamped into range).
- `clause.lo ≤ clause.hi` and `clause.hi < schema.num_states()` (clamp `hi`, then `lo ≤ hi`).
- `1 ≤ min_satisfied ≤ CLAUSES_PER_SET` (clamped). Effective threshold further min'd by active count
  at eval, so a bank with few enabled clauses still fires sensibly.
- `1 ≤ risk.size_bps ≤ 10_000`.
- `exit.max_holding_bars ≥ 1`.
- A genome on the empty schema (`len == 0`) is degenerate by construction; `repair` is a no-op and the
  genome simply never fires (guarded so it cannot panic).

### D6 — Serialisation

`serde` derive on every type (the workspace already standardises on `serde` + `serde_json` with exact
string money). The canonical form is JSON (human-diffable, lineage-friendly); the reference fixture is
checked in as the documented genome. `REP_VERSION` is a field so a future representation change is a
loud decode mismatch, mirroring QE-108's versioned schema header.

## Module / API plan

New module `crates/wfo/src/genome.rs`, re-exported from `qe-wfo`:

- Types: `Genome`, `RuleSet`, `Clause`, `ExitParams`, `RiskParams`, `Decision`, `PositionState`,
  consts `REP_VERSION`, `CLAUSES_PER_SET`.
- `Clause::satisfied(&FeatureVector) -> bool`
- `RuleSet::fires(&FeatureVector) -> bool`
- `Genome::decide(&FeatureVector, PositionState) -> Decision`
- `Genome::is_valid(&FeatureSchema) -> bool` / `Genome::repair(&FeatureSchema)`
- `Genome::referenced_features() -> BTreeSet<u16>`
- `Cargo.toml`: add `serde.workspace = true` (+ `serde_json` dev-dep for the fixture round-trip test).

## Test plan (TDD)

1. **Fixture genome + hand-traced decisions** (the AC). A small explicit 3-feature / 5-state schema,
   a documented genome, and a sequence of `FeatureVector`s whose expected `Decision`s are hand-computed
   in the test (long entry, no-entry on ambiguity, max-holding exit, opposite-signal exit, hold).
2. **k-of-n firing**: `min_satisfied` threshold and `enabled` toggling change firing as specified;
   warm-vs-`None` slots; all-disabled bank never fires.
3. **Validity/repair**: out-of-domain genes (`feature ≥ len`, `hi ≥ num_states`, `lo > hi`,
   `size_bps = 0`, `max_holding = 0`, `min_satisfied = 0`) → `repair` lands `is_valid` true and is
   idempotent.
4. **Determinism / parity**: `decide` is pure — same inputs, same output; repeated calls equal.
5. **Serde round-trip**: `Genome` → JSON → `Genome` is identity; `REP_VERSION` present.
6. **Empty-schema guard**: degenerate genome never panics, never fires.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test -p qe-wfo`, `cargo test --workspace`.

## Risks

- **Expressiveness ceiling.** A k-of-n band-clause rule set cannot express arbitrary nested logic
  (OR-of-ANDs beyond one k-of-n layer). Accepted: it is DE/MAP-Elites-tractable and matches the spec's
  "logic over quantised states"; richer logic is a future representation bump behind `REP_VERSION`.
- **Fixed `CLAUSES_PER_SET`.** Caps rule complexity; `enabled` flags make the *effective* count
  evolvable, and 4 clauses × 2 directions is ample for the P1 dev universe. Revisit with evidence.
- **Size/stop minimalism.** Only `size_bps` + `max_holding` + opposite-signal exit live in the genome;
  hard stops / breakers are the runtime/risk layer (QE-116/QE-212), not the search genome — kept out
  deliberately to avoid the genome overfitting stop placement.
- **`version` is stamped but not yet *enforced* on decode.** `REP_VERSION` is written into every genome
  and normalised by `repair`, but nothing in this SPIKE rejects a mismatched `version` — `serde` will
  deserialise an old/foreign version silently, and `is_valid` only checks the constraint manifold (not
  the version). The D6 "loud decode mismatch" guarantee must therefore be wired at the genuine
  decode/load entrypoint when genome persistence/lineage lands (QE-118 / vintage artefact, QE-129).
  Tracked here so the promise is not lost.
