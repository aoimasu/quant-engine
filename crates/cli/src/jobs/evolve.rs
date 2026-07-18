//! The offline GP **evolve** job (QE-452 Phase A): illuminate an `Expr`-tree MAP-Elites pool, screen the
//! survivors through the merged QE-451 deflation, freeze `K ≤ 16` formulas, and **seal a formula-pool
//! artefact** — mirroring the QE-260 train job, but producing a [`qe_formula_pool::FormulaPool`] under a
//! **separate pool root**, **never a vintage** (§13.3). Deterministic for a fixed seed (the illumination
//! rides the merged QE-451 `DetRng`/`task_rng` seeding), single-threaded, no wall-clock/RNG in the sealed
//! output.
//!
//! Pipeline: open store + scan OHLCV (`load`) → `illuminate` the `Elite<ExprTree>` archive (`search`) →
//! collect the cell champions + `assess_gp_champion` deflation over the uncensored population (`deflate`)
//! → `FrozenPool::freeze` the top-`K` distinct trees + seal a [`FormulaPool`] under the pool root (`seal`).
//! This job **constructs no `VintageRepository`** and writes nothing under the vintage root.

use std::path::PathBuf;
use std::str::FromStr;

use qe_determinism::Lineage;
use qe_domain::{InstrumentId, Resolution, Timestamp};
use qe_formula_pool::{
    DeflationSummary, FormulaPool, FormulaPoolContent, FormulaPoolRepository, PoolFormula,
    PoolLineage, PoolMode, MAX_POOL_SIZE, POOL_FORMAT_VERSION,
};
use qe_run_protocol::{ArchiveCell, ArchiveTrialBasis, EvolveArchive, EvolveMode};
use qe_signal::indicator::expr::ExprTree;
use qe_signal::indicator::Sample;
use qe_wfo::gp::{
    assess_gp_champion, formula_returns, illuminate, FrozenPool, IlluminationParams, EXPR_CELLS,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use super::datetime::parse_ymd_to_millis;
use super::{ProgressLine, RunError};

/// Even CSCV block count for the small-budget uncensored-PBO estimate (minimum meaningful value).
const CSCV_BLOCKS: usize = 2;
/// Single-asset / single-window basis for the analytic trial floor in Phase A (cross-asset pooling and
/// multi-window folds are later-phase engine work; the honest distinct-canonical count still drives `N`).
const EVOLVE_WINDOWS: usize = 1;
/// Decimal places every hashed deflation stat is rounded to before conversion to `Decimal` — bounded,
/// round-trip-stable precision (far finer than any Sharpe-scale stat needs).
const STAT_SCALE_DP: u32 = 12;

/// Everything [`run_evolve_job`] needs. Built by `lib` from a parsed `Command::Evolve` (+ the store/pool
/// roots + the config-derived [`Lineage`]) and directly in tests (pointing at the committed fixtures).
#[derive(Debug, Clone)]
pub struct EvolveParams {
    /// Path to the LMDB `MarketStore` the illumination samples are scanned from.
    pub store_path: PathBuf,
    /// LMDB map size to open the store with.
    pub map_size: usize,
    /// Root of the **formula-pool** repository the sealed pool is written into — a directory **separate
    /// from the vintage root** (sandbox → `<artifacts>/research/pools`, production → `<artifacts>/pools`).
    pub pool_root: PathBuf,
    /// The instrument to illuminate over (the first configured universe symbol).
    pub instrument: String,
    /// Inclusive window start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive window end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution (`1h`, …).
    pub resolution: String,
    /// The campaign mode (sandbox / production).
    pub mode: EvolveMode,
    /// The master illumination seed (**required**; deterministic).
    pub seed: u64,
    /// Illumination generations.
    pub generations: usize,
    /// Offspring evaluated per generation.
    pub offspring: usize,
    /// Quantiser state count for the trivial decision head.
    pub states: u16,
    /// Frozen-pool size `K` (clamped to [`MAX_POOL_SIZE`]).
    pub k: usize,
    /// The config-derived lineage (config hash + snapshot + code commit + seed). Its [`Lineage::id`] is
    /// the pool id — deterministic and independent of the stochastic search order.
    pub lineage: Lineage,
    /// The operating profile label (recorded in the result sidecar).
    pub profile: String,
}

/// The window recorded in the result sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct EvolveWindow {
    /// Inclusive start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution.
    pub resolution: String,
}

/// The `result.json` sidecar the evolve job produces. Deterministic for a fixed seed.
#[derive(Debug, Clone, Serialize)]
pub struct EvolveResultDoc {
    /// The sealed pool id (64-hex lineage id).
    pub pool_id: String,
    /// The pool content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The single content address over the sorted formula hashes.
    pub pool_hash: String,
    /// The campaign mode (`sandbox` / `production`).
    pub mode: String,
    /// The operating profile label.
    pub profile: String,
    /// The instrument illuminated over.
    pub instrument: String,
    /// The window.
    pub window: EvolveWindow,
    /// The master seed.
    pub seed: u64,
    /// Illumination generations run.
    pub generations: usize,
    /// Offspring per generation.
    pub offspring: usize,
    /// Occupied niches in the archive.
    pub occupied_cells: usize,
    /// Distinct-canonical formulas evaluated (QE-439 basis).
    pub distinct_evaluations: u64,
    /// The number of frozen formulas (`K ≤ 16`).
    pub pool_size: usize,
}

/// The outcome of an evolve run: the sealed pool + the result document.
#[derive(Debug, Clone)]
pub struct EvolveOutcome {
    /// The sealed pool id.
    pub pool_id: String,
    /// Where the sealed pool was written (`<pool_root>/<pool_id>.json`).
    pub pool_path: PathBuf,
    /// The pool content hash.
    pub content_hash: String,
    /// The result sidecar (written to `<run-dir>/result.json` by `main`).
    pub result: EvolveResultDoc,
    /// The MAP-Elites archive snapshot (written to `<run-dir>/archive.json` by `main`) — the heatmap +
    /// trial-basis the `GET /api/runs/{id}/archive` route (QE-452 Phase B) serves to the QE-453 SPA.
    /// Deterministic for a fixed seed; not a hashed artefact.
    pub archive: EvolveArchive,
}

/// Run the evolve pipeline, streaming structured [`ProgressLine`]s through `emit`.
///
/// # Errors
/// [`RunError`] on invalid inputs, a storage failure, an empty scan window, an empty archive (no elites),
/// or a pool-seal failure.
pub fn run_evolve_job(
    params: &EvolveParams,
    emit: &mut dyn FnMut(ProgressLine),
) -> Result<EvolveOutcome, RunError> {
    // ---- load ------------------------------------------------------------------------------------
    progress(emit, 10, "load", "opening store and scanning bars");
    let resolution = Resolution::from_str(&params.resolution)
        .map_err(|_| RunError::BadResolution(params.resolution.clone()))?;
    let instrument =
        InstrumentId::new(&params.instrument).map_err(|source| RunError::Instrument {
            symbol: params.instrument.clone(),
            source,
        })?;
    let from = Timestamp::from_millis(
        parse_ymd_to_millis(&params.start)
            .ok_or_else(|| RunError::BadDate(params.start.clone()))?,
    );
    let to = Timestamp::from_millis(
        parse_ymd_to_millis(&params.end).ok_or_else(|| RunError::BadDate(params.end.clone()))?,
    );

    let store = qe_storage::MarketStore::open(&params.store_path, params.map_size)?;
    let bars = store.scan_bars(&instrument, resolution, from, to)?;
    if bars.is_empty() {
        return Err(RunError::NoBars {
            symbol: instrument.as_str().to_owned(),
            resolution: resolution.as_str().to_owned(),
        });
    }
    let samples: Vec<Sample> = bars.into_iter().map(Sample::from_bar).collect();

    // ---- illuminate ------------------------------------------------------------------------------
    progress(emit, 30, "search", "illuminating the Expr-tree archive");
    let il_params = IlluminationParams {
        master_seed: params.seed,
        generations: params.generations.max(1),
        offspring_per_generation: params.offspring.max(1),
        states: params.states.max(2),
    };
    let report = illuminate(il_params, &samples, params.lineage.clone());
    let occupied_cells = report.occupied_cells();
    let distinct_evaluations = report.distinct_evaluations();

    // ---- collect cell champions (the uncensored deflation population) ----------------------------
    // One champion per occupied niche, gathered in deterministic (sorted-cell) order, then sorted by
    // descending fitness with a stable tie-break and deduplicated by canonical hash.
    let mut champions: Vec<(f64, ExprTree)> = report
        .archive
        .occupied_cells()
        .filter_map(|cell| report.archive.best_in(cell))
        .filter(|e| e.fitness.is_finite())
        .map(|e| (e.fitness, e.tree.clone()))
        .collect();
    champions.sort_by(|(a, _), (b, _)| b.total_cmp(a));
    dedup_by_canonical_hash(&mut champions);
    if champions.is_empty() {
        return Err(RunError::NoElites);
    }

    // ---- deflation over the uncensored champion population ---------------------------------------
    progress(
        emit,
        60,
        "deflate",
        "assessing GP deflation over the population",
    );
    let population: Vec<Vec<f64>> = champions
        .iter()
        .map(|(_, tree)| formula_returns(tree, &samples, il_params.states))
        .collect();
    let deflation = assess_gp_champion(
        &population,
        0, // the best-fitness champion under scrutiny
        distinct_evaluations,
        occupied_cells,
        il_params.generations,
        EVOLVE_WINDOWS,
        CSCV_BLOCKS,
    );

    // ---- freeze the top-K distinct trees ---------------------------------------------------------
    let k = params.k.clamp(1, MAX_POOL_SIZE);
    let top_trees: Vec<ExprTree> = champions
        .iter()
        .take(k)
        .map(|(_, tree)| tree.clone())
        .collect();
    let frozen = FrozenPool::freeze(&top_trees).map_err(|e| RunError::Freeze(e.to_string()))?;
    if frozen.is_empty() {
        return Err(RunError::NoElites);
    }

    // ---- seal the pool artefact under the (separate) pool root ------------------------------------
    progress(emit, 90, "seal", "sealing formula pool");
    let pool_id = params.lineage.id()?;
    let content = FormulaPoolContent {
        format_version: POOL_FORMAT_VERSION,
        pool_id: pool_id.clone(),
        mode: pool_mode(params.mode),
        formulas: frozen
            .formulas
            .iter()
            .map(|f| PoolFormula {
                sexpr: f.sexpr.clone(),
                formula_hash: f.formula_hash.clone(),
            })
            .collect(),
        deflation: DeflationSummary {
            // The distinct-canonical count is the real trial-counter output (QE-439 basis), so the pool
            // records a GP-aware basis (an *absent* basis would be a later production hard-block, QE-454).
            gp_aware: true,
            distinct_evaluations: deflation.distinct_evaluations,
            n_trials: deflation.n_trials as u64,
            analytic_floor: deflation.analytic_floor as u64,
            variance_trials: deflation.variance_trials as u64,
            trial_variance: dec(deflation.trial_variance),
            expected_max_sharpe: dec(deflation.expected_max_sharpe),
            champion_dsr: dec(deflation.champion_dsr),
            uncensored_pbo: deflation.uncensored_pbo.map(dec),
        },
        // QE-454 Phase B: the per-formula tradability/parsimony evidence (§13.5 hard-blocks 5–8) is not
        // yet emitted by the sandbox evolve path — left absent so the pool serialises byte-identically to a
        // pre-Phase-B pool. An absent evidence block is a production seal hard-block (every absent stat
        // blocks); wiring the real per-formula IC/cost/MDL/null evidence is a production-path follow-up.
        gate_evidence: None,
        lineage: PoolLineage {
            campaign_id: pool_id.clone(),
            seed: params.seed,
            mode: pool_mode(params.mode),
            code_commit: params.lineage.code_commit.clone(),
            input_snapshot_id: params.lineage.input_snapshot_id.clone(),
            config_hash: params.lineage.config_hash.clone(),
            pool_hash: frozen.pool_hash(),
        },
    };
    let pool = FormulaPool::seal(content).map_err(|e| RunError::Pool(e.to_string()))?;
    let content_hash = pool.content_hash.clone();
    let pool_path = FormulaPoolRepository::new(&params.pool_root)
        .write(&pool)
        .map_err(|e| RunError::Pool(e.to_string()))?;

    let result = EvolveResultDoc {
        pool_id: pool_id.clone(),
        content_hash: content_hash.clone(),
        pool_hash: frozen.pool_hash(),
        mode: params.mode.as_str().to_owned(),
        profile: params.profile.clone(),
        instrument: params.instrument.clone(),
        window: EvolveWindow {
            start: params.start.clone(),
            end: params.end.clone(),
            resolution: params.resolution.clone(),
        },
        seed: params.seed,
        generations: il_params.generations,
        offspring: il_params.offspring_per_generation,
        occupied_cells,
        distinct_evaluations,
        pool_size: frozen.len(),
    };

    // ---- archive snapshot (heatmap cells + trial-basis) for GET /api/runs/{id}/archive ------------
    // Deterministic: `occupied_cells()` iterates in sorted-cell order, so the heatmap is stable for a
    // fixed seed. Purely diagnostic (not hashed) — the authoritative deflation lives in the sealed pool.
    let cells: Vec<ArchiveCell> = report
        .archive
        .occupied_cells()
        .filter_map(|cell| report.archive.best_in(cell).map(|elite| (cell, elite)))
        .map(|(cell, elite)| ArchiveCell {
            family: format!("{:?}", cell.family),
            timescale: format!("{:?}", cell.timescale),
            complexity: format!("{:?}", cell.complexity),
            node_count: elite.tree.node_count(),
            best_fitness: elite.fitness.is_finite().then_some(elite.fitness),
        })
        .collect();
    let archive = EvolveArchive {
        pool_id: pool_id.clone(),
        mode: params.mode.as_str().to_owned(),
        generations: il_params.generations,
        offspring: il_params.offspring_per_generation,
        cells,
        trial_basis: ArchiveTrialBasis {
            distinct_evaluations: deflation.distinct_evaluations,
            n_trials: deflation.n_trials as u64,
            analytic_floor: deflation.analytic_floor as u64,
            expected_max_sharpe: deflation
                .expected_max_sharpe
                .is_finite()
                .then_some(deflation.expected_max_sharpe),
            occupied_cells,
            total_cells: EXPR_CELLS,
        },
    };

    Ok(EvolveOutcome {
        pool_id,
        pool_path,
        content_hash,
        result,
        archive,
    })
}

/// Map a wire [`EvolveMode`] onto the artefact's [`PoolMode`].
fn pool_mode(mode: EvolveMode) -> PoolMode {
    match mode {
        EvolveMode::Sandbox => PoolMode::Sandbox,
        EvolveMode::Production => PoolMode::Production,
    }
}

/// Round a `f64` deflation stat to a bounded, round-trip-stable [`Decimal`]; non-finite → `0`.
fn dec(value: f64) -> Decimal {
    Decimal::from_f64(value)
        .map(|d| d.round_dp(STAT_SCALE_DP))
        .unwrap_or(Decimal::ZERO)
}

/// Deduplicate `(fitness, tree)` pairs by canonical hash, keeping the first (highest-fitness) occurrence.
fn dedup_by_canonical_hash(champions: &mut Vec<(f64, ExprTree)>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    champions.retain(|(_, tree)| seen.insert(tree.canonical_hash()));
}

/// Emit a coarse `progress` line through the sink.
fn progress(emit: &mut dyn FnMut(ProgressLine), pct: u8, stage: &str, msg: &str) {
    emit(ProgressLine::Progress {
        pct,
        stage: stage.to_owned(),
        msg: msg.to_owned(),
    });
}
