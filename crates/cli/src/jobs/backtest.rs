//! The backtest job: orchestrate a sealed vintage over a window into a [`BacktestResultDoc`]
//! (QE-251 Task 5). Deterministic, single-threaded, no wall-clock/RNG in any output.
//!
//! Pipeline: open store + load/verify vintage (`load`) → scan OHLCV/funding/premium (`scan`) → build
//! decision bars via the feature bridge (`features`) → run each chromosome through
//! `qe_wfo::backtest::backtest_with_trades` and weight-aggregate per-bar returns (`simulate`) → compute
//! the metric contract + map trades (`report`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use qe_domain::{Direction, InstrumentId, Resolution, Timestamp};
use qe_vintage::VintageRepository;
use qe_wfo::backtest::{backtest_with_trades, BacktestConfig, TradeFill};
use qe_wfo::friction::{FeeSchedule, FrictionConfig, SlippageModel};
use qe_wfo::Genome;
use rust_decimal::Decimal;

use super::datetime::parse_ymd_to_millis;
use super::features::{catalogue_schema, check_schema, to_decision_bars};
use super::metrics;
use super::result::{BacktestResultDoc, Costs, Metrics, Strategy, TradeRow, Universe, Window};
use super::RunError;

/// Everything the [`run_backtest`] job needs. Built by `main` from a parsed `Command::Backtest` (+ the
/// store/vintage roots from config) and directly in tests (pointing at the committed fixtures).
#[derive(Debug, Clone)]
pub struct BacktestParams {
    /// Path to the LMDB `MarketStore`.
    pub store_path: PathBuf,
    /// LMDB map size to open the store with.
    pub map_size: usize,
    /// Root directory of the vintage repository (`<root>/<id>.json`).
    pub vintage_root: PathBuf,
    /// Vintage id to load.
    pub vintage_id: String,
    /// Optional single-chromosome selector (`#<index>`); unset ⇒ the whole ensemble.
    pub strategy: Option<String>,
    /// Inclusive window start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive window end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution (`1h`, …).
    pub resolution: String,
    /// Instrument symbols (v1 backtests the first).
    pub universe: Vec<String>,
    /// Taker fee, basis points of notional.
    pub taker_fee_bps: f64,
    /// Slippage-model label (recorded verbatim in the contract).
    pub slippage_model: String,
    /// Reporting participation-impact coefficient (QE-428/QE-440). `None` ⇒ **match selection** — use the
    /// selection cost model's coefficient (`SlippageModel::default().impact_coeff`, i.e.
    /// `FrictionConfig::default()`'s value, the one the train search optimises against) so reported net
    /// PnL matches selection. `Some(v)` ⇒ override the reporting `impact_coeff` with `v` (e.g. `0` to
    /// reproduce the legacy zero-impact reporting).
    pub reporting_impact: Option<Decimal>,
}

/// Run the backtest, emitting coarse progress through `progress(pct, stage, msg)`.
///
/// # Errors
/// [`RunError`] on empty/invalid inputs, a storage/vintage failure, a catalogue schema mismatch, or an
/// empty scan window. See the variants for specifics.
pub fn run_backtest(
    params: &BacktestParams,
    progress: &mut impl FnMut(u8, &str, &str),
) -> Result<BacktestResultDoc, RunError> {
    // ---- load ------------------------------------------------------------------------------------
    progress(10, "load", "opening store and loading vintage");

    if params.universe.is_empty() {
        return Err(RunError::EmptyUniverse);
    }
    let resolution = Resolution::from_str(&params.resolution)
        .map_err(|_| RunError::BadResolution(params.resolution.clone()))?;
    let from = Timestamp::from_millis(
        parse_ymd_to_millis(&params.start)
            .ok_or_else(|| RunError::BadDate(params.start.clone()))?,
    );
    let to = Timestamp::from_millis(
        parse_ymd_to_millis(&params.end).ok_or_else(|| RunError::BadDate(params.end.clone()))?,
    );

    // Canonicalise every requested symbol. NOTE (v1 single-instrument limitation): only `symbols[0]`
    // (`primary`) is actually simulated below — the whole `symbols` list is recorded so
    // `universe.count` reflects the *requested* universe, not the *simulated* one. Multi-instrument
    // portfolio aggregation is out of scope for v1 (design note, decision 3).
    let symbols: Vec<InstrumentId> = params
        .universe
        .iter()
        .map(|s| {
            InstrumentId::new(s).map_err(|source| RunError::Instrument {
                symbol: s.clone(),
                source,
            })
        })
        .collect::<Result<_, _>>()?;
    let primary = symbols[0].clone();

    let store = qe_storage::MarketStore::open(&params.store_path, params.map_size)?;

    let vintage = VintageRepository::new(&params.vintage_root).load(&params.vintage_id)?;
    vintage.verify()?; // belt-and-braces; `load` already verified the content hash.

    // Select chromosomes + weights (whole ensemble, or a single `#<index>` strategy).
    let (chromosomes, weights) = select_chromosomes(&vintage, params.strategy.as_deref())?;
    let schema = catalogue_schema();
    check_schema(&chromosomes, &schema)?;

    // ---- scan ------------------------------------------------------------------------------------
    progress(30, "scan", "scanning bars, funding and premium");
    let bars = store.scan_bars(&primary, resolution, from, to)?;
    if bars.is_empty() {
        return Err(RunError::NoBars {
            symbol: primary.as_str().to_owned(),
            resolution: resolution.as_str().to_owned(),
        });
    }
    let funding = store.scan_funding(&primary, from, to)?;
    let premium = store.scan_premium(&primary, from, to)?;

    // ---- features --------------------------------------------------------------------------------
    progress(50, "features", "assembling decision bars");
    let decision_bars = to_decision_bars(&bars, &funding, &premium);

    // ---- simulate --------------------------------------------------------------------------------
    progress(80, "simulate", "running chromosomes");
    let cfg = backtest_config(params.taker_fee_bps, params.reporting_impact);

    // Per-bar ensemble returns (aligned to bars[1..]): Σ_c weight_c · return_c[t]. All chromosomes run
    // over the same decision bars, so their return series share a length.
    let return_len = decision_bars.len().saturating_sub(1);
    let mut ensemble = vec![0.0_f64; return_len];
    let mut all_fills: Vec<(usize, TradeFill)> = Vec::new(); // (chromosome index, fill)

    for (ci, (genome, &w)) in chromosomes.iter().zip(weights.iter()).enumerate() {
        let (res, fills) = backtest_with_trades(genome, &decision_bars, &cfg);
        for (t, r) in res.returns.iter().enumerate() {
            if t < ensemble.len() {
                ensemble[t] += w * r;
            }
        }
        for f in fills {
            all_fills.push((ci, f));
        }
    }

    // ---- report ----------------------------------------------------------------------------------
    progress(95, "report", "computing metrics and assembling result");
    let doc = assemble_doc(
        params,
        &vintage,
        &primary,
        &symbols,
        &decision_bars,
        &ensemble,
        &mut all_fills,
        resolution,
    );

    Ok(doc)
}

/// Select the chromosomes + aligned weights to backtest. `None` ⇒ the whole ensemble; `Some("#i")` ⇒
/// just chromosome `i` at weight `1.0`.
fn select_chromosomes(
    vintage: &qe_vintage::Vintage,
    strategy: Option<&str>,
) -> Result<(Vec<Genome>, Vec<f64>), RunError> {
    let content = &vintage.content;
    if content.chromosomes.is_empty() {
        return Err(RunError::EmptyVintage);
    }
    match strategy {
        None => Ok((content.chromosomes.clone(), content.weights.clone())),
        Some(sel) => {
            let idx = parse_strategy_index(sel)
                .filter(|&i| i < content.chromosomes.len())
                .ok_or_else(|| RunError::StrategyNotFound(sel.to_owned()))?;
            Ok((vec![content.chromosomes[idx].clone()], vec![1.0]))
        }
    }
}

/// Parse a `#<index>` strategy selector into its `usize` index.
fn parse_strategy_index(sel: &str) -> Option<usize> {
    sel.strip_prefix('#').unwrap_or(sel).parse().ok()
}

/// Build the backtest config: map `taker_fee_bps` onto the fee schedule (fills take liquidity) and set
/// the size-impact coefficient (QE-428).
///
/// The slippage model now defaults to the **selection** cost model (`SlippageModel::default()`, the
/// concave √-in-participation impact — QE-440 — that `FrictionConfig::default()` (the config the train
/// search runs on) prices), so reported net PnL matches selection instead of the legacy zero-impact
/// reporting. `reporting_impact = Some(v)` overrides the participation `impact_coeff` (e.g. `0` reproduces
/// the legacy zero-impact reporting); `None` matches selection.
fn backtest_config(taker_fee_bps: f64, reporting_impact: Option<Decimal>) -> BacktestConfig {
    let default_fees = FeeSchedule::default();
    let taker = Decimal::try_from(taker_fee_bps)
        .ok()
        .map(|d| d / Decimal::from(10_000))
        .unwrap_or(default_fees.taker);
    let fees = FeeSchedule {
        taker,
        maker: default_fees.maker,
    };
    // Default = the selection slippage model, so reporting == selection. An explicit `reporting_impact`
    // overrides only the participation impact coefficient; the half-spread and β stay at the default.
    let slippage = match reporting_impact {
        Some(impact_coeff) => SlippageModel {
            impact_coeff,
            ..SlippageModel::default()
        },
        None => SlippageModel::default(),
    };
    BacktestConfig {
        friction: FrictionConfig {
            fees,
            slippage,
            ..FrictionConfig::default()
        },
        ..BacktestConfig::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble_doc(
    params: &BacktestParams,
    vintage: &qe_vintage::Vintage,
    primary: &InstrumentId,
    symbols: &[InstrumentId],
    decision_bars: &[qe_wfo::backtest::Bar],
    ensemble: &[f64],
    all_fills: &mut [(usize, TradeFill)],
    resolution: Resolution,
) -> BacktestResultDoc {
    // Deterministic trade ordering: by entry bar, then chromosome index.
    all_fills.sort_by_key(|(ci, f)| (f.entry_idx, *ci));

    let symbol = primary.as_str().to_owned();
    let trades: Vec<TradeRow> = all_fills
        .iter()
        .enumerate()
        .map(|(i, (_, f))| trade_row(i, &symbol, f, decision_bars))
        .collect();

    // Return-producing bars are bars[1..]; use their open times for monthly bucketing.
    let times: Vec<i64> = decision_bars.iter().skip(1).map(feature_time).collect();

    let equity = metrics::equity_curve(ensemble);
    let dd = metrics::drawdown(&equity);
    let ppy = periods_per_year(resolution);
    let years = ensemble.len() as f64 / ppy;

    let metrics = Metrics {
        cagr: round10(metrics::cagr(&equity, years)),
        sharpe: round10(metrics::sharpe(ensemble, ppy)),
        sortino: round10(metrics::sortino(ensemble, ppy)),
        max_dd: round10(metrics::max_drawdown(&dd)),
        win_rate: round10(metrics::win_rate(&trades)),
        profit_factor: round10(metrics::profit_factor(&trades)),
    };
    let equity: Vec<f64> = equity.iter().map(|&x| round10(x)).collect();
    let dd: Vec<f64> = dd.iter().map(|&x| round10(x)).collect();
    let mut monthly = metrics::monthly_returns(ensemble, &times);
    for row in &mut monthly {
        for m in &mut row.months {
            *m = round10(*m);
        }
    }

    let mut sparams: BTreeMap<String, String> = BTreeMap::new();
    sparams.insert(
        "chromosomes".to_owned(),
        vintage.content.chromosomes.len().to_string(),
    );
    sparams.insert(
        "format_version".to_owned(),
        vintage.content.format_version.to_string(),
    );
    sparams.insert("content_hash".to_owned(), vintage.content_hash.clone());

    BacktestResultDoc {
        strategy: Strategy {
            name: vintage.content.vintage_id.clone(),
            status: "sealed".to_owned(),
            tags: Vec::new(),
            params: sparams,
        },
        window: Window {
            start: params.start.clone(),
            end: params.end.clone(),
            resolution: resolution.as_str().to_owned(),
        },
        universe: Universe {
            symbols: symbols.iter().map(|s| s.as_str().to_owned()).collect(),
            count: symbols.len(),
        },
        costs: Costs {
            taker_fee_bps: params.taker_fee_bps,
            slippage_model: params.slippage_model.clone(),
        },
        metrics,
        equity_curve: equity,
        drawdown: dd,
        monthly_returns: monthly,
        trades,
    }
}

/// Round a metric to 10 decimal places. Neutralises last-ULP differences from the one transcendental
/// on the metric path (`cagr`'s `powf`), which is not guaranteed correctly-rounded across the arm64
/// (dev) and x86_64 (CI) targets — so the golden `result.json` stays byte-identical. Non-finite values
/// (e.g. `profit_factor = INFINITY` on no losses) pass through unchanged.
fn round10(x: f64) -> f64 {
    if x.is_finite() {
        (x * 1e10).round() / 1e10
    } else {
        x
    }
}

/// The open-time (epoch-ms) of a decision bar, read from its feature vector.
fn feature_time(bar: &qe_wfo::backtest::Bar) -> i64 {
    bar.features.time_ms
}

/// Map one recorded [`TradeFill`] to a display [`TradeRow`].
fn trade_row(i: usize, symbol: &str, f: &TradeFill, bars: &[qe_wfo::backtest::Bar]) -> TradeRow {
    let side = match f.side {
        Direction::Long => "LONG",
        Direction::Short => "SHORT",
    };
    let entry_ms = bars.get(f.entry_idx).map_or(0, feature_time);
    let exit_ms = bars.get(f.exit_idx).map_or(0, feature_time);
    TradeRow {
        id: format!("#{i}"),
        symbol: symbol.to_owned(),
        side: side.to_owned(),
        entry: f.entry_px.normalize().to_string(),
        exit: f.exit_px.normalize().to_string(),
        hold: format_hold(exit_ms - entry_ms),
        return_pct: round10(f.return_frac * 100.0),
        result: if f.return_frac > 0.0 { "WIN" } else { "LOSS" }.to_owned(),
    }
}

/// Format a millisecond duration as `Xd Yh` (or `Yh` when under a day).
fn format_hold(ms: i64) -> String {
    let ms = ms.max(0);
    let days = ms / 86_400_000;
    let hours = (ms % 86_400_000) / 3_600_000;
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h")
    }
}

/// Bar periods per calendar year for the resolution (used to annualise Sharpe/Sortino and CAGR).
fn periods_per_year(resolution: Resolution) -> f64 {
    let minutes_per_year = 365.0 * 24.0 * 60.0;
    minutes_per_year / f64::from(resolution.minutes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_per_year_hourly() {
        assert!((periods_per_year(Resolution::H1) - 8760.0).abs() < 1e-9);
        assert!((periods_per_year(Resolution::D1) - 365.0).abs() < 1e-9);
    }

    #[test]
    fn format_hold_days_and_hours() {
        assert_eq!(format_hold(0), "0h");
        assert_eq!(format_hold(3_600_000), "1h");
        assert_eq!(format_hold(86_400_000 + 6 * 3_600_000), "1d 6h");
        assert_eq!(format_hold(-5), "0h");
    }

    #[test]
    fn parse_strategy_index_forms() {
        assert_eq!(parse_strategy_index("#3"), Some(3));
        assert_eq!(parse_strategy_index("3"), Some(3));
        assert_eq!(parse_strategy_index("#x"), None);
    }

    /// QE-428: the reporting backtest's default size-impact must equal the SELECTION cost model's impact
    /// (the value `train.rs` runs the search on), so reported net PnL matches selection. Non-vacuous:
    /// this fails under the old `impact = ZERO` reporting pin.
    #[test]
    fn reporting_impact_defaults_to_selection_impact() {
        let reporting = backtest_config(2.0, None).friction.slippage.impact_coeff;
        let selection = qe_wfo::backtest::BacktestConfig::default()
            .friction
            .slippage
            .impact_coeff;
        assert_eq!(
            reporting, selection,
            "reporting impact must match the selection cost model"
        );
        assert_ne!(
            reporting,
            Decimal::ZERO,
            "selection prices a non-zero impact; reporting must too"
        );
    }

    /// QE-428/QE-440: an explicit `--reporting-impact` overrides the participation coefficient (e.g. `0`
    /// reproduces the legacy zero-impact reporting), while the half-spread and β stay at their defaults.
    #[test]
    fn reporting_impact_flag_overrides() {
        let cfg = backtest_config(2.0, Some(Decimal::ZERO));
        assert_eq!(cfg.friction.slippage.impact_coeff, Decimal::ZERO);
        assert_eq!(
            cfg.friction.slippage.half_spread,
            SlippageModel::default().half_spread
        );
        assert_eq!(
            cfg.friction.slippage.impact_exponent,
            SlippageModel::default().impact_exponent
        );

        let custom = Decimal::new(3, 4); // 0.0003
        assert_eq!(
            backtest_config(2.0, Some(custom))
                .friction
                .slippage
                .impact_coeff,
            custom
        );
    }
}
