# QE-111 — SPIKE: QD/MAP-Elites archive & behaviour descriptors — design / decision record

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-110` · **Blocks: QE-118**
`Branch: qe-111/qd-archive-behaviour-descriptors`

## Goal (from backlog)

The archive's behaviour descriptors and resolution determine diversity quality and stability across
walk-forward windows.

**Scope / requirements.**
- Choose behaviour descriptors that are **structural/genotype-derived** where possible (indicator
  family, parameterised timescale, max-holding cap) to keep a genome's niche stable across windows;
  justify any outcome-derived descriptor.
- Decide archive resolution tied to a **minimum trades-per-cell** target; adopt Deep-Grid
  sub-populations (Flageat & Cully 2020) for noise robustness.
- Define per-direction archives and how the final ensemble avoids being net-long by construction.

**Acceptance criteria.**
- [ ] Decision record specifies descriptors, resolution, and sub-population size with rationale.
- [ ] A **descriptor-stability** metric is defined: cell-reassignment rate under re-evaluation on a
  different window is below a stated threshold.

**Out of scope.** Operator selection (QE-112); archive insertion / elite replacement / parallel
evaluation (QE-118); fitness (QE-120).

## Current-state evidence

- **QE-110** (`qe_wfo::genome`) gives the genome the descriptors are read off: per-direction entry
  banks (`long_entry`/`short_entry`, each a `RuleSet` of `Clause { enabled, feature, lo, hi }`),
  `exit.max_holding_bars`, and `referenced_features()`. All descriptor inputs are **genotype** — fixed
  genes, not outcomes.
- **QE-107/108** (`qe_signal`) give the `FeatureSchema`: ordered `ids()` (e.g. `rsi_14`, `ema_ratio_20`,
  `atr_pct_14`, `funding_state`, …) and now `lookbacks()` (added here, symmetric with `ids()`), plus
  `num_states()` / `len()`. The catalogue is the 22 indicators of QE-107; their ids carry the family
  and the lookback carries the timescale — both static, window-invariant.

## Decision

### D1 — All three descriptor axes are **structural / genotype-derived** (no outcome axis)

Per the spec (Theory — MAP-Elites) and the reviewer's stability concern, a genome's niche must be
**stable across walk-forward windows**. Outcome-derived descriptors (realised vol, hit-rate, turnover)
move with the data window, so a genome migrates cells under re-evaluation and the archive's diversity
guarantee decays. We therefore make **every** axis a pure function of the genotype + the static
catalogue (ids + lookbacks):

1. **Indicator family** — the *dominant* family among the direction-bank's **enabled** clauses,
   mapping each referenced feature → one of `{Trend, Momentum, Volatility, Volume, Flow}` via its
   catalogue id. Ties broken by a fixed family order (determinism). *What kind of signal the strategy
   reasons over.*
2. **Timescale** — discretised **max lookback** among the bank's referenced features
   (`Fast / Medium / Slow`). *How fast the strategy reacts.*
3. **Holding cap** — discretised `exit.max_holding_bars` (`Scalp / Swing / Position`). *How long it
   stays in.*

No outcome-derived axis is adopted; none is needed for the P1 dev universe. (Were one ever required —
e.g. a realised-direction axis — it would have to be justified against this stability metric.)

### D2 — Archive resolution tied to a minimum-trades-per-cell target

The MAP-Elites grid is the Cartesian product of the three discretised axes:

```
cells_per_direction = |families| × |timescale bands| × |holding bands| = 5 × 3 × 3 = 45
```

**Rationale (min trades/cell).** Resolution is deliberately **coarse**. An elite is only trustworthy
if evaluated on enough trades to beat noise (QE-113/124); finer descriptors fragment the population so
cells hold too few genomes/trades to fill or to support a noise-robust elite. 45 cells/direction over
the QE-012 dev universe keeps each occupied cell above the configured **min-trades-per-cell floor**
(the floor itself is enforced at evaluation in QE-118/120, not here). Bands are **config-driven** so
resolution can be tuned once real fill counts are known — count-agnostic, like QE-012.

### D3 — Deep-Grid sub-populations for noise robustness

Each cell holds up to `SUBPOP_SIZE` elites rather than a single champion (Deep-Grid, Flageat & Cully
2020): financial fitness is fat-tailed and noisy, so a single-elite cell over-commits to a lucky
evaluation. A small sub-population per cell lets QE-118 sample parents and re-evaluate without
discarding a genome on one noisy draw. **Decision: `SUBPOP_SIZE = 8`** — large enough to average out
single-evaluation noise, small enough to keep the archive compact (45 × 8 = 360 slots/direction).
Configurable; revisit with QE-124 robustness evidence.

### D4 — Per-direction archives; ensemble not net-long by construction

Two archives, **keyed by `Direction`**. A genome is placed into a direction's archive using the
descriptor computed from **that direction's** entry bank (`descriptor_for(genome, dir, schema)`); a
genome whose bank for a direction has no active/classifiable clauses simply does not occupy that
archive. The downstream ensemble search (QE-115/126) draws from **both** the Long and Short archives,
so balanced exposure is available **by construction** rather than as a post-hoc constraint — the
archive cannot structurally force net-long because short niches are maintained as first-class.

### D5 — Descriptor-stability metric (the AC)

**Metric.** `cell_reassignment_rate(assign_a, assign_b)` = the fraction of genomes whose assigned
`Cell` differs between two evaluations `a` and `b` (e.g. the same genomes re-evaluated on a different
walk-forward window). Genomes unassigned in both are excluded; an assigned↔unassigned flip counts as a
reassignment.

**Stated threshold.** `STABILITY_THRESHOLD = 0.05` (≤ 5% of genomes may change cell across windows).

**Why genotype-derived descriptors pass.** Because every axis (D1) is a pure function of the genotype
and the static catalogue — neither of which depends on the evaluation window — re-evaluating the same
genomes on any other window yields **identical** cells, so the reassignment rate is **exactly 0.0 ≤
0.05**. This is the whole point of D1, and the metric is defined precisely so a future outcome-derived
axis would be *measured* against it (the metric genuinely detects instability — proven by a test that
feeds it two differing assignment sets and gets a non-zero rate).

## Module / API plan

New module `crates/wfo/src/archive.rs`, re-exported from `qe-wfo`:

- `IndicatorFamily { Trend, Momentum, Volatility, Volume, Flow }`; `family_of(id: &str) -> Option<IndicatorFamily>`.
- `TimescaleBand { Fast, Medium, Slow }` (from a lookback); `HoldingBand { Scalp, Swing, Position }` (from `max_holding_bars`). Cutoffs are `const`, documented, config-ready.
- `Cell { family, timescale, holding }` — `Hash + Eq + Ord` (a grid coordinate).
- `descriptor_for(genome, direction, schema) -> Option<Cell>` — dominant family + max-lookback timescale + holding band from the direction's bank; `None` if no classifiable active clause.
- `grid_cells() -> impl Iterator<Item = Cell>` / `CELLS_PER_DIRECTION` — the enumerable resolution.
- `SUBPOP_SIZE`, `STABILITY_THRESHOLD` consts.
- `cell_reassignment_rate(&[Option<Cell>], &[Option<Cell>]) -> f64`.
- `qe-signal`: add `FeatureSchema::lookbacks()` (done) so timescale is genotype-derived.

## Test plan (TDD)

1. **Family classifier covers the real catalogue.** Every `FeatureSchema::from_catalogue(default)` id
   maps to `Some(family)` (guards against a new indicator silently going unclassified).
2. **Descriptor is genotype-derived & correct.** A hand-built genome → expected `Cell` (dominant
   family, timescale band from max referenced lookback, holding band); disabled clauses ignored;
   per-direction banks give per-direction cells.
3. **Descriptor stability (AC).** A set of genomes assigned "on window A" and "on window B" (the
   function ignores window data) → `cell_reassignment_rate == 0.0 ≤ STABILITY_THRESHOLD`.
4. **Metric detects instability.** Two deliberately-differing assignment sets → non-zero rate
   (proves the metric isn't trivially zero); assigned↔None flip counts.
5. **Resolution.** `grid_cells()` enumerates exactly `CELLS_PER_DIRECTION = 45` unique cells.
6. **No-active-clause / empty cases.** A genome with an all-disabled bank → `None` for that direction.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test -p qe-wfo -p qe-signal`, `cargo test --workspace`.

## Risks

- **Band cutoffs are judgement calls.** Fast/Medium/Slow and Scalp/Swing/Position boundaries are seeded
  from the 5m base grid and the catalogue's lookback spread; they are `const` now and must become
  config (QE-002) once real fill/trade counts inform the min-trades-per-cell target (D2).
- **Dominant-family loses multi-family nuance.** A genome mixing families collapses to its mode; this
  is intentional (keeps the grid coarse and stable) but means two structurally different mixes can
  share a cell. Sub-populations (D3) absorb some of this; revisit if diversity suffers.
- **Family map couples to QE-107 ids.** `family_of` matches catalogue id prefixes; a new indicator
  needs a family rule. Test #1 fails loudly if an id goes unclassified, so the coupling is guarded.
- **`SUBPOP_SIZE`/resolution are pre-data guesses.** Tuned later against QE-124 robustness + real trade
  counts; the structure (config-driven) makes that mechanical.
