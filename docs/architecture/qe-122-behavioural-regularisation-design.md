# QE-122 — Behavioural regularisation — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-118`
`Branch: qe-122/behavioural-regularisation`

## Goal (from backlog)

Keeps the archive behaviourally regular/diverse and counters degenerate crowding.

- Implement the regularisation defined in QE-111 (e.g. niche penalties / novelty pressure).

**Acceptance criteria.**
- [ ] Archive diversity metric improves vs an ablation without regularisation on a fixture run.

**Out of scope.** Persistence (QE-123).

## Current-state evidence

- **QE-111/118** give the niche grid (`Cell` = family × timescale × holding) and the per-direction
  `MapElitesArchive` with bounded Deep-Grid cells. Parent selection so far is uniform (QE-118) or
  Thompson over *reward* (QE-121) — neither pushes the population to **spread** across niches, so a search
  that keeps reproducing from the early-discovered region crowds a few cells while the frontier stays
  empty. QE-122 adds the missing **novelty pressure / niche penalty**.
- `TimescaleBand`/`HoldingBand` are **ordinal** (3 bands each), so a cell has a natural ordinal
  neighbourhood (±1 band in timescale or holding, family fixed) — the behaviour-space locality novelty
  pressure is defined over.

## Design

### D1 — Diversity metrics

Two read-only metrics on a direction archive:
- `coverage` — the number of occupied cells (filled niches). The primary QD diversity metric and the
  AC's yardstick.
- `occupancy_entropy` — Shannon entropy of the per-cell elite counts (nats). Higher ⇒ a more *even*
  spread (less degenerate crowding); a secondary metric.

### D2 — Behavioural neighbourhood & local crowding

`neighbours(cell)` = the cells one ordinal step away in timescale **or** holding, **same family**
(family is categorical — no ordering). So each cell has up to 4 neighbours (a 3×3 grid per family).
`local_crowding(archive, direction, cell)` = the number of that cell's neighbours that are **occupied** —
an interior cell (all neighbours filled) is maximally crowded; a frontier cell (few occupied neighbours)
is novel.

### D3 — Niche penalty / novelty pressure (the regulariser)

`BehaviouralRegulariser { pressure }` turns crowding into a reproduction weight:

```
novelty_weight(cell) = 1 / (1 + pressure · local_crowding(cell))
```

`select_parent_cell(archive, direction, rng)` samples an **occupied** cell with probability proportional
to its novelty weight, then the caller varies an elite from it. Crowded interior cells are *penalised*;
frontier cells reproduce more, so cell-local variation pushes offspring into the adjacent **empty**
niches — coverage grows along the frontier instead of re-saturating the interior. `pressure = 0`
degenerates to uniform selection (the ablation). Deterministic through the seeded `DetRng`.

### D4 — Why coverage improves vs the ablation (AC)

With a fixed reproduction budget and the Deep-Grid bound, **uniform** parent selection spends many
reproductions on crowded interior cells that are already full (`Rejected`, no new coverage), while
**novelty-pressure** selection concentrates the budget on frontier cells whose cell-local offspring land
in empty neighbours. So at a fixed step count the regularised run reaches more distinct niches. The
fixture demonstrates exactly this: the same seeded behaviour-space random walk run with `pressure > 0`
covers strictly more cells than the `pressure = 0` ablation.

## Module / API plan

New module `crates/wfo/src/regularise.rs`, re-exported:

- `coverage(&MapElitesArchive, Direction) -> usize`; `occupancy_entropy(&MapElitesArchive, Direction) -> f64`.
- `neighbours(Cell) -> Vec<Cell>`; `local_crowding(&MapElitesArchive, Direction, &Cell) -> usize`.
- `BehaviouralRegulariser::{new, with_defaults, novelty_weight, select_parent_cell}`; `DEFAULT_NOVELTY_PRESSURE`.
- Reuses `qe_wfo::{archive, mapelites}`; `qe_determinism::DetRng`. No new dependencies.

## Test plan (TDD)

1. **Neighbourhood.** `neighbours` returns the ±1-band timescale/holding cells, same family, clamped at
   the grid edges; never the cell itself; corner cells have 2 neighbours, centre has 4.
2. **Crowding & weight.** `local_crowding` counts occupied neighbours; `novelty_weight` is monotonically
   decreasing in crowding; `pressure = 0` ⇒ uniform weights.
3. **Metrics.** `coverage` counts occupied cells; `occupancy_entropy` is 0 for a single occupied cell and
   maximal for an even spread.
4. **Diversity improves vs ablation (AC).** A seeded behaviour-space random walk over the reachable cells,
   reproducing via novelty-pressure parent selection, ends with strictly higher `coverage` than the same
   walk with `pressure = 0`.
5. **Determinism.** Same seed ⇒ identical parent-cell selections.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Neighbourhood is ordinal only over timescale/holding.** Family is categorical, so novelty pressure
  diffuses *within* a family's 3×3 grid; cross-family novelty is left to the exploratory operators
  (QE-119) and fresh-random. Documented; a learned behaviour metric is a later refinement.
- **`pressure` is a pre-data constant.** Config-ready (QE-002); the *direction* of the effect (more
  coverage) is robust to the exact value, only its magnitude varies.
- **Standalone regulariser.** Wiring novelty-pressure selection into the live search loop (alongside the
  QE-121 Thompson reward) is QE-124+/loop scope; QE-122 delivers and proves the regulariser in isolation.
