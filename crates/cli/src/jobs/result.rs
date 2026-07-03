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
    /// Slippage-model label.
    pub slippage_model: String,
}

/// The six headline metrics (§8.1 `metrics`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Compound annual growth rate (fraction).
    pub cagr: f64,
    /// Annualised Sharpe ratio.
    pub sharpe: f64,
    /// Annualised Sortino ratio.
    pub sortino: f64,
    /// Maximum drawdown (`≤ 0`).
    pub max_dd: f64,
    /// Fraction of winning trades (gross; see `metrics::win_rate`).
    pub win_rate: f64,
    /// Gross profit factor (see `metrics::profit_factor`).
    pub profit_factor: f64,
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
                sharpe: 2.14,
                sortino: 3.08,
                max_dd: -0.083,
                win_rate: 0.582,
                profit_factor: 1.94,
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
        // metrics keys
        for k in [
            "cagr",
            "sharpe",
            "sortino",
            "max_dd",
            "win_rate",
            "profit_factor",
        ] {
            assert!(v["metrics"].get(k).is_some(), "missing metrics.`{k}`");
        }
        assert!(v["metrics"]["profit_factor"].is_number());
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
