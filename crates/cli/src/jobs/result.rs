//! The backtest **result contract** (`result.json`) — the serialisable shape the admin UI reads.
//!
//! Field names are verbatim from the admin-ui design doc §8.1 (`docs/superpowers/specs/2026-07-02-
//! admin-ui-training-backtest-design.md`). All numbers serialise as JSON numbers; money/qty are
//! carried as `f64` here (exact `Decimal` accounting stays inside the job) so the UI never has to
//! parse a stringified decimal.

use serde::{Deserialize, Serialize};

/// The full backtest result document written to `<run-dir>/result.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestResultDoc {
    /// Read-only strategy header (name/status/tags/params).
    pub strategy: Strategy,
    /// The backtested window.
    pub window: Window,
    /// The instrument universe.
    pub universe: Universe,
    /// The cost assumptions.
    pub costs: Costs,
    /// The six headline metrics.
    pub metrics: Metrics,
    /// Compounded equity curve (starts at `1.0`).
    pub equity_curve: Vec<f64>,
    /// Drawdown series (`≤ 0`), aligned to `equity_curve`.
    pub drawdown: Vec<f64>,
    /// Monthly-return heatmap rows.
    pub monthly_returns: Vec<MonthlyRow>,
    /// Per-trade rows.
    pub trades: Vec<TradeRow>,
}

/// Read-only strategy header (§8.1 `strategy`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strategy {
    /// Human name / vintage id.
    pub name: String,
    /// Lifecycle status (`sealed` | `deployed`).
    pub status: String,
    /// Descriptive tags.
    pub tags: Vec<String>,
    /// Read-only genome header params (stringified key/values).
    pub params: std::collections::BTreeMap<String, String>,
}

/// The backtested window (§8.1 `window`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Inclusive start (`YYYY-MM-DD`).
    pub start: String,
    /// Exclusive end (`YYYY-MM-DD`).
    pub end: String,
    /// Bar resolution (`1h`, …).
    pub resolution: String,
}

/// The instrument universe (§8.1 `universe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Universe {
    /// The symbols backtested.
    pub symbols: Vec<String>,
    /// Symbol count (`symbols.len()`).
    pub count: usize,
}

/// The cost assumptions (§8.1 `costs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Costs {
    /// Taker fee in basis points.
    pub taker_fee_bps: f64,
    /// **Nominal** slippage-model label: records the *requested* model verbatim (e.g.
    /// `"square-root-impact"`). It is not necessarily the friction the v1 engine applied — the
    /// backtester runs its default *linear* `FrictionConfig` (spread + size-impact); this string is a
    /// contract tag, not a re-parametrisation of the engine (see the design note, decision 2).
    pub slippage_model: String,
}

/// The headline metrics (§8.1 `metrics`), made **honest** by QE-468: the headline `sharpe`/`sortino` are
/// now computed on **daily**-aggregated net returns (not per-bar), the Sharpe is Lo (2002)
/// autocorrelation-adjusted, and every figure is reported beside its PSR plus the persisted deflation
/// evidence (DSR/PBO/`N`) so a reader sees `SR (daily, Lo-adj) · PSR · DSR · PBO · N` rather than a bare,
/// `√ppy`-inflated, un-deflated number. The six original keys keep their names (contract-stable); the
/// added fields are the honesty surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Compound annual growth rate (fraction).
    pub cagr: f64,
    /// **Headline** annualised Sharpe: computed on the **daily**-aggregated net-return series and
    /// annualised by the **Lo (2002)** autocorrelation-adjusted factor (`√365 / √(1 + 2·Σ(1−k/q)·ρ_k)`),
    /// not the naïve per-bar `√ppy`. Labelled by [`sharpe_clock`](Self::sharpe_clock).
    pub sharpe: f64,
    /// **Headline** annualised Sortino: computed on the same daily-aggregated series, annualised by
    /// `√365` (Lo's correction is Sharpe-specific; the downside-deviation Sortino keeps the plain daily
    /// annualisation).
    pub sortino: f64,
    /// Maximum drawdown (`≤ 0`).
    pub max_dd: f64,
    /// Fraction of winning trades (gross; see `metrics::win_rate`).
    pub win_rate: f64,
    /// Gross profit factor (see `metrics::profit_factor`).
    pub profit_factor: f64,
    /// The clock + adjustment label for the headline Sharpe (e.g. `"daily, Lo-adj"`) so a per-bar and a
    /// daily figure can never be silently compared (QE-468, López de Prado "Third Law").
    pub sharpe_clock: String,
    /// **Diagnostic only:** the legacy per-bar Sharpe annualised by the naïve `√ppy` — retained beside
    /// the honest headline so the inflation it carried is visible, never the headline (QE-468).
    pub sharpe_per_bar: f64,
    /// **Probabilistic Sharpe Ratio** `P[true SR > 0]` on the daily-aggregated series
    /// (`probabilistic_sharpe_ratio`, skew/kurtosis-aware) — the book's mandated companion to SR (QE-468).
    pub psr: f64,
    /// **Deflated Sharpe Ratio** — read verbatim from the sealed vintage's persisted `SealEvidence`
    /// (QE-467); **not recomputed** on the report path.
    pub dsr: f64,
    /// **Probability of Backtest Overfitting** — read verbatim from the sealed vintage's `SealEvidence`.
    pub pbo: f64,
    /// **Trial count `N`** the DSR deflated against — read verbatim from the sealed vintage's
    /// `SealEvidence` (the "Third Law": report every backtest with its trial count).
    pub n_trials: u64,
}

/// One heatmap row: a year and its twelve monthly returns (§8.1 `monthly_returns`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonthlyRow {
    /// Calendar year.
    pub year: i32,
    /// The twelve monthly returns (fractions); `0.0` for months with no data.
    pub months: [f64; 12],
}

/// One trade row (§8.1 `trades[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeRow {
    /// Stable display id (`#<n>`).
    pub id: String,
    /// Instrument symbol.
    pub symbol: String,
    /// `LONG` | `SHORT`.
    pub side: String,
    /// Entry price (display string).
    pub entry: String,
    /// Exit price (display string).
    pub exit: String,
    /// Holding duration (e.g. `4d 6h`).
    pub hold: String,
    /// Gross price-only return, in percent.
    pub return_pct: f64,
    /// `WIN` | `LOSS`.
    pub result: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_doc() -> BacktestResultDoc {
        BacktestResultDoc {
            strategy: Strategy {
                name: "v-2026-07".into(),
                status: "sealed".into(),
                tags: vec!["crypto".into(), "perp".into()],
                params: BTreeMap::from([("chromosomes".into(), "1".into())]),
            },
            window: Window {
                start: "2021-01-01".into(),
                end: "2021-02-01".into(),
                resolution: "1h".into(),
            },
            universe: Universe {
                symbols: vec!["BTCUSDT".into()],
                count: 1,
            },
            costs: Costs {
                taker_fee_bps: 2.0,
                slippage_model: "square-root-impact".into(),
            },
            metrics: Metrics {
                cagr: 0.412,
                sharpe: 1.62,
                sortino: 2.30,
                max_dd: -0.083,
                win_rate: 0.582,
                profit_factor: 1.94,
                sharpe_clock: "daily, Lo-adj".into(),
                sharpe_per_bar: 2.14,
                psr: 0.87,
                dsr: 0.91,
                pbo: 0.12,
                n_trials: 7680,
            },
            equity_curve: vec![1.0, 1.1],
            drawdown: vec![0.0, 0.0],
            monthly_returns: vec![MonthlyRow {
                year: 2021,
                months: [0.0; 12],
            }],
            trades: vec![TradeRow {
                id: "#0".into(),
                symbol: "BTCUSDT".into(),
                side: "LONG".into(),
                entry: "61204".into(),
                exit: "63180".into(),
                hold: "4d 6h".into(),
                return_pct: 3.23,
                result: "WIN".into(),
            }],
        }
    }

    #[test]
    fn json_keys_match_the_contract_verbatim() {
        let v = serde_json::to_value(sample_doc()).unwrap();
        // top-level keys
        for k in [
            "strategy",
            "window",
            "universe",
            "costs",
            "metrics",
            "equity_curve",
            "drawdown",
            "monthly_returns",
            "trades",
        ] {
            assert!(v.get(k).is_some(), "missing top-level key `{k}`");
        }
        // metrics keys — the six original headline keys plus the QE-468 honesty surface.
        for k in [
            "cagr",
            "sharpe",
            "sortino",
            "max_dd",
            "win_rate",
            "profit_factor",
            "sharpe_clock",
            "sharpe_per_bar",
            "psr",
            "dsr",
            "pbo",
            "n_trials",
        ] {
            assert!(v["metrics"].get(k).is_some(), "missing metrics.`{k}`");
        }
        assert!(v["metrics"]["profit_factor"].is_number());
        // The headline Sharpe is labelled with its clock + adjustment (never a bare number).
        assert_eq!(v["metrics"]["sharpe_clock"], "daily, Lo-adj");
        assert!(v["metrics"]["n_trials"].is_number());
        // window / universe / costs
        assert_eq!(v["window"]["resolution"], "1h");
        assert_eq!(v["universe"]["count"], 1);
        assert_eq!(v["costs"]["taker_fee_bps"], 2.0);
        // trade-row keys
        let t = &v["trades"][0];
        for k in [
            "id",
            "symbol",
            "side",
            "entry",
            "exit",
            "hold",
            "return_pct",
            "result",
        ] {
            assert!(t.get(k).is_some(), "missing trade.`{k}`");
        }
        // monthly row
        assert_eq!(v["monthly_returns"][0]["year"], 2021);
        assert_eq!(
            v["monthly_returns"][0]["months"].as_array().unwrap().len(),
            12
        );
    }

    #[test]
    fn round_trips_through_serde() {
        let doc = sample_doc();
        let json = serde_json::to_string(&doc).unwrap();
        let back: BacktestResultDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }
}
