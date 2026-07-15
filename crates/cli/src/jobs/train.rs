//! The training-search job (QE-260): evolve strategies, build an ensemble, validate, run the G1 gate,
//! and **seal a vintage** — mirroring the QE-251 backtest job. Deterministic for a fixed seed,
//! single-threaded, no wall-clock/RNG in the sealed output.
//!
//! Pipeline: open store + scan OHLCV/funding/premium (`load`) → assemble decision bars and split
//! `train | embargo | holdout` (`features`) → MAP-Elites **search** over the train window with the
//! adaptive [`VariationDriver`], emitting per-generation archive coverage + best-so-far fitness
//! (`search`) → discrete-DE **ensemble** construction (`ensemble`) → robustness **validation** +
//! **G1 gate** on the untouched holdout (`gate`) → **seal** a `qe_vintage::Vintage` under the artefacts
//! dir (`seal`).
//!
//! **Catalogue-schema alignment (QE-251 carried context).** The search evolves genomes against
//! [`catalogue_schema`] = `FeatureSchema::from_catalogue(&CatalogueConfig::default())` — the *same*
//! schema the QE-251 backtest job assembles decision bars against — so a vintage sealed here is directly
//! backtestable by QE-251 (proved by the `train_job` integration test, which backtests the sealed
//! vintage).

use std::path::PathBuf;
use std::str::FromStr;

use qe_determinism::{seed_rng, Lineage};
use qe_domain::{Direction, InstrumentId, Resolution, Timestamp};
use qe_gate::{evaluate_g1, split_with_embargo, G1Criteria, G1Decision};
use qe_risk::{CalibrationProfile, Fraction};
use qe_validation::{
    assess, buy_and_hold_returns, effective_trials, sharpe_ratio, RobustnessReport, SpaConfig,
    VintageStats,
};
use qe_vintage::{Vintage, VintageContent, VintageRepository, VINTAGE_FORMAT_VERSION};
use qe_wfo::backtest::{backtest, BacktestConfig, Bar as DecisionBar};
use qe_wfo::regularise::coverage;
use qe_wfo::{Genome, MapElitesArchive, OperatorSelector, VariationDriver};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use super::datetime::parse_ymd_to_millis;
use super::features::{catalogue_schema, check_schema, to_decision_bars};
use super::{ProgressLine, RunError};

/// The even CSCV block count for the small-budget robustness assessment (min meaningful value).
const CSCV_BLOCKS: usize = 2;
/// Cross-validation folds the ensemble portfolio search scores over.
const ENSEMBLE_FOLDS: usize = 4;
/// The default ensemble-search population (small — the pool is a handful of elites).
const ENSEMBLE_POP: usize = 12;
/// The default ensemble-search generations.
const ENSEMBLE_GENERATIONS: usize = 12;
/// Cap on the elite pool the ensemble searches over. The correlation-penalised objective is roughly
/// cubic in the pool size (leave-one-out × pairwise correlation), so an uncapped archive (dozens of
/// elites over a long window) makes the discrete-DE search pathologically slow. The pool is the top-N
/// elites by fitness (deterministic, stable tie-break) — the strongest candidates the ensemble would
/// pick from anyway.
const MAX_POOL: usize = 10;

/// Everything the [`run_train_job`] needs. Built by `main`/`lib` from a parsed `Command::Train` (+ the
/// store/artefacts roots + the config-derived [`Lineage`]) and directly in tests (pointing at the
/// committed fixtures).
#[derive(Debug, Clone)]
pub struct TrainParams {
    /// Path to the LMDB `MarketStore` the training bars are scanned from.
    pub store_path: PathBuf,
    /// LMDB map size to open the store with.
    pub map_size: usize,
    /// Root of the vintage repository the sealed vintage is written into (`<artifacts>/vintages`).
    pub vintage_root: PathBuf,
    /// The instrument to train over (the first configured universe symbol).
    pub instrument: String,
    /// Inclusive window start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive window end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution (`1h`, …).
    pub resolution: String,
    /// The master search seed (single-threaded, deterministic). Defaults to the config seed.
    pub seed: u64,
    /// MAP-Elites search generations (small-budget default).
    pub generations: usize,
    /// Variation steps per direction per generation.
    pub population: usize,
    /// Number of final bars reserved as the untouched G1 holdout.
    pub holdout: usize,
    /// Embargo bars purged between the train window and the holdout.
    pub embargo: usize,
    /// The config-derived lineage (config hash + snapshot + code commit + seed). Its [`Lineage::id`] is
    /// the sealed vintage id — deterministic and independent of the stochastic search.
    pub lineage: Lineage,
    /// The operating profile label (recorded in the result sidecar).
    pub profile: String,
}

/// The training window recorded in the result sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct TrainWindow {
    /// Inclusive start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution.
    pub resolution: String,
}

/// The `result.json` sidecar the train job produces (consumed by QE-261). Deterministic for a fixed seed.
#[derive(Debug, Clone, Serialize)]
pub struct TrainResultDoc {
    /// The sealed vintage id (64-hex lineage id).
    pub vintage_id: String,
    /// The vintage content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The operating profile label.
    pub profile: String,
    /// The instrument trained over.
    pub instrument: String,
    /// The training window.
    pub window: TrainWindow,
    /// The master search seed.
    pub seed: u64,
    /// MAP-Elites generations run.
    pub generations: usize,
    /// Variation steps per direction per generation.
    pub population: usize,
    /// Total occupied MAP-Elites cells across both directions.
    pub coverage: usize,
    /// Occupied cells in the Long archive.
    pub coverage_long: usize,
    /// Occupied cells in the Short archive.
    pub coverage_short: usize,
    /// Best archive fitness at the end of the search.
    pub best_fitness: f64,
    /// Number of elite trials the ensemble searched over.
    pub pool_size: usize,
    /// Indices (into the elite pool) of the chromosomes selected into the ensemble.
    pub selected: Vec<usize>,
    /// Per-chromosome ensemble weight (equal-weight, aligned to `selected`).
    pub weights: Vec<f64>,
    /// The converged cross-validated robust-basin ensemble score.
    pub ensemble_score: f64,
    /// The robustness diagnostics (DSR / PBO / SPA).
    pub robustness: RobustnessReport,
    /// The recorded G1 decision (promotion verdict + per-criterion evidence).
    pub g1: G1Decision,
}

/// The outcome of a training run: the sealed vintage + the result document.
#[derive(Debug, Clone)]
pub struct TrainOutcome {
    /// The sealed vintage id (64-hex lineage id).
    pub vintage_id: String,
    /// Where the sealed vintage was written (`<vintage_root>/<vintage_id>.json`).
    pub vintage_path: PathBuf,
    /// The vintage content hash.
    pub content_hash: String,
    /// The result sidecar (written to `<run-dir>/result.json` by `main`).
    pub result: TrainResultDoc,
}

/// Run the training pipeline, streaming structured [`ProgressLine`]s through `emit`.
///
/// # Errors
/// [`RunError`] on invalid inputs, a storage/vintage failure, an empty scan window, a too-short training
/// window, or an empty search (no elites) / empty ensemble.
pub fn run_train_job(
    params: &TrainParams,
    emit: &mut dyn FnMut(ProgressLine),
) -> Result<TrainOutcome, RunError> {
    // ---- load ------------------------------------------------------------------------------------
    progress(emit, 10, "load", "opening store and scanning training bars");
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
    let funding = store.scan_funding(&instrument, from, to)?;
    let premium = store.scan_premium(&instrument, from, to)?;

    // ---- features + split ------------------------------------------------------------------------
    progress(
        emit,
        20,
        "features",
        "assembling decision bars and splitting train/holdout",
    );
    let schema = catalogue_schema();
    let decision_bars = to_decision_bars(&bars, &funding, &premium);

    // The holdout is the final `holdout` bars; `embargo` purges the boundary so no train information
    // leaks into the untouched G1 slice.
    let split = split_with_embargo(decision_bars.len(), params.holdout, params.embargo);
    let train_bars = &decision_bars[split.train.clone()];
    let holdout_bars = &decision_bars[split.holdout];
    if train_bars.len() < 2 {
        return Err(RunError::TrainWindowTooShort);
    }

    // Small-budget backtest config: a low min-trade gate so short fixture series produce finite fitness.
    // The *sealed* genomes are still evolved fully net-of-cost (QE-109 friction is unchanged).
    let train_cfg = BacktestConfig {
        min_trades: 1,
        windows: 2,
        ..BacktestConfig::default()
    };

    // ---- search ----------------------------------------------------------------------------------
    let generations = params.generations.max(1);
    let population = params.population.max(1);
    let mut archive = MapElitesArchive::new(schema.clone());
    let mut long = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Long);
    let mut short = VariationDriver::new(OperatorSelector::with_defaults(), Direction::Short);
    let mut rng = seed_rng(params.seed);
    let eval = |g: &Genome| backtest(g, train_bars, &train_cfg).elite_fitness();
    let mut best_fitness = f64::NEG_INFINITY;

    for generation in 1..=generations {
        for _ in 0..population {
            let _ = long.step(&mut archive, &schema, &mut rng, eval);
            let _ = short.step(&mut archive, &schema, &mut rng, eval);
        }
        best_fitness = archive_best(&archive).max(best_fitness);
        let coverage_long = coverage(&archive, Direction::Long);
        let coverage_short = coverage(&archive, Direction::Short);
        emit(ProgressLine::Gen {
            pct: search_pct(generation, generations),
            stage: "search".to_owned(),
            generation,
            generations,
            coverage: coverage_long + coverage_short,
            coverage_long,
            coverage_short,
            // The shared protocol field is `Option<f64>` (the server's non-finite-as-null tolerance);
            // a non-finite `best_fitness` still serializes to `null`, so the wire bytes are unchanged.
            best_fitness: Some(best_fitness),
        });
    }

    // ---- collect the elite pool ------------------------------------------------------------------
    let pool_genomes = elite_pool(&archive);
    if pool_genomes.is_empty() {
        return Err(RunError::NoElites);
    }
    // Per-elite net-of-cost return series over the train window (the ensemble trials + DSR/SPA columns).
    let pool: Vec<Vec<f64>> = pool_genomes
        .iter()
        .map(|g| backtest(g, train_bars, &train_cfg).returns)
        .collect();

    // ---- ensemble --------------------------------------------------------------------------------
    let ens_cfg = qe_ensemble::SearchConfig {
        pop_size: ENSEMBLE_POP,
        generations: ENSEMBLE_GENERATIONS,
        folds: ENSEMBLE_FOLDS,
        ..qe_ensemble::SearchConfig::default()
    };
    let ens = qe_ensemble::search_portfolio(&pool, &ens_cfg, params.seed);
    let selected = ens.best.members();
    if selected.is_empty() {
        return Err(RunError::EmptyEnsemble);
    }
    let chromosomes: Vec<Genome> = selected.iter().map(|&i| pool_genomes[i].clone()).collect();
    // Catalogue-schema backstop: the evolved+repaired genomes must be valid against the schema the
    // backtest job assembles against (QE-251 alignment). This never fires for search output, but makes
    // the invariant explicit at the seal boundary.
    check_schema(&chromosomes, &schema)?;
    let k = chromosomes.len();
    let weights = vec![1.0 / k as f64; k];
    emit(ProgressLine::Ensemble {
        pct: 75,
        stage: "ensemble".to_owned(),
        folds: ens_cfg.folds,
        members: k,
        score: Some(ens.score),
    });

    // ---- validation + G1 gate --------------------------------------------------------------------
    let in_sample_returns = combine(&chromosomes, &weights, train_bars, &train_cfg);
    let holdout_returns = combine(&chromosomes, &weights, holdout_bars, &train_cfg);
    let n_trials = effective_trials(archive.occupied_cells(), generations, train_cfg.windows);

    let robustness =
        assess_robustness(&pool, &in_sample_returns, train_bars, n_trials, params.seed);

    let in_sample_sharpe = sharpe_ratio(&in_sample_returns);
    let holdout_sharpe = sharpe_ratio(&holdout_returns);
    let g1 = evaluate_g1(
        in_sample_sharpe,
        &holdout_returns,
        &robustness,
        &G1Criteria::with_defaults(),
    );
    emit(ProgressLine::Gate {
        pct: 85,
        stage: "gate".to_owned(),
        promoted: g1.promoted,
        failed: g1.failed_criteria().iter().map(|s| s.to_string()).collect(),
        in_sample_sharpe: Some(in_sample_sharpe),
        holdout_sharpe: Some(holdout_sharpe),
        dsr: Some(robustness.dsr),
        spa_pvalue: Some(robustness.spa_pvalue),
        n_trials,
    });

    // ---- seal ------------------------------------------------------------------------------------
    progress(emit, 95, "seal", "sealing vintage");
    let vintage_id = params.lineage.id()?;
    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: vintage_id.clone(),
        chromosomes,
        weights: weights.clone(),
        // A default calibration sidecar (0.1 ensemble fast-drop). Observed-behaviour calibration
        // (QE-116) and the QE-130 worst-case-loss stress figure feed later gates (G3) and are out of
        // this ticket's scope.
        calibration: CalibrationProfile::new(
            Fraction::new(Decimal::new(1, 1)).expect("0.1 is a valid fraction"),
        ),
        worst_case_loss: None,
        // Pin the identity of the catalogue these chromosomes were evolved against (QE-402) — the
        // exact-match key the backtest/live load boundary asserts. `schema` is the same
        // `catalogue_schema()` the search/seal ran against, so this is the honest identity.
        catalogue: qe_signal::CatalogueIdentity::from_schema(&schema),
        lineage: params.lineage.clone(),
    };
    let vintage = Vintage::seal(content)?;
    let content_hash = vintage.content_hash.clone();
    let vintage_path = VintageRepository::new(&params.vintage_root).write(&vintage)?;

    let coverage_long = coverage(&archive, Direction::Long);
    let coverage_short = coverage(&archive, Direction::Short);
    let result = TrainResultDoc {
        vintage_id: vintage_id.clone(),
        content_hash: content_hash.clone(),
        profile: params.profile.clone(),
        instrument: params.instrument.clone(),
        window: TrainWindow {
            start: params.start.clone(),
            end: params.end.clone(),
            resolution: params.resolution.clone(),
        },
        seed: params.seed,
        generations,
        population,
        coverage: coverage_long + coverage_short,
        coverage_long,
        coverage_short,
        best_fitness,
        pool_size: pool.len(),
        selected,
        weights,
        ensemble_score: ens.score,
        robustness,
        g1,
    };

    Ok(TrainOutcome {
        vintage_id,
        vintage_path,
        content_hash,
        result,
    })
}

/// Emit a coarse `progress` line (load/features/seal stages) through the sink.
fn progress(emit: &mut dyn FnMut(ProgressLine), pct: u8, stage: &str, msg: &str) {
    emit(ProgressLine::Progress {
        pct,
        stage: stage.to_owned(),
        msg: msg.to_owned(),
    });
}

/// Map a completed generation onto the `[20, 70]` search progress band.
fn search_pct(generation: usize, generations: usize) -> u8 {
    let frac = generation as f64 / generations.max(1) as f64;
    (20.0 + frac * 50.0).round().clamp(0.0, 100.0) as u8
}

/// The best (max) elite fitness across both direction archives, or `−∞` if the archive is empty.
fn archive_best(archive: &MapElitesArchive) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for direction in [Direction::Long, Direction::Short] {
        let dir = archive.direction(direction);
        for cell in dir.occupied_cells() {
            if let Some(sub) = dir.cell(cell) {
                if let Some(elite) = sub.best() {
                    if elite.fitness > best {
                        best = elite.fitness;
                    }
                }
            }
        }
    }
    best
}

/// The distinct, finite-fitness elite genomes across both directions, as the ensemble's candidate pool —
/// the top [`MAX_POOL`] by fitness. Deterministic: elites are gathered in a fixed order (Long then Short;
/// occupied cells are BTreeMap-sorted; elites in insertion order), then a **stable** sort by descending
/// fitness keeps that order for ties, so the same search always yields the same pool.
fn elite_pool(archive: &MapElitesArchive) -> Vec<Genome> {
    let mut scored: Vec<(f64, Genome)> = Vec::new();
    for direction in [Direction::Long, Direction::Short] {
        let dir = archive.direction(direction);
        for cell in dir.occupied_cells() {
            if let Some(sub) = dir.cell(cell) {
                for elite in sub.elites() {
                    if elite.fitness.is_finite() && !scored.iter().any(|(_, g)| g == &elite.genome)
                    {
                        scored.push((elite.fitness, elite.genome.clone()));
                    }
                }
            }
        }
    }
    // Stable sort by descending fitness (ties keep gather order → deterministic), then take the top N.
    scored.sort_by(|(a, _), (b, _)| b.total_cmp(a));
    scored.truncate(MAX_POOL);
    scored.into_iter().map(|(_, g)| g).collect()
}

/// Equal-/given-weight combine of `genomes`' net-of-cost return series over `bars`: `Σ_c w_c · r_c[t]`,
/// truncated to the shortest series (all equal for a shared bar slice).
fn combine(
    genomes: &[Genome],
    weights: &[f64],
    bars: &[DecisionBar],
    cfg: &BacktestConfig,
) -> Vec<f64> {
    let series: Vec<Vec<f64>> = genomes
        .iter()
        .map(|g| backtest(g, bars, cfg).returns)
        .collect();
    let len = series.iter().map(Vec::len).min().unwrap_or(0);
    (0..len)
        .map(|t| {
            series
                .iter()
                .zip(weights.iter())
                .map(|(s, &w)| w * s[t])
                .sum()
        })
        .collect()
}

/// Assess robustness (DSR / PBO / SPA) for the candidate over the elite `pool`. Falls back to a
/// conservative report (that cannot promote) when the pool is too thin for CSCV or the assessment
/// errors — so a tiny-budget run always reaches the gate rather than aborting.
fn assess_robustness(
    pool: &[Vec<f64>],
    candidate_returns: &[f64],
    train_bars: &[DecisionBar],
    n_trials: usize,
    seed: u64,
) -> RobustnessReport {
    let conservative = || RobustnessReport {
        observed_sharpe: sharpe_ratio(candidate_returns),
        dsr: 0.0,
        pbo: 1.0,
        spa_pvalue: 1.0,
        n_trials,
    };
    if pool.len() < CSCV_BLOCKS {
        return conservative();
    }
    // SPA benchmark: the instrument's per-bar buy-&-hold return over the train window (aligned to the
    // per-elite return series, length `train_bars − 1`).
    let prices: Vec<f64> = train_bars
        .iter()
        .map(|b| b.price.to_f64().unwrap_or(0.0))
        .collect();
    let benchmark = buy_and_hold_returns(&prices);
    let excess: Vec<Vec<f64>> = pool
        .iter()
        .map(|s| s.iter().zip(benchmark.iter()).map(|(r, b)| r - b).collect())
        .collect();
    let stats = VintageStats {
        candidate_returns,
        trial_returns: pool,
        excess_over_benchmark: &excess,
        n_trials,
        cscv_blocks: CSCV_BLOCKS,
    };
    assess(&stats, &SpaConfig::with_defaults(), seed).unwrap_or_else(|_| conservative())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_pct_spans_the_band() {
        assert_eq!(search_pct(0, 8), 20);
        assert_eq!(search_pct(8, 8), 70);
        assert!((20..=70).contains(&search_pct(4, 8)));
    }
}
