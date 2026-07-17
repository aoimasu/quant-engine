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
use qe_ensemble::{
    cap_weights, capacity, default_synthetic_shocks, weighted_combined, worst_case_loss,
    CapacityModel, StrategyProfile,
};
use qe_gate::{evaluate_g1, split_with_embargo, G1Criteria, G1Decision};
use qe_risk::{
    calibrate_threshold, calibrate_thresholds, default_calibration_margin, quantize_calibration,
    CalibrationProfile, PortfolioSizer, DEFAULT_FAST_QUANTILE, DEFAULT_FAST_WINDOW,
};
use qe_validation::{
    assess, buy_and_hold_returns, effective_trials, sharpe_ratio, RobustnessReport, SpaConfig,
    VintageStats,
};
use qe_vintage::{Vintage, VintageContent, VintageRepository, VINTAGE_FORMAT_VERSION};
use qe_wfo::backtest::{backtest, BacktestConfig, Bar as DecisionBar};
use qe_wfo::cv_fitness::{
    fold_isolation_fitness, fold_test_ranges, selection_kfold, DEFAULT_LABEL_HORIZON,
};
use qe_wfo::regularise::coverage;
use qe_wfo::{
    fractional_kelly, Genome, MapElitesArchive, OperatorSelector, VariationDriver,
    DEFAULT_KELLY_FRACTION,
};
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

/// Binance USDT-M funding cadence: one stamp every 8 hours (QE-403 coverage grid).
const FUNDING_PERIOD_MS: i64 = 8 * 60 * 60 * 1_000;

/// Target book AUM (USD) the QE-128 capacity model caps the sealed ensemble weights against (QE-416). A
/// documented default book size — a member whose modelled capacity at this AUM is below its equal-weight
/// dollar share is scaled down and the freed budget water-filled to higher-capacity members. Not yet a
/// per-run config input (reviewer sanity-check item).
const TARGET_AUM_USD: f64 = 1_000_000.0;

/// Basis-point denominator for a genome's `size_bps` position sizing (mirrors the WFO backtest's
/// `size_frac = size_bps / 10_000`), used to estimate per-member turnover for the capacity model.
const BPS_DENOMINATOR: f64 = 10_000.0;

/// Scale (`10^12`) the sealed `f64` allocation weights and worst-case-loss figure are rounded to before
/// hashing (QE-416). The vintage's content hash is the digest of `serde_json`'s output, whose **default**
/// float parser is not correctly-rounded: a 17-significant-digit `f64` (e.g. a raw capacity weight or
/// stress loss) can re-parse to a neighbouring `f64` that serialises one ULP differently, breaking the
/// QE-402 content-hash verify on reload. Rounding to 12 decimal places keeps every hashed `f64` within
/// the parser's exact range (far finer than any allocation / risk figure needs) so seal → load is
/// byte-stable.
const HASH_STABLE_SCALE: f64 = 1e12;

/// Round an `f64` to [`HASH_STABLE_SCALE`] so it serialises to a bounded-precision, round-trip-stable
/// decimal (see [`HASH_STABLE_SCALE`]). Non-finite inputs pass through (the vintage's `validate` rejects
/// them at seal).
fn hash_stable(value: f64) -> f64 {
    if value.is_finite() {
        (value * HASH_STABLE_SCALE).round() / HASH_STABLE_SCALE
    } else {
        value
    }
}

/// Funding coverage over a decision-bar series: `(present, expected)` where `present` counts bars carrying
/// a funding stamp and `expected` is the number of 8h grid points spanning `[t_first, t_last]`
/// (`floor(span / 8h) + 1`). Computed over the *actual* bar span (not the requested window, which can
/// exceed the data on hand). Empty / single-bar inputs yield `expected = 1`.
fn funding_coverage(bars: &[DecisionBar]) -> (usize, usize) {
    let present = bars.iter().filter(|b| b.funding_rate.is_some()).count();
    let expected = match (bars.first(), bars.last()) {
        (Some(first), Some(last)) => {
            let span = last.features.time_ms - first.features.time_ms;
            let periods = span.max(0) / FUNDING_PERIOD_MS;
            (periods as usize) + 1
        }
        _ => 1,
    };
    (present, expected.max(1))
}

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
    /// Minimum fraction (`0.0..=1.0`) of the expected 8h funding stamps that must be present over the
    /// training window before a vintage may be sealed (QE-403). Below this the job fails with
    /// [`RunError::FundingCoverage`] rather than selecting on funding-free returns. Sourced from
    /// `config.selection.funding_coverage_min`.
    pub funding_coverage_min: f64,
    /// Number of cross-validation folds the *selection* fitness scores each genome over (QE-415). `≥ 2`;
    /// sourced from `config.selection.cv_folds`. The search records an elite's fitness as the mean per-fold
    /// log-growth over these disjoint, isolated (flat-start) folds instead of a single whole-window in-sample
    /// number. The folds tile the train window (nothing held out) — an in-window robustness signal, not a true
    /// OOS gate; the G1 terminal holdout remains the only true OOS boundary.
    pub cv_folds: usize,
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
    /// Per-chromosome ensemble weight (capacity-capped, QE-128/QE-416; aligned to `selected`).
    pub weights: Vec<f64>,
    /// The converged cross-validated robust-basin ensemble score.
    pub ensemble_score: f64,
    /// The smallest sample size the selected ensemble's correlation penalty rested on across the CV fold
    /// slices (QE-430) — the "tiny sample" flag recorded alongside the score, mirroring `TailRisk::tail_n`.
    /// `0` when fewer than two members were selected (the penalty rests on no pair).
    pub ensemble_corr_effective_n: usize,
    /// Realised funding cashflow (signed) of the selected ensemble over the train window (QE-403
    /// net-of-cost visibility). Weight-summed across chromosomes; unit starting capital per member.
    pub funding_pnl: f64,
    /// Realised funding as a fraction of the ensemble's net P&L over the train window (`funding_pnl /
    /// net_pnl`; `0.0` when net P&L is zero). A funding-free run shows `0.0`, making the gap visible.
    pub funding_fraction_of_net: f64,
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

    // ---- funding-coverage gate (QE-403) ----------------------------------------------------------
    // Funding accrues only on bars carrying a stamp; a sparse/empty series would have every genome
    // selected, DSR/SPA-assessed, and G1-gated on FUNDING-FREE returns — exactly the funding-negative
    // strategies QE-109 exists to reject. Assert a minimum fraction of the expected 8h stamps over the
    // actual bar span BEFORE the search, so an inadequate window errors rather than sealing.
    let (present, expected) = funding_coverage(&decision_bars);
    let funding_frac = present as f64 / expected as f64;
    let floor = params.funding_coverage_min;
    if funding_frac < floor {
        return Err(RunError::FundingCoverage {
            present,
            expected,
            coverage_pct: (funding_frac * 100.0).floor() as u32,
            threshold_pct: (floor * 100.0).round() as u32,
        });
    }

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

    // QE-415: selection fitness = cross-validated fold-isolation robustness, not whole-window in-sample
    // growth. Build the fold geometry ONCE (genome-independent): `cv_folds` (config-driven, ≥ 2) balanced test
    // blocks over the train window, purged/embargoed by the real feature lookback + label horizon so each fold
    // satisfies `windows_disjoint`. The per-genome eval scores the genome on each disjoint test fold in
    // isolation (flat start) and records the mean per-fold log-growth (`.mean`) — same units/scale as the old
    // `elite_fitness()`, so the archive's scalar comparison is unchanged, but selection pressure now rewards
    // genomes that generalise across folds rather than fitting one contiguous stretch.
    //
    // NB: the folds TILE the train window (nothing is held out) — this is an in-window cross-validation
    // robustness signal, not a true OOS gate; the purge/embargo shapes each fold's (unused) train partition
    // and the disjointness invariant, not the scored test blocks. The G1 terminal holdout stays the only true
    // OOS boundary (see `qe_wfo::cv_fitness` module docs).
    let cv = selection_kfold(
        params.cv_folds,
        schema.max_lookback(),
        DEFAULT_LABEL_HORIZON,
    );
    let cv_ranges = fold_test_ranges(&cv, train_bars.len());
    let eval = |g: &Genome| fold_isolation_fitness(g, train_bars, &cv_ranges, &train_cfg).mean;
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

    // ---- QE-416: capacity-capped weights + stress worst-case-loss + observed-behaviour calibration ----
    // Backtest the *selected* members over the train window (deterministic) — the per-member net-return
    // series and trade counts are the inputs to all three sealed artefacts: the QE-128 capacity model
    // (weights), the QE-130 stress set (worst-case loss), and the QE-116 calibration model (per-strategy
    // breaker thresholds). Replaces the equal-weight / `None` / constant placeholders.
    let selected_bt: Vec<qe_wfo::backtest::BacktestResult> = chromosomes
        .iter()
        .map(|g| backtest(g, train_bars, &train_cfg))
        .collect();
    let selected_returns: Vec<Vec<f64>> = selected_bt.iter().map(|b| b.returns.clone()).collect();
    let weights = capacity_capped_weights(&chromosomes, &selected_bt);

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

    // QE-403 net-of-cost visibility: realised funding of the selected ensemble over the train window, and
    // its share of net P&L. A funding-free run reads 0.0 — visible in the sidecar QE-261 consumes.
    let (funding_pnl, net_pnl) =
        ensemble_funding_net(&chromosomes, &weights, train_bars, &train_cfg);
    let funding_fraction_of_net = if net_pnl != 0.0 {
        funding_pnl / net_pnl
    } else {
        0.0
    };
    let n_trials = effective_trials(archive.occupied_cells(), generations, train_cfg.windows);

    // QE-414: the DSR deflation bar's cross-trial Sharpe *dispersion* is estimated from the FULL cell
    // population — the best elite of every occupied cell, one representative Sharpe per behavioural niche
    // — not the top-`MAX_POOL` `pool` (which stays the ensemble/CSCV/SPA population). This population's
    // size is `archive.occupied_cells()`, the same cell factor `n_trials` uses, so the trial count and the
    // trial variance derive from the SAME population. A censored top-N sample under-estimates dispersion
    // and inflates the DSR — exactly what G1 must not reward.
    let variance_returns = cell_champion_returns(&archive, train_bars, &train_cfg);

    let robustness = assess_robustness(
        &pool,
        &variance_returns,
        &in_sample_returns,
        train_bars,
        n_trials,
        params.seed,
    );

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

    // QE-416/QE-130: run the stress set over the capacity-weighted selected members and record the
    // worst-case capital loss (the figure the vintage carries to G3). Only synthetic shocks — the engine
    // has no calendar knowledge to supply historical windows.
    let stress = worst_case_loss(&selected_returns, &weights, &default_synthetic_shocks());
    // QE-416/QE-116: per-strategy breaker thresholds calibrated from each member's replayed equity, keyed
    // by the same positional strategy ids the runtime breaker layer looks up — so every sealed strategy
    // is found (no unintended pre-gating), instead of an empty map that pre-gates the whole vintage.
    let calibration = calibrate_profile(&selected_returns, &weights);

    // QE-433: the advisory portfolio-Kelly sizer. Solve the growth-optimal leverage `f*` on the realised
    // combined **net-of-cost** series (`in_sample_returns` is already net of the QE-431 calibrated cost
    // model, since the train backtest prices cost via `BacktestConfig::default().friction`), apply the
    // fractional (≤½) multiplier `κ`, and seal it. The live netter scales the netted book by it and clamps
    // **below** the pretrade cap — it can cut as readily as raise, and the hard cap stays the backstop.
    let sizer =
        PortfolioSizer::from_kelly(fractional_kelly(&in_sample_returns, DEFAULT_KELLY_FRACTION));

    let content = VintageContent {
        format_version: VINTAGE_FORMAT_VERSION,
        vintage_id: vintage_id.clone(),
        chromosomes,
        weights: weights.clone(),
        calibration,
        // QE-431: the content-addressed slippage/impact calibration that priced selection. The train
        // search runs on `BacktestConfig::default().friction`, whose model derives from this exact
        // `SlippageCalibration::default()`, so sealing it ties the cost coefficients into the lineage.
        // (Wiring a live-fitted calibration through here is the QE-431 follow-up.)
        slippage: qe_risk::SlippageCalibration::default(),
        sizer,
        worst_case_loss: Some(hash_stable(stress.worst_case_loss)),
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
        ensemble_corr_effective_n: ens.corr_effective_n,
        funding_pnl,
        funding_fraction_of_net,
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

/// The net-of-cost train-window return series of the **best elite in every occupied cell**, across both
/// directions — the full cross-niche trial population the MAP-Elites archive retains (QE-414). One
/// representative Sharpe per behavioural niche, gathered in deterministic order (Long then Short;
/// occupied cells are BTreeMap-sorted; the cell champion is `SubPopulation::best()`), so the same search
/// yields the same population ⇒ the same trial variance ⇒ the same DSR.
///
/// This is the *uncensored* Sharpe-dispersion sample the Deflated-Sharpe trial variance is estimated
/// from — every niche champion, not just the top-[`MAX_POOL`] by fitness. Its length equals
/// `archive.occupied_cells()`, the same cell factor `n_trials` counts, so the trial count and the trial
/// variance are derived from the SAME population.
fn cell_champion_returns(
    archive: &MapElitesArchive,
    bars: &[DecisionBar],
    cfg: &BacktestConfig,
) -> Vec<Vec<f64>> {
    let mut series: Vec<Vec<f64>> = Vec::new();
    for direction in [Direction::Long, Direction::Short] {
        let dir = archive.direction(direction);
        for cell in dir.occupied_cells() {
            if let Some(elite) = dir.cell(cell).and_then(|sub| sub.best()) {
                series.push(backtest(&elite.genome, bars, cfg).returns);
            }
        }
    }
    series
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

/// Capacity-capped ensemble weights (QE-128/QE-416): estimate each selected member's per-period capacity
/// from its train-window economics and water-fill the equal-weight budget so no member is allocated more
/// capital than its modelled capacity at [`TARGET_AUM_USD`], replacing the equal-weight overwrite.
///
/// Per member: `gross_edge` ≈ the mean per-period **net** return (a conservative gross proxy — using net
/// understates capacity, never overstates it); `turnover` ≈ round-trip notional per period
/// (`trades · 2 · size_frac / n`). Both are deterministic functions of the seeded search's members.
fn capacity_capped_weights(
    chromosomes: &[Genome],
    selected_bt: &[qe_wfo::backtest::BacktestResult],
) -> Vec<f64> {
    let model = CapacityModel::with_defaults();
    let capacities: Vec<f64> = chromosomes
        .iter()
        .zip(selected_bt)
        .map(|(g, bt)| {
            let n = bt.returns.len().max(1) as f64;
            let gross_edge = bt.returns.iter().sum::<f64>() / n;
            let size_frac = f64::from(g.risk.size_bps) / BPS_DENOMINATOR;
            let turnover = (bt.trades as f64 * 2.0 * size_frac) / n;
            capacity(
                &StrategyProfile {
                    gross_edge,
                    turnover,
                },
                &model,
            )
        })
        .collect();
    cap_or_equal(chromosomes.len(), &capacities)
}

/// Cap the equal-weight budget by `capacities` at [`TARGET_AUM_USD`], falling back to equal weights when
/// the capping would zero the entire budget (every member modelled uneconomic at the target AUM) so the
/// seal still yields a tradeable vintage. Split out from [`capacity_capped_weights`] so the binding /
/// non-binding / degenerate branches are unit-testable without constructing backtest results.
fn cap_or_equal(k: usize, capacities: &[f64]) -> Vec<f64> {
    if k == 0 {
        return Vec::new();
    }
    let equal = vec![1.0 / k as f64; k];
    let capped = cap_weights(&equal, capacities, TARGET_AUM_USD);
    let chosen = if capped.iter().sum::<f64>() > 0.0 {
        capped
    } else {
        equal
    };
    // Round to a hash-stable precision so the sealed weights round-trip byte-identically (a raw capacity
    // weight can carry 17 significant digits that serde_json's default parser does not round-trip).
    chosen.into_iter().map(hash_stable).collect()
}

/// The unit-capital equity curve of a net-return series: `E_0 = 1`, `E_t = E_{t-1}·(1 + r_t)`, as
/// `Decimal` ticks for the QE-116 breaker-calibration measures. Length is `returns.len() + 1`.
fn equity_curve(returns: &[f64]) -> Vec<Decimal> {
    let mut equity = 1.0_f64;
    let mut out = Vec::with_capacity(returns.len() + 1);
    out.push(Decimal::from_f64_retain(equity).unwrap_or(Decimal::ONE));
    for &r in returns {
        equity *= 1.0 + r;
        out.push(Decimal::from_f64_retain(equity).unwrap_or(Decimal::ZERO));
    }
    out
}

/// The per-vintage [`CalibrationProfile`] from replayed equity behaviour (QE-116/QE-416): the ensemble
/// fast-drop from the capacity-weighted ensemble equity curve, and a per-strategy [`BreakerThresholds`]
/// entry for **every** member (keyed by its positional strategy id — the same id
/// [`VintageContent::strategy_ids`] yields and the runtime breaker layer looks up), replacing the
/// constant sidecar whose empty `per_strategy` map pre-gated the whole vintage live.
fn calibrate_profile(selected_returns: &[Vec<f64>], weights: &[f64]) -> CalibrationProfile {
    let margin = default_calibration_margin();
    // Ensemble fast-drop: calibrated from the capacity-weighted ensemble equity curve's fast-window drops.
    let ensemble_equity = equity_curve(&weighted_combined(selected_returns, weights));
    let ensemble_fast_drop = quantize_calibration(calibrate_threshold(
        &qe_risk::fast_drop_distribution(&ensemble_equity, DEFAULT_FAST_WINDOW),
        DEFAULT_FAST_QUANTILE,
        margin,
    ));
    let mut profile = CalibrationProfile::new(ensemble_fast_drop);
    for (i, returns) in selected_returns.iter().enumerate() {
        let equity = equity_curve(returns);
        profile.per_strategy.insert(
            i.to_string(),
            calibrate_thresholds(&equity, DEFAULT_FAST_WINDOW, margin),
        );
    }
    profile
}

/// Weight-summed realised funding and net P&L of `genomes` over `bars` (each member backtested with unit
/// starting capital), as `(funding_pnl, net_pnl)` in `f64`. Both are `Σ_c w_c · x_c`, so an equal-weight
/// ensemble yields the mean per-member figure — the QE-403 funding-visibility inputs for the sidecar.
fn ensemble_funding_net(
    genomes: &[Genome],
    weights: &[f64],
    bars: &[DecisionBar],
    cfg: &BacktestConfig,
) -> (f64, f64) {
    genomes
        .iter()
        .zip(weights.iter())
        .fold((0.0, 0.0), |(f_acc, n_acc), (g, &w)| {
            let res = backtest(g, bars, cfg);
            let funding = res.funding.to_f64().unwrap_or(0.0);
            let net = res.net_pnl.to_f64().unwrap_or(0.0);
            (f_acc + w * funding, n_acc + w * net)
        })
}

/// Assess robustness (DSR / PBO / SPA) for the candidate over the elite `pool`. Falls back to a
/// conservative report (that cannot promote) when the pool is too thin for CSCV or the assessment
/// errors — so a tiny-budget run always reaches the gate rather than aborting.
fn assess_robustness(
    pool: &[Vec<f64>],
    variance_returns: &[Vec<f64>],
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
        // No deflation basis was applied (the DSR is pinned to the conservative floor).
        trial_variance: 0.0,
        variance_trials: 0,
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
        // QE-414: dispersion from the full cell population; `pool` (top-N) stays the CSCV/SPA columns.
        variance_returns,
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

    /// QE-416 AC (a): the sealed weights differ from equal-weight when capacity binds, stay equal when it
    /// does not, and fall back to equal (a tradeable book) when every member is modelled uneconomic.
    #[test]
    fn capacity_capped_weights_differ_from_equal_only_when_capacity_binds() {
        let equal = [0.5, 0.5];

        // Member 0's capacity ($100k) is far below its $500k equal-weight dollar share at the $1M target,
        // so it is capped down and the freed budget water-fills to the high-capacity member 1.
        let binding = cap_or_equal(2, &[100_000.0, 1e15]);
        assert!(
            (binding[0] - 0.5).abs() > 1e-9,
            "capacity binds ⇒ weight differs from equal: {binding:?}"
        );
        assert!(
            binding[0] < 0.5 && binding[1] > 0.5,
            "capped member shrinks, freed budget water-fills the other: {binding:?}"
        );
        // Capacities far above the target ⇒ no binding ⇒ equal weights unchanged.
        let free = cap_or_equal(2, &[1e15, 1e15]);
        assert!(
            (free[0] - 0.5).abs() < 1e-12 && (free[1] - 0.5).abs() < 1e-12,
            "no binding ⇒ equal weights: {free:?}"
        );
        // Every member uneconomic (capacity 0) ⇒ fall back to equal so the vintage still trades.
        let degenerate = cap_or_equal(2, &[0.0, 0.0]);
        assert_eq!(
            degenerate,
            equal.to_vec(),
            "all-uneconomic ⇒ equal-weight fallback (tradeable): {degenerate:?}"
        );
    }
}
