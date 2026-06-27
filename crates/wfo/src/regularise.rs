//! Behavioural regularisation (QE-122) — novelty pressure / niche penalty that keeps the archive
//! behaviourally diverse and counters degenerate crowding.
//!
//! Parent selection so far (uniform, QE-118; Thompson-over-reward, QE-121) does not push the population
//! to *spread*, so a search that keeps reproducing from the early-discovered region crowds a few niches
//! while the frontier stays empty. [`BehaviouralRegulariser`] turns local crowding into a reproduction
//! penalty: frontier cells (few occupied neighbours) reproduce more, so cell-local variation pushes
//! offspring into the adjacent empty niches and coverage grows along the frontier instead of
//! re-saturating the interior. The neighbourhood ([`neighbours`]) is ±1-band ordinal in timescale/holding
//! plus every other family at the same (timescale, holding) — family being categorical — which connects
//! the whole 45-cell behaviour space. Two read-only diversity metrics — [`coverage`] and
//! [`occupancy_entropy`] — make the effect measurable. `pressure = 0` degenerates to uniform (the ablation).

use qe_determinism::DetRng;
use qe_domain::Direction;
use rand_core::RngCore;

use crate::archive::{Cell, FAMILIES, HOLDINGS, TIMESCALES};
use crate::mapelites::MapElitesArchive;

/// Default novelty-pressure strength.
pub const DEFAULT_NOVELTY_PRESSURE: f64 = 4.0;

/// The number of occupied cells (filled niches) in a direction — the primary QD diversity metric.
#[must_use]
pub fn coverage(archive: &MapElitesArchive, direction: Direction) -> usize {
    archive.direction(direction).len()
}

/// Shannon entropy (nats) of the per-cell elite counts in a direction — a measure of how *evenly* the
/// population is spread across occupied niches (higher ⇒ less degenerate crowding). `0` for ≤ 1 occupied
/// cell.
#[must_use]
pub fn occupancy_entropy(archive: &MapElitesArchive, direction: Direction) -> f64 {
    let dir = archive.direction(direction);
    let counts: Vec<f64> = dir
        .occupied_cells()
        .filter_map(|c| dir.cell(c))
        .map(|s| s.len() as f64)
        .collect();
    let total: f64 = counts.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    -counts
        .iter()
        .filter(|&&n| n > 0.0)
        .map(|&n| {
            let p = n / total;
            p * p.ln()
        })
        .sum::<f64>()
}

/// The behavioural neighbours of `cell`: cells one step away along exactly one axis. Timescale and
/// holding are **ordinal** (±1 band); family is **categorical**, so every *other* family at the same
/// (timescale, holding) is an equidistant neighbour. So up to `2 + 2 + (FAMILIES − 1)` neighbours
/// (edge-clamped for timescale/holding); never includes `cell` itself. This connects the whole 45-cell
/// behaviour space into one graph along which novelty pressure diffuses.
#[must_use]
pub fn neighbours(cell: Cell) -> Vec<Cell> {
    let ti = TIMESCALES.iter().position(|&t| t == cell.timescale);
    let hi = HOLDINGS.iter().position(|&h| h == cell.holding);
    let mut out = Vec::with_capacity(2 + 2 + FAMILIES.len() - 1);
    if let Some(ti) = ti {
        for d in [-1i64, 1] {
            let j = ti as i64 + d;
            if (0..TIMESCALES.len() as i64).contains(&j) {
                out.push(Cell {
                    family: cell.family,
                    timescale: TIMESCALES[j as usize],
                    holding: cell.holding,
                });
            }
        }
    }
    if let Some(hi) = hi {
        for d in [-1i64, 1] {
            let j = hi as i64 + d;
            if (0..HOLDINGS.len() as i64).contains(&j) {
                out.push(Cell {
                    family: cell.family,
                    timescale: cell.timescale,
                    holding: HOLDINGS[j as usize],
                });
            }
        }
    }
    for &family in FAMILIES.iter().filter(|&&f| f != cell.family) {
        out.push(Cell {
            family,
            timescale: cell.timescale,
            holding: cell.holding,
        });
    }
    out
}

/// The number of `cell`'s behavioural [`neighbours`] that are **occupied** in `direction` — interior
/// cells (all neighbours filled) are maximally crowded; frontier cells are novel.
#[must_use]
pub fn local_crowding(archive: &MapElitesArchive, direction: Direction, cell: &Cell) -> usize {
    let dir = archive.direction(direction);
    neighbours(*cell)
        .into_iter()
        .filter(|n| dir.cell(n).is_some())
        .count()
}

/// Novelty-pressure parent-cell selector: a niche penalty that weights occupied cells inversely to local
/// crowding, so frontier cells reproduce more often.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BehaviouralRegulariser {
    /// Strength of the niche penalty (`0` ⇒ uniform selection — the ablation).
    pub pressure: f64,
}

impl Default for BehaviouralRegulariser {
    fn default() -> Self {
        BehaviouralRegulariser {
            pressure: DEFAULT_NOVELTY_PRESSURE,
        }
    }
}

impl BehaviouralRegulariser {
    /// Build a regulariser with an explicit pressure (`< 0` is floored to `0`).
    #[must_use]
    pub fn new(pressure: f64) -> Self {
        BehaviouralRegulariser {
            pressure: pressure.max(0.0),
        }
    }

    /// A regulariser with the QE-122 default pressure.
    #[must_use]
    pub fn with_defaults() -> Self {
        BehaviouralRegulariser::default()
    }

    /// The reproduction weight of `cell` = `1 / (1 + pressure · local_crowding)` — monotonically
    /// decreasing in crowding; `pressure = 0` ⇒ a uniform weight of `1`.
    #[must_use]
    pub fn novelty_weight(
        &self,
        archive: &MapElitesArchive,
        direction: Direction,
        cell: &Cell,
    ) -> f64 {
        let crowding = local_crowding(archive, direction, cell) as f64;
        1.0 / (1.0 + self.pressure * crowding)
    }

    /// Sample an **occupied** parent cell with probability proportional to its [`novelty_weight`], from
    /// one `DetRng` draw. `None` if the direction's archive is empty. Deterministic for a given rng state.
    ///
    /// [`novelty_weight`]: BehaviouralRegulariser::novelty_weight
    #[must_use]
    pub fn select_parent_cell(
        &self,
        archive: &MapElitesArchive,
        direction: Direction,
        rng: &mut DetRng,
    ) -> Option<Cell> {
        let dir = archive.direction(direction);
        let cells: Vec<Cell> = dir.occupied_cells().copied().collect();
        if cells.is_empty() {
            return None;
        }
        let weights: Vec<f64> = cells
            .iter()
            .map(|c| self.novelty_weight(archive, direction, c))
            .collect();
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Some(cells[0]);
        }
        let mut u = uniform01(rng) * total;
        for (cell, w) in cells.iter().zip(weights.iter()) {
            u -= w;
            if u < 0.0 {
                return Some(*cell);
            }
        }
        cells.last().copied() // float round-off fallthrough
    }
}

/// Uniform in `[0, 1)` from the 53 high bits of one `u64` draw.
fn uniform01(rng: &mut DetRng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{descriptor_for, HoldingBand, IndicatorFamily, TimescaleBand};
    use crate::genome::{
        Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
    };
    use qe_determinism::task_rng;
    use qe_signal::{CatalogueConfig, FeatureSchema};
    use std::collections::BTreeMap;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    fn long_genome(feature: u16, hold: u16) -> Genome {
        let mut clauses = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature,
            lo: 1,
            hi: 2,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [Clause {
                    enabled: false,
                    feature: 0,
                    lo: 0,
                    hi: 0,
                }; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn cell(family: IndicatorFamily, timescale: TimescaleBand, holding: HoldingBand) -> Cell {
        Cell {
            family,
            timescale,
            holding,
        }
    }

    #[test]
    fn neighbours_span_ordinal_bands_and_other_families() {
        // 4 other families add a cross-family neighbour each (same timescale/holding).
        let others = FAMILIES.len() - 1; // 4
                                         // Centre cell (Medium / Swing) → 4 ordinal + 4 family = 8.
        let centre = cell(
            IndicatorFamily::Trend,
            TimescaleBand::Medium,
            HoldingBand::Swing,
        );
        let n = neighbours(centre);
        assert_eq!(n.len(), 4 + others);
        assert!(!n.contains(&centre));
        // Corner (Fast / Scalp) → 2 ordinal + 4 family = 6.
        let corner = cell(
            IndicatorFamily::Flow,
            TimescaleBand::Fast,
            HoldingBand::Scalp,
        );
        assert_eq!(neighbours(corner).len(), 2 + others);
        // Edge (Fast / Swing) → 3 ordinal + 4 family = 7.
        let edge = cell(
            IndicatorFamily::Flow,
            TimescaleBand::Fast,
            HoldingBand::Swing,
        );
        assert_eq!(neighbours(edge).len(), 3 + others);
        // The cross-family neighbours share the cell's timescale/holding.
        assert!(neighbours(centre)
            .iter()
            .filter(|c| c.family != IndicatorFamily::Trend)
            .all(|c| c.timescale == TimescaleBand::Medium && c.holding == HoldingBand::Swing));
    }

    /// Map every single-feature-reachable cell to a representative long-only genome (the fixture grid).
    fn reachable(schema: &FeatureSchema) -> BTreeMap<Cell, Genome> {
        let mut m = BTreeMap::new();
        for f in 0..schema.len() {
            for hold in [3u16, 20, 100] {
                let g = long_genome(f as u16, hold);
                if let Some(c) = descriptor_for(&g, Direction::Long, schema) {
                    m.entry(c).or_insert(g);
                }
            }
        }
        m
    }

    #[test]
    fn crowding_and_weight_are_monotone() {
        let s = schema();
        let reachable = reachable(&s);
        // Insert a small cluster, then check a cell with more occupied neighbours weighs less.
        let mut arc = MapElitesArchive::new(schema());
        for (_, g) in reachable.iter().take(reachable.len().min(12)) {
            arc.insert(g.clone(), 1.0);
        }
        let reg = BehaviouralRegulariser::with_defaults();
        // Find two occupied cells with different local crowding.
        let mut by_crowd: Vec<(usize, Cell)> = arc
            .direction(Direction::Long)
            .occupied_cells()
            .map(|c| (local_crowding(&arc, Direction::Long, c), *c))
            .collect();
        by_crowd.sort_by_key(|(k, _)| *k);
        if let (Some(&(c_lo, cell_lo)), Some(&(c_hi, cell_hi))) =
            (by_crowd.first(), by_crowd.last())
        {
            if c_lo < c_hi {
                assert!(
                    reg.novelty_weight(&arc, Direction::Long, &cell_lo)
                        > reg.novelty_weight(&arc, Direction::Long, &cell_hi),
                    "less-crowded cell must weigh more"
                );
            }
        }
        // pressure = 0 ⇒ uniform weights.
        let uniform = BehaviouralRegulariser::new(0.0);
        let any = arc
            .direction(Direction::Long)
            .occupied_cells()
            .next()
            .copied();
        if let Some(c) = any {
            assert_eq!(uniform.novelty_weight(&arc, Direction::Long, &c), 1.0);
        }
    }

    #[test]
    fn coverage_and_entropy_metrics() {
        let s = schema();
        let mut arc = MapElitesArchive::new(schema());
        assert_eq!(coverage(&arc, Direction::Long), 0);
        assert_eq!(occupancy_entropy(&arc, Direction::Long), 0.0);

        // One occupied cell → entropy 0.
        let reachable = reachable(&s);
        let mut it = reachable.values();
        arc.insert(it.next().unwrap().clone(), 1.0);
        assert_eq!(coverage(&arc, Direction::Long), 1);
        assert_eq!(occupancy_entropy(&arc, Direction::Long), 0.0);

        // Two evenly-occupied cells → entropy ln(2).
        arc.insert(it.next().unwrap().clone(), 1.0);
        assert_eq!(coverage(&arc, Direction::Long), 2);
        assert!((occupancy_entropy(&arc, Direction::Long) - 2.0_f64.ln()).abs() < 1e-9);
    }

    /// A seeded behaviour-space random walk: each step picks a parent cell, then inserts a representative
    /// genome of a random reachable neighbour of it (cell-local variation). Returns final coverage.
    fn walk_coverage(
        reachable: &BTreeMap<Cell, Genome>,
        pressure: f64,
        steps: usize,
        seed: u64,
    ) -> usize {
        let reg = BehaviouralRegulariser::new(pressure);
        let mut arc = MapElitesArchive::new(schema());
        // Seed one cell.
        let start = reachable.iter().next().unwrap();
        arc.insert(start.1.clone(), 1.0);
        let mut rng = task_rng(seed, 0);
        for _ in 0..steps {
            let Some(parent) = reg.select_parent_cell(&arc, Direction::Long, &mut rng) else {
                break;
            };
            // Reachable neighbours of the parent cell.
            let nbrs: Vec<Cell> = neighbours(parent)
                .into_iter()
                .filter(|c| reachable.contains_key(c))
                .collect();
            if nbrs.is_empty() {
                continue;
            }
            let pick = nbrs[(rng.next_u64() % nbrs.len() as u64) as usize];
            arc.insert(reachable[&pick].clone(), 1.0);
        }
        coverage(&arc, Direction::Long)
    }

    #[test]
    fn diversity_improves_versus_ablation() {
        // THE AC. Before the graph saturates, novelty pressure reaches more distinct niches per step
        // (it spends reproduction on the frontier, not on crowded interior cells). Averaged over several
        // seeds to wash out the random-neighbour noise, the novelty-pressure walk covers strictly more
        // cells than the pressure = 0 ablation at a fixed, pre-saturation step budget.
        let s = schema();
        let reachable = reachable(&s);
        let steps = 30;
        let seeds: [u64; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let regularised: usize = seeds
            .iter()
            .map(|&sd| walk_coverage(&reachable, DEFAULT_NOVELTY_PRESSURE, steps, sd))
            .sum();
        let ablation: usize = seeds
            .iter()
            .map(|&sd| walk_coverage(&reachable, 0.0, steps, sd))
            .sum();
        assert!(
            regularised > ablation,
            "novelty pressure should raise coverage (sum over {} seeds): regularised={regularised} ablation={ablation}",
            seeds.len()
        );
    }

    #[test]
    fn parent_cell_selection_is_deterministic() {
        let s = schema();
        let reachable = reachable(&s);
        let mut arc = MapElitesArchive::new(schema());
        for (_, g) in reachable.iter().take(reachable.len().min(10)) {
            arc.insert(g.clone(), 1.0);
        }
        let reg = BehaviouralRegulariser::with_defaults();
        let run = || {
            let mut rng = task_rng(9, 0);
            (0..50)
                .map(|_| reg.select_parent_cell(&arc, Direction::Long, &mut rng))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
