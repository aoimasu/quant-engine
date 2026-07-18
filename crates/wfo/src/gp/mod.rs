//! QE-451 **Phase 1a** — offline GP `Expr`-tree MAP-Elites pool illumination (QE-450 §9 Phase 1a row).
//!
//! A **default-off, opt-in** offline stage: it illuminates a separate [`ExprArchive`] of FIR indicator
//! trees under a **trivial fixed decision head** (threshold-cross, §7 of the design note) — *not* a
//! pooled backtest (that is the screened-survivors path in Phase 1b). Nothing in the default
//! `train`/`backtest`/`catalogue` pipeline reaches here, so the production catalogue/vintage is
//! unchanged and no golden moves.
//!
//! Delivered here (Phase 1a **only**): the completed grammar's normalising roots (in `qe-signal`),
//! [`ExprTree::repair`], tree-aware [`variation`] operators on [`DetRng`], the
//! [`Elite<ExprTree>`](archive::ExprElite) [`archive`] with structural descriptors + behavioural dedup,
//! and the **distinct-canonical trial count** emitted into a dedicated [`PoolLineage`] record (the input
//! QE-439's deflation basis will consume in Phase 1b).
//!
//! **Out of scope (Phase 1b):** DSR/PBO deflation, IC pre-screen, MDL rent, cost/turnover/capacity
//! gates, cross-asset pooled fitness, freezing `K ≤ 16` into `CatalogueIdentity`, flow terminals.

pub mod archive;
pub mod deflation;
pub mod descriptor;
pub mod freeze;
pub mod gates;
pub mod variation;

use std::collections::HashSet;

use qe_determinism::{task_rng, Lineage};
use qe_signal::indicator::expr::{eval_stream, ExprTree, WinOp};
use qe_signal::indicator::{Quantiser, Sample};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::operator::{ApplicationOutcome, Operator, OperatorSelector};

pub use archive::{quantised_correlation, ExprArchive, ExprElite, ExprInsert, DEDUP_THRESHOLD};
pub use deflation::{
    assess_gp_champion, calibrate_null_basis, formula_returns, gp_trial_basis,
    market_forward_returns, mdl_penalised_fitness, mean_cross_asset_correlation, pooled_t_eff,
    signal_series, uncensored_pbo, GpDeflationGate, GpDeflationReport,
};
pub use descriptor::{
    descriptor_for_tree, family_of_tree, grid_cells, ComplexityBand, ExprCell, COMPLEXITIES,
    EXPR_CELLS,
};
pub use freeze::{FreezeError, FrozenFormula, FrozenPool, MAX_POOL_SIZE};
pub use gates::{
    cost_stressed_net, evaluate_tradability, ic_screen_trees, inlined_capacity, turnover_frac,
    TradabilityConfig, TradabilityVerdict, CAPACITY_FLOOR, MAX_TURNOVER_FRAC,
};
pub use variation::{explore, fresh_random, local_refine};

/// Parameters for one offline illumination campaign (Phase 1a).
#[derive(Debug, Clone, Copy)]
pub struct IlluminationParams {
    /// Master RNG seed — every per-offspring stream derives from `task_rng(master_seed, index)`.
    pub master_seed: u64,
    /// Number of generations.
    pub generations: usize,
    /// Offspring evaluated per generation.
    pub offspring_per_generation: usize,
    /// Quantiser state count for the trivial decision head (≥ 2).
    pub states: u16,
}

impl Default for IlluminationParams {
    fn default() -> Self {
        IlluminationParams {
            master_seed: 0,
            generations: 40,
            offspring_per_generation: 32,
            states: 5,
        }
    }
}

/// The **GP-pool lineage record** (Phase 1a): a reproducible [`Lineage`] plus the **distinct-canonical
/// trial count**. The production [`Lineage`] struct is *not* modified (that would move every vintage id);
/// the count rides this dedicated Phase-1a record that *contains* a `Lineage`. Phase 1b's deflation
/// consumes `distinct_evaluations` as QE-439's trial basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolLineage {
    /// The reproducible lineage (config hash + input snapshot + code commit + seeds) of the campaign.
    pub lineage: Lineage,
    /// Distinct **canonical** formulas evaluated — canonicalised (constant-folded / order-normalised /
    /// rank-monotone-collapsed) then content-hashed, counting **every** evaluated tree including
    /// dedup-/replacement-rejected offspring. This is QE-439's deflation basis (Phase 1b).
    pub distinct_evaluations: u64,
    /// Total offspring evaluated (raw count, `≥ distinct_evaluations`).
    pub total_evaluations: u64,
}

/// The result of an illumination campaign: the illuminated archive plus its GP-pool lineage.
#[derive(Debug, Clone)]
pub struct IlluminationReport {
    /// The illuminated `Elite<ExprTree>` archive.
    pub archive: ExprArchive,
    /// The GP-pool lineage record carrying the distinct-canonical trial count.
    pub pool_lineage: PoolLineage,
}

impl IlluminationReport {
    /// Distinct-canonical trial count (QE-439 basis).
    #[must_use]
    pub fn distinct_evaluations(&self) -> u64 {
        self.pool_lineage.distinct_evaluations
    }
    /// Total offspring evaluated.
    #[must_use]
    pub fn total_evaluations(&self) -> u64 {
        self.pool_lineage.total_evaluations
    }
    /// Occupied niches in the archive.
    #[must_use]
    pub fn occupied_cells(&self) -> usize {
        self.archive.len()
    }
    /// Total elites illuminated.
    #[must_use]
    pub fn total_elites(&self) -> usize {
        self.archive.total_elites()
    }
}

/// The point-wise quantiser the trivial head uses for a normalising root (§4.4): `Rank`→`Linear{0,1}`,
/// `Zscore`→symmetric `Bands`. The **existing** stateless quantiser, unchanged.
#[must_use]
pub fn quantiser_for_root(root_op: WinOp, states: u16) -> Quantiser {
    let states = states.max(2);
    match root_op {
        WinOp::Zscore => {
            // Symmetric interior edges in (−2, 2): `−2 + i·(4/states)` for `i ∈ 1..states`.
            let edges: Vec<Decimal> = (1..states)
                .map(|i| {
                    Decimal::from(-2) + Decimal::from(4) * Decimal::from(i) / Decimal::from(states)
                })
                .collect();
            Quantiser::Bands { edges }
        }
        _ => Quantiser::Linear {
            min: Decimal::ZERO,
            max: Decimal::ONE,
            states,
        },
    }
}

/// Evaluate a tree under the **trivial threshold-cross head** (§7). Returns `(fitness, series)`:
/// `fitness = mean_t( signal_t · r_{t+1} )` (an IC-like scalar; `signal = +1` if `state ≥ mid` else
/// `−1`), and the quantised state series (`None` while warming). Deterministic; exact-`Decimal`
/// arithmetic — the `f64` fitness only orders elites, never feeds a hash.
#[must_use]
pub fn eval_tree(tree: &ExprTree, samples: &[Sample], states: u16) -> (f64, Vec<Option<i64>>) {
    let root_op = tree.root_op().unwrap_or(WinOp::Rank);
    let q = quantiser_for_root(root_op, states);
    let raw = eval_stream(tree.root(), samples);
    let series: Vec<Option<i64>> = raw
        .iter()
        .map(|o| o.map(|v| i64::from(q.quantise(v).index())))
        .collect();

    let mid = i64::from(states.max(2) / 2);
    let mut sum = Decimal::ZERO;
    let mut n: u64 = 0;
    for t in 0..samples.len().saturating_sub(1) {
        if let Some(state) = series[t] {
            let c0 = samples[t].bar.close().get();
            let c1 = samples[t + 1].bar.close().get();
            if !c0.is_zero() {
                let r = c1 / c0 - Decimal::ONE;
                let signal = if state >= mid {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                sum += signal * r;
                n += 1;
            }
        }
    }
    let fitness = if n > 0 {
        (sum / Decimal::from(n)).to_f64().unwrap_or(0.0)
    } else {
        0.0
    };
    (fitness, series)
}

fn credit_of(outcome: ExprInsert) -> ApplicationOutcome {
    match outcome {
        ExprInsert::NewCell => ApplicationOutcome::NewCell,
        ExprInsert::ImprovedElite => ApplicationOutcome::ImprovedElite { gain: 1.0 },
        ExprInsert::Added | ExprInsert::Rejected | ExprInsert::DedupRejected => {
            ApplicationOutcome::NoImprovement
        }
    }
}

/// Run one offline illumination campaign (Phase 1a). Deterministic: same `params.master_seed` + same
/// `samples` ⇒ byte-identical archive and counts (each offspring's stream is `task_rng(master, index)`,
/// thread-count-independent). `base_lineage` is the reproducible provenance of the campaign; the returned
/// [`PoolLineage`] augments it with the distinct-canonical trial count.
#[must_use]
pub fn illuminate(
    params: IlluminationParams,
    samples: &[Sample],
    base_lineage: Lineage,
) -> IlluminationReport {
    let mut archive = ExprArchive::new();
    let mut selector = OperatorSelector::with_defaults();
    let mut distinct: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    let mut index: u64 = 0;

    for _gen in 0..params.generations {
        for _ in 0..params.offspring_per_generation {
            let mut rng = task_rng(params.master_seed, index);
            index += 1;

            let op = selector.select(&mut rng);
            let offspring = match op {
                Operator::LocalRefine => match archive.sample_parent(&mut rng).cloned() {
                    Some(p) => local_refine(&p.tree, &mut rng),
                    None => fresh_random(&mut rng),
                },
                Operator::Explore => {
                    let parent = archive.sample_parent(&mut rng).cloned();
                    let other = archive.sample_parent(&mut rng).cloned();
                    match parent {
                        Some(p) => explore(&p.tree, other.as_ref().map(|e| &e.tree), &mut rng),
                        None => fresh_random(&mut rng),
                    }
                }
                Operator::FreshRandom => fresh_random(&mut rng),
            };

            let (fitness, series) = eval_tree(&offspring, samples, params.states);
            let hash = offspring.canonical_hash();
            distinct.insert(hash.clone());
            total += 1;

            let outcome = archive.insert(ExprElite {
                tree: offspring,
                fitness,
                hash,
                series,
            });
            selector.record(op, &credit_of(outcome));
        }
    }

    IlluminationReport {
        archive,
        pool_lineage: PoolLineage {
            lineage: base_lineage,
            distinct_evaluations: distinct.len() as u64,
            total_evaluations: total,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
    use qe_signal::indicator::expr::{Expr, Field};

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }
    fn boxed(e: Expr) -> Box<Expr> {
        Box::new(e)
    }

    /// A deterministic, gently-trending-then-oscillating price series (enough structure that the trivial
    /// head separates trees, and long enough to warm up to `lookback = 100`).
    fn series(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                let base = 100 + (i64i % 11) * 4 - (i64i % 5) * 3 + i64i / 3;
                let high = base + 6 + (i64i % 4);
                let low = base - 6 - (i64i % 3);
                let close = base + (i64i % 7) - 3;
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 5 * MIN),
                    Resolution::M5,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(high.max(1))).unwrap(),
                    Price::new(dec(low.max(1))).unwrap(),
                    Price::new(dec(close.max(1))).unwrap(),
                    Qty::new(dec(10 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample::from_bar(bar)
            })
            .collect()
    }

    fn lineage() -> Lineage {
        Lineage::new("cfg", "snap", "commit", vec![7])
    }

    fn small_params(seed: u64) -> IlluminationParams {
        IlluminationParams {
            master_seed: seed,
            generations: 8,
            offspring_per_generation: 24,
            states: 5,
        }
    }

    #[test]
    fn illumination_fills_the_archive_and_counts_trials() {
        let samples = series(200);
        let report = illuminate(small_params(2026), &samples, lineage());
        assert!(
            report.occupied_cells() > 1,
            "should illuminate several niches"
        );
        assert!(report.total_elites() >= report.occupied_cells());
        // Every evaluated tree is counted; distinct ≤ total.
        assert_eq!(report.total_evaluations(), 8 * 24);
        assert!(report.distinct_evaluations() >= 1);
        assert!(report.distinct_evaluations() <= report.total_evaluations());
    }

    #[test]
    fn a_dedup_rejected_offspring_still_counts_toward_the_trial_basis() {
        // Isolate ONE reject (design §5 "rejects all count toward N"). `illuminate` runs
        // `distinct.insert(hash)` + `total += 1` for EVERY offspring BEFORE the archive decides
        // accept/reject, so a dedup-rejected offspring is still counted. Here two offspring share the same
        // canonical form AND behavioural series: the first fills the cell, the second is DedupRejected —
        // total = 2 (both counted), distinct = 1 (same canonical hash), and only one enters the archive.
        let mut archive = ExprArchive::new();
        let mut distinct: HashSet<String> = HashSet::new();
        let mut total: u64 = 0;

        let tree = ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                20,
            )),
            50,
        ));
        let series: Vec<Option<i64>> = (0..40).map(|i| Some((i % 5) as i64)).collect();

        // Offer 1 — counted, then accepted into a fresh cell.
        let hash1 = tree.canonical_hash();
        distinct.insert(hash1.clone());
        total += 1;
        let out1 = archive.insert(ExprElite {
            tree: tree.clone(),
            fitness: 1.0,
            hash: hash1,
            series: series.clone(),
        });
        assert_eq!(out1, ExprInsert::NewCell);

        // Offer 2 — counted BEFORE the archive rejects it as a behavioural duplicate in the same cell.
        let hash2 = tree.canonical_hash();
        distinct.insert(hash2.clone());
        total += 1;
        let out2 = archive.insert(ExprElite {
            tree: tree.clone(),
            fitness: 2.0,
            hash: hash2,
            series: series.clone(),
        });

        assert_eq!(
            out2,
            ExprInsert::DedupRejected,
            "identical behavioural series in the same cell ⇒ dedup reject"
        );
        assert_eq!(
            total, 2,
            "the single dedup-rejected offspring still increments the total trial count"
        );
        assert_eq!(
            distinct.len(),
            1,
            "identical canonical form ⇒ distinct stays 1 (both offers counted toward N)"
        );
        assert_eq!(
            archive.total_elites(),
            1,
            "the rejected offspring did NOT enter the archive (it filters, it does not un-count)"
        );
    }

    #[test]
    fn same_seed_reproduces_the_archive_byte_for_byte() {
        let samples = series(200);
        let a = illuminate(small_params(99), &samples, lineage());
        let b = illuminate(small_params(99), &samples, lineage());
        // Archives are structurally identical.
        assert_eq!(a.archive, b.archive);
        assert_eq!(a.distinct_evaluations(), b.distinct_evaluations());
        assert_eq!(a.pool_lineage, b.pool_lineage);
        // A different seed diverges somewhere (archive content differs).
        let c = illuminate(small_params(100), &samples, lineage());
        assert_ne!(a.archive, c.archive);
    }

    #[test]
    fn eval_tree_is_deterministic_and_bounded() {
        let samples = series(120);
        let tree = ExprTree::repaired(Expr::Window(
            WinOp::Rank,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                20,
            )),
            50,
        ));
        let (f1, s1) = eval_tree(&tree, &samples, 5);
        let (f2, s2) = eval_tree(&tree, &samples, 5);
        assert_eq!(f1, f2);
        assert_eq!(s1, s2);
        // Rank root → states in 0..5.
        for st in s1.iter().flatten() {
            assert!((0..5).contains(st));
        }
    }

    #[test]
    fn golden_mutation_stream_is_pinned() {
        // Pins the tree-mutation RNG stream: the first canonical hashes of a fresh_random stream, plus a
        // canonical eval vector of a fixed tree over a fixed series. A DetRng / grammar / canonicaliser
        // change breaks this deliberately (mirrors the rng.rs golden-stream test).
        let mut rng = task_rng(20_260_718, 0);
        let hashes: Vec<String> = (0..4)
            .map(|_| fresh_random(&mut rng).canonical_hash())
            .collect();
        assert_eq!(
            hashes,
            vec![
                GOLDEN_HASH_0.to_string(),
                GOLDEN_HASH_1.to_string(),
                GOLDEN_HASH_2.to_string(),
                GOLDEN_HASH_3.to_string(),
            ]
        );

        let samples = series(60);
        let tree = ExprTree::repaired(Expr::Window(
            WinOp::Zscore,
            boxed(Expr::Window(
                WinOp::Mean,
                boxed(Expr::Input(Field::Close)),
                10,
            )),
            20,
        ));
        let (_f, series) = eval_tree(&tree, &samples, 5);
        let warm: Vec<i64> = series.iter().flatten().copied().collect();
        assert_eq!(warm, GOLDEN_EVAL_VECTOR);
    }

    // Golden constants (pinned after the first green run — see golden_mutation_stream_is_pinned).
    const GOLDEN_HASH_0: &str = "1651ec71ff9edf754f17102a2dd723d9e7b173ed25c266352fd80708d54c7b08";
    const GOLDEN_HASH_1: &str = "74a345a541c8ed26a4b0e836027ff1cfa410145f98d14c8d8d61c86fc969138f";
    const GOLDEN_HASH_2: &str = "dc784df3c12d25d3ff0c4851cd2d3b883a5f46d12aafc139e9fde4d92cc999a7";
    const GOLDEN_HASH_3: &str = "c1233c492a7544e7d7747f2c440ea6fe12d23af39d6a5790c9fa1c9f2daf6c51";
    const GOLDEN_EVAL_VECTOR: &[i64] = &[
        3, 3, 3, 3, 4, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 3, 3, 3, 3, 3, 4, 4, 4, 4,
        3, 3,
    ];
}
