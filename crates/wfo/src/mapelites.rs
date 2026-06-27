//! QD / MAP-Elites archive implementation (QE-118) — stores elites in the QE-111 niche grid, keeping
//! behavioural diversity by construction.
//!
//! Two per-direction archives (Long, Short) each hold a `BTreeMap<Cell, SubPopulation>` over the
//! genotype-derived [`Cell`](crate::archive::Cell)s from QE-111; a genome is placed at the cell its
//! *that-direction* bank describes, so it may occupy both, one, or neither archive. Each cell is a
//! Deep-Grid [`SubPopulation`] bounded to [`SUBPOP_SIZE`] elites (noise robustness: one unlucky
//! evaluation cannot evict a genome). Parent selection ([`MapElitesArchive::sample_parent`]) samples a
//! non-empty cell *uniformly* then an elite within it — sparse niches reproduce as often as crowded
//! ones, which is what preserves diversity.
//!
//! Evaluation ([`evaluate_batch`]) is embarrassingly parallel across cores via `rayon`, yet
//! byte-deterministic regardless of pool size: each task seeds its own [`DetRng`] from
//! `task_rng(master, index)` (QE-006), so a genome's stream depends only on its index, never on which
//! thread runs it. Operator credit assignment is QE-119 (the insert path only *returns* an outcome);
//! persistence / quality gate is QE-123.

use std::collections::BTreeMap;

use qe_determinism::{task_rng, DetRng};
use qe_domain::Direction;
use qe_signal::FeatureSchema;
use rand_core::RngCore;
use rayon::prelude::*;

use crate::archive::{descriptor_for, Cell, SUBPOP_SIZE};
use crate::genome::Genome;

/// A stored elite: a genome and its scalar fitness (a score, not money — `f64`).
#[derive(Debug, Clone, PartialEq)]
pub struct Elite {
    /// The strategy genome (the stored artefact, QE-110).
    pub genome: Genome,
    /// Fitness score (metric-agnostic; the noise-robust metric is QE-113/QE-120).
    pub fitness: f64,
}

/// What happened when a candidate was offered to a cell's sub-population.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// The cell was empty — a genuinely new niche was filled.
    NewCell,
    /// The cell had room (`len < SUBPOP_SIZE`) and the candidate was added.
    Added,
    /// The cell was full and the candidate displaced the worst elite (strictly better).
    ImprovedElite,
    /// The cell was full and the candidate was not better than its worst elite.
    Rejected,
}

/// The per-direction outcomes of inserting one genome (None ⇒ no descriptor in that direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insertion {
    /// Outcome in the Long archive, if the genome's long bank described a cell.
    pub long: Option<InsertOutcome>,
    /// Outcome in the Short archive, if the genome's short bank described a cell.
    pub short: Option<InsertOutcome>,
}

/// A Deep-Grid sub-population: up to [`SUBPOP_SIZE`] elites in a single cell (Flageat & Cully 2020).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubPopulation {
    elites: Vec<Elite>,
}

impl SubPopulation {
    /// The elites currently held (read-only).
    #[must_use]
    pub fn elites(&self) -> &[Elite] {
        &self.elites
    }

    /// Number of elites held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.elites.len()
    }

    /// Whether the cell holds no elites.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elites.is_empty()
    }

    /// Whether the cell is at the Deep-Grid bound.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.elites.len() >= SUBPOP_SIZE
    }

    /// The highest-fitness elite, if any.
    #[must_use]
    pub fn best(&self) -> Option<&Elite> {
        self.elites
            .iter()
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
    }

    /// The lowest-fitness elite, if any (ties broken by lowest index) — the candidate a full cell would
    /// displace. Used by QE-119 to size the displaced-improvement `gain`.
    #[must_use]
    pub fn worst(&self) -> Option<&Elite> {
        self.worst_index().map(|i| &self.elites[i])
    }

    /// Index of the worst (min-fitness) elite, ties broken by lowest index (deterministic).
    fn worst_index(&self) -> Option<usize> {
        self.elites
            .iter()
            .enumerate()
            .min_by(|(ia, a), (ib, b)| a.fitness.total_cmp(&b.fitness).then_with(|| ia.cmp(ib)))
            .map(|(i, _)| i)
    }

    /// Offer a candidate to the cell. Empty ⇒ `NewCell`; room ⇒ `Added`; full ⇒ replace the worst iff
    /// the candidate is *strictly* better (`ImprovedElite`), else `Rejected`.
    pub fn consider(&mut self, candidate: Elite) -> InsertOutcome {
        if self.elites.is_empty() {
            self.elites.push(candidate);
            return InsertOutcome::NewCell;
        }
        if !self.is_full() {
            self.elites.push(candidate);
            return InsertOutcome::Added;
        }
        // Full: displace the worst only if the candidate strictly beats it.
        let worst = self
            .worst_index()
            .expect("non-empty cell has a worst elite");
        if candidate.fitness > self.elites[worst].fitness {
            self.elites[worst] = candidate;
            InsertOutcome::ImprovedElite
        } else {
            InsertOutcome::Rejected
        }
    }
}

/// One direction's MAP-Elites grid: a sparse map from [`Cell`] to its [`SubPopulation`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirectionArchive {
    cells: BTreeMap<Cell, SubPopulation>,
}

impl DirectionArchive {
    /// Offer a candidate at a specific cell.
    fn consider_at(&mut self, cell: Cell, candidate: Elite) -> InsertOutcome {
        self.cells.entry(cell).or_default().consider(candidate)
    }

    /// The occupied cells, in deterministic (sorted) order.
    pub fn occupied_cells(&self) -> impl Iterator<Item = &Cell> {
        self.cells.keys()
    }

    /// Number of occupied cells (filled niches).
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether no cell is occupied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Total elites across all cells.
    #[must_use]
    pub fn total_elites(&self) -> usize {
        self.cells.values().map(SubPopulation::len).sum()
    }

    /// The sub-population at `cell`, if occupied.
    #[must_use]
    pub fn cell(&self, cell: &Cell) -> Option<&SubPopulation> {
        self.cells.get(cell)
    }

    /// Deep-Grid parent sampling: a non-empty cell uniformly, then an elite within it uniformly.
    fn sample_parent(&self, rng: &mut DetRng) -> Option<&Elite> {
        if self.cells.is_empty() {
            return None;
        }
        let cell_idx = (rng.next_u64() % self.cells.len() as u64) as usize;
        let sub = self.cells.values().nth(cell_idx)?;
        if sub.elites.is_empty() {
            return None;
        }
        let elite_idx = (rng.next_u64() % sub.elites.len() as u64) as usize;
        sub.elites.get(elite_idx)
    }
}

/// The QD MAP-Elites archive: per-direction grids over the QE-111 niche substrate.
#[derive(Debug, Clone)]
pub struct MapElitesArchive {
    schema: FeatureSchema,
    long: DirectionArchive,
    short: DirectionArchive,
}

impl MapElitesArchive {
    /// An empty archive over a fixed feature `schema` (the descriptor inputs are genotype + schema).
    #[must_use]
    pub fn new(schema: FeatureSchema) -> Self {
        MapElitesArchive {
            schema,
            long: DirectionArchive::default(),
            short: DirectionArchive::default(),
        }
    }

    /// Read-only access to one direction's archive.
    #[must_use]
    pub fn direction(&self, direction: Direction) -> &DirectionArchive {
        match direction {
            Direction::Long => &self.long,
            Direction::Short => &self.short,
        }
    }

    /// Insert a `(genome, fitness)` into whichever direction archives its banks describe. Returns the
    /// per-direction [`InsertOutcome`]s (None where the bank described no cell).
    pub fn insert(&mut self, genome: Genome, fitness: f64) -> Insertion {
        let long_cell = descriptor_for(&genome, Direction::Long, &self.schema);
        let short_cell = descriptor_for(&genome, Direction::Short, &self.schema);
        let long = long_cell.map(|cell| {
            self.long.consider_at(
                cell,
                Elite {
                    genome: genome.clone(),
                    fitness,
                },
            )
        });
        let short = short_cell.map(|cell| self.short.consider_at(cell, Elite { genome, fitness }));
        Insertion { long, short }
    }

    /// Sample a parent genome from one direction's archive (Deep-Grid niche sampling). Deterministic for
    /// a given `rng` state; `None` if that direction's archive is empty.
    #[must_use]
    pub fn sample_parent(&self, direction: Direction, rng: &mut DetRng) -> Option<&Genome> {
        self.direction(direction)
            .sample_parent(rng)
            .map(|e| &e.genome)
    }

    /// Like [`sample_parent`](Self::sample_parent) but returns the whole [`Elite`] (genome + fitness) —
    /// QE-119's variation driver needs the parent's fitness to size the displaced-improvement `gain`.
    #[must_use]
    pub fn sample_parent_elite(&self, direction: Direction, rng: &mut DetRng) -> Option<&Elite> {
        self.direction(direction).sample_parent(rng)
    }

    /// Total occupied cells across both directions.
    #[must_use]
    pub fn occupied_cells(&self) -> usize {
        self.long.len() + self.short.len()
    }

    /// Total elites across both directions.
    #[must_use]
    pub fn total_elites(&self) -> usize {
        self.long.total_elites() + self.short.total_elites()
    }
}

/// Evaluate a batch of genomes across cores, **byte-deterministically regardless of pool size**.
///
/// Each genome is scored on its own `DetRng` seeded from `task_rng(master_seed, index)` (QE-006), so its
/// random stream depends only on its index — never on which thread runs it. `rayon`'s ordered `collect`
/// returns an index-aligned `Vec<f64>`. Two runs with the same `master_seed` are identical; the result
/// does not change with the number of threads.
pub fn evaluate_batch<F>(master_seed: u64, genomes: &[Genome], eval: F) -> Vec<f64>
where
    F: Fn(&Genome, &mut DetRng) -> f64 + Sync,
{
    genomes
        .par_iter()
        .enumerate()
        .map(|(index, genome)| {
            let mut rng = task_rng(master_seed, index as u64);
            eval(genome, &mut rng)
        })
        .collect()
}

/// Evaluate a batch in parallel ([`evaluate_batch`]) then insert each `(genome, fitness)` sequentially
/// (insertion is cheap and order-deterministic), returning the per-genome [`Insertion`]s.
pub fn evaluate_and_insert<F>(
    archive: &mut MapElitesArchive,
    master_seed: u64,
    genomes: Vec<Genome>,
    eval: F,
) -> Vec<Insertion>
where
    F: Fn(&Genome, &mut DetRng) -> f64 + Sync,
{
    let fitnesses = evaluate_batch(master_seed, &genomes, eval);
    genomes
        .into_iter()
        .zip(fitnesses)
        .map(|(genome, fitness)| archive.insert(genome, fitness))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::IndicatorFamily;
    use crate::genome::{Clause, ExitParams, RiskParams, RuleSet, REP_VERSION};
    use qe_signal::CatalogueConfig;

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    fn idx_of(schema: &FeatureSchema, id: &str) -> u16 {
        schema
            .ids()
            .iter()
            .position(|s| s == id)
            .map(|p| p as u16)
            .unwrap_or_else(|| panic!("indicator {id} not in catalogue"))
    }

    fn clause(enabled: bool, feature: u16) -> Clause {
        Clause {
            enabled,
            feature,
            lo: 0,
            hi: 1,
        }
    }

    fn genome_with(long_feats: &[u16], short_feats: &[u16], max_holding_bars: u16) -> Genome {
        let bank = |feats: &[u16]| {
            let mut clauses = [
                clause(false, 0),
                clause(false, 0),
                clause(false, 0),
                clause(false, 0),
            ];
            for (slot, &f) in clauses.iter_mut().zip(feats.iter()) {
                *slot = clause(true, f);
            }
            RuleSet {
                clauses,
                min_satisfied: 1,
            }
        };
        Genome {
            version: REP_VERSION,
            long_entry: bank(long_feats),
            short_entry: bank(short_feats),
            exit: ExitParams {
                max_holding_bars,
                exit_on_opposite: true,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    #[test]
    fn fills_distinct_niches_across_directions() {
        let s = schema();
        let mut arc = MapElitesArchive::new(schema());
        // Three genomes whose long banks land in three different cells.
        let g_trend = genome_with(
            &[idx_of(&s, "ema_ratio_20")],
            &[idx_of(&s, "funding_state")],
            3,
        );
        let g_mom = genome_with(&[idx_of(&s, "rsi_14")], &[idx_of(&s, "cmf_20")], 30);
        let g_vol = genome_with(&[idx_of(&s, "atr_pct_14")], &[idx_of(&s, "oi_roc_10")], 60);

        arc.insert(g_trend, 1.0);
        arc.insert(g_mom, 1.0);
        arc.insert(g_vol, 1.0);

        // Three distinct long niches filled.
        assert_eq!(arc.direction(Direction::Long).len(), 3);
        let fams: Vec<IndicatorFamily> = arc
            .direction(Direction::Long)
            .occupied_cells()
            .map(|c| c.family)
            .collect();
        assert!(fams.contains(&IndicatorFamily::Trend));
        assert!(fams.contains(&IndicatorFamily::Momentum));
        assert!(fams.contains(&IndicatorFamily::Volatility));
        // Short banks also placed (Flow / Volume / Flow) — short niches are first-class.
        assert_eq!(arc.direction(Direction::Short).total_elites(), 3);
    }

    #[test]
    fn subpopulation_is_bounded_and_keeps_the_fittest() {
        let s = schema();
        let mut arc = MapElitesArchive::new(schema());
        // All these share a long cell (Momentum / Fast / Scalp via rsi_14 lb15... actually lb15→Medium).
        // Use the same feature + holding so they collide in one cell.
        let feat = idx_of(&s, "rsi_14");
        let cell_genome = |id: u16| genome_with(&[id], &[], 3);
        // Insert SUBPOP_SIZE + 4 genomes with increasing fitness into the same cell.
        for i in 0..(SUBPOP_SIZE + 4) {
            let outcome = arc.insert(cell_genome(feat), i as f64);
            match i {
                0 => assert_eq!(outcome.long, Some(InsertOutcome::NewCell)),
                x if x < SUBPOP_SIZE => assert_eq!(outcome.long, Some(InsertOutcome::Added)),
                _ => assert_eq!(outcome.long, Some(InsertOutcome::ImprovedElite)),
            }
        }
        let long = arc.direction(Direction::Long);
        assert_eq!(long.len(), 1, "all collided into one cell");
        let sub = long
            .occupied_cells()
            .next()
            .and_then(|c| long.cell(c))
            .unwrap();
        // Bounded.
        assert_eq!(sub.len(), SUBPOP_SIZE);
        // Kept the top SUBPOP_SIZE fitnesses: the highest is the last inserted (SUBPOP_SIZE+3).
        assert_eq!(sub.best().unwrap().fitness, (SUBPOP_SIZE + 3) as f64);
        // The worst retained is at least the (count - SUBPOP_SIZE)-th smallest = 4.0.
        let min = sub
            .elites()
            .iter()
            .map(|e| e.fitness)
            .fold(f64::INFINITY, f64::min);
        assert_eq!(min, 4.0);
    }

    #[test]
    fn rejects_candidate_not_better_than_worst_of_full_cell() {
        let s = schema();
        let mut arc = MapElitesArchive::new(schema());
        let feat = idx_of(&s, "rsi_14");
        for _ in 0..SUBPOP_SIZE {
            arc.insert(genome_with(&[feat], &[], 3), 5.0); // fill with fitness 5
        }
        // Worse candidate is rejected; strictly-better is accepted.
        assert_eq!(
            arc.insert(genome_with(&[feat], &[], 3), 4.9).long,
            Some(InsertOutcome::Rejected)
        );
        assert_eq!(
            arc.insert(genome_with(&[feat], &[], 3), 5.0).long,
            Some(InsertOutcome::Rejected),
            "equal fitness does not displace (strict-better)"
        );
        assert_eq!(
            arc.insert(genome_with(&[feat], &[], 3), 5.1).long,
            Some(InsertOutcome::ImprovedElite)
        );
    }

    #[test]
    fn genome_with_no_descriptor_is_stored_in_neither_direction() {
        let mut arc = MapElitesArchive::new(schema());
        // All clauses disabled → no descriptor in either direction.
        let g = genome_with(&[], &[], 3);
        let ins = arc.insert(g, 1.0);
        assert_eq!(ins.long, None);
        assert_eq!(ins.short, None);
        assert_eq!(arc.total_elites(), 0);
    }

    #[test]
    fn niche_sampling_is_deterministic_and_reaches_sparse_cells() {
        let s = schema();
        let mut arc = MapElitesArchive::new(schema());
        // A crowded Momentum cell (many elites) and a sparse Trend cell (one elite).
        let mom = idx_of(&s, "rsi_14");
        for i in 0..SUBPOP_SIZE {
            arc.insert(genome_with(&[mom], &[], 3), i as f64);
        }
        arc.insert(genome_with(&[idx_of(&s, "ema_ratio_20")], &[], 3), 100.0);

        // Deterministic: same seed → same parent.
        let mut r1 = task_rng(42, 0);
        let mut r2 = task_rng(42, 0);
        assert_eq!(
            arc.sample_parent(Direction::Long, &mut r1),
            arc.sample_parent(Direction::Long, &mut r2)
        );

        // Over many draws with uniform-cell sampling, the sparse Trend cell is reached (≈ half the time,
        // since there are two cells) — diversity preserved despite the crowded cell.
        let mut rng = task_rng(7, 0);
        let mut trend_hits = 0;
        for _ in 0..200 {
            let g = arc.sample_parent(Direction::Long, &mut rng).unwrap();
            if descriptor_for(g, Direction::Long, &s).unwrap().family == IndicatorFamily::Trend {
                trend_hits += 1;
            }
        }
        assert!(
            trend_hits > 40,
            "uniform-cell sampling should reach the sparse cell often, got {trend_hits}/200"
        );

        // Empty archive → None.
        let empty = MapElitesArchive::new(schema());
        let mut r = task_rng(1, 0);
        assert!(empty.sample_parent(Direction::Long, &mut r).is_none());
    }

    // --- the determinism AC -----------------------------------------------------------------------

    /// A deterministic eval that *consumes the rng* so a scheduling-dependent (shared-rng) implementation
    /// would produce different results — this is what makes the parallel-determinism test meaningful.
    fn rng_eval(g: &Genome, rng: &mut DetRng) -> f64 {
        let r = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64; // in [0,1)
        g.risk.size_bps as f64 + r
    }

    fn batch(s: &FeatureSchema, n: usize) -> Vec<Genome> {
        (0..n)
            .map(|i| genome_with(&[idx_of(s, "rsi_14")], &[], (i % 60 + 1) as u16))
            .collect()
    }

    #[test]
    fn parallel_evaluation_is_deterministic_regardless_of_thread_count() {
        let s = schema();
        let genomes = batch(&s, 64);

        let pool1 = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let pool8 = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();

        let single = pool1.install(|| evaluate_batch(2024, &genomes, rng_eval));
        let many = pool8.install(|| evaluate_batch(2024, &genomes, rng_eval));

        // Identical regardless of pool size — the determinism AC.
        assert_eq!(single, many);
        // Same seed twice → identical; different seed → differs somewhere.
        assert_eq!(single, evaluate_batch(2024, &genomes, rng_eval));
        assert_ne!(single, evaluate_batch(2025, &genomes, rng_eval));
        // Index-aligned with the input.
        assert_eq!(single.len(), genomes.len());
    }

    #[test]
    fn evaluate_and_insert_is_deterministic() {
        let s = schema();
        let genomes = batch(&s, 32);
        let mut a1 = MapElitesArchive::new(schema());
        let mut a2 = MapElitesArchive::new(schema());
        let pool1 = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let pool4 = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        pool1.install(|| evaluate_and_insert(&mut a1, 99, genomes.clone(), rng_eval));
        pool4.install(|| evaluate_and_insert(&mut a2, 99, genomes.clone(), rng_eval));
        // Same archive content regardless of pool size.
        assert_eq!(a1.total_elites(), a2.total_elites());
        assert_eq!(a1.occupied_cells(), a2.occupied_cells());
        assert_eq!(a1.long, a2.long);
        assert_eq!(a1.short, a2.short);
    }
}
