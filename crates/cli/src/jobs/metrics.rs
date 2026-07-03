//! Pure, IO-free performance metrics for the backtest result contract (QE-251 Task 3/4).
//!
//! Every function is deterministic and allocation-only; they take plain `&[f64]` return series (and, for
//! the trade metrics, `&[TradeRow]`) so they are trivially unit-tested against hand-checked values.
//!
//! **Cost provenance (carried QE-252 review note).** [`win_rate`] and [`profit_factor`] read
//! [`TradeRow::return_pct`], which is sourced from `qe_wfo::backtest::TradeFill::return_frac` — a
//! **gross, price-only** round-trip return (no quantity, no fees, no slippage). They are therefore
//! deliberate **gross approximations**: a cost-blind view of trade quality. Net-of-cost performance
//! lives in the equity-curve-derived metrics ([`cagr`], [`sharpe`], [`sortino`]), which are built from
//! the backtester's net-of-cost per-bar returns.

use super::datetime::year_month;
use super::result::{MonthlyRow, TradeRow};

/// Compounded equity from unit capital: `eq[0] = 1`, `eq[i+1] = eq[i]·(1 + returns[i])`.
/// Length is `returns.len() + 1`.
#[must_use]
pub fn equity_curve(returns: &[f64]) -> Vec<f64> {
    let mut eq = Vec::with_capacity(returns.len() + 1);
    let mut v = 1.0;
    eq.push(v);
    for r in returns {
        v *= 1.0 + r;
        eq.push(v);
    }
    eq
}

/// Drawdown series: `(v − running_peak) / running_peak`, `≤ 0`, aligned to `equity`.
/// An empty input yields an empty series.
#[must_use]
pub fn drawdown(equity: &[f64]) -> Vec<f64> {
    let mut peak = f64::MIN;
    equity
        .iter()
        .map(|&v| {
            peak = peak.max(v);
            (v - peak) / peak
        })
        .collect()
}

/// The most-negative value of a drawdown series (`0.0` for an empty series).
#[must_use]
pub fn max_drawdown(drawdown: &[f64]) -> f64 {
    drawdown.iter().copied().fold(0.0, f64::min)
}

/// Compound annual growth rate from an equity curve spanning `years`: `eq_last^(1/years) − 1`.
/// Returns `0.0` for a non-positive `years`, a curve shorter than two points, or a non-positive final
/// equity (a total wipeout has no real geometric growth rate).
#[must_use]
pub fn cagr(equity: &[f64], years: f64) -> f64 {
    if years <= 0.0 || equity.len() < 2 {
        return 0.0;
    }
    let last = *equity.last().unwrap();
    if last <= 0.0 {
        return 0.0;
    }
    last.powf(1.0 / years) - 1.0
}

/// Annualised Sharpe: `mean/stdev · √ppy` over per-bar `returns` (excess over a zero risk-free rate).
/// Zero (or undefined) variance ⇒ `0.0` (never `NaN`); fewer than two points ⇒ `0.0`.
#[must_use]
pub fn sharpe(returns: &[f64], periods_per_year: f64) -> f64 {
    let n = returns.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    if var <= 0.0 {
        return 0.0;
    }
    mean / var.sqrt() * periods_per_year.sqrt()
}

/// Annualised Sortino: `mean/downside_deviation · √ppy`, where the downside deviation uses only
/// negative returns (target = 0). No downside (or fewer than two points) ⇒ `0.0`.
#[must_use]
pub fn sortino(returns: &[f64], periods_per_year: f64) -> f64 {
    let n = returns.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / n;
    // Downside deviation over the full sample (negative returns contribute their square; others 0).
    let downside = returns
        .iter()
        .map(|r| if *r < 0.0 { r * r } else { 0.0 })
        .sum::<f64>()
        / n;
    if downside <= 0.0 {
        return 0.0;
    }
    mean / downside.sqrt() * periods_per_year.sqrt()
}

/// Bucket each per-bar `returns[i]` into the calendar month of `times[i]` (epoch-ms, UTC), compound
/// within a month (`Π(1+r) − 1`), and group the months by year into [`MonthlyRow`]s (ascending year;
/// months with no data are `0.0`). `times` must be aligned to `returns`; extra `times` are ignored.
#[must_use]
pub fn monthly_returns(returns: &[f64], times: &[i64]) -> Vec<MonthlyRow> {
    use std::collections::BTreeMap;
    // year -> ([growth factor per month], [saw-data per month]); factors start at 1.0.
    let mut acc: BTreeMap<i32, ([f64; 12], [bool; 12])> = BTreeMap::new();
    for (i, r) in returns.iter().enumerate() {
        let Some(&t) = times.get(i) else { break };
        let (y, m) = year_month(t);
        let (factors, seen) = acc.entry(y).or_insert(([1.0; 12], [false; 12]));
        let idx = (m - 1) as usize;
        factors[idx] *= 1.0 + r;
        seen[idx] = true;
    }
    acc.into_iter()
        .map(|(year, (factors, seen))| {
            let mut months = [0.0_f64; 12];
            for j in 0..12 {
                // Months with no return report 0.0; others report the compounded return.
                months[j] = if seen[j] { factors[j] - 1.0 } else { 0.0 };
            }
            MonthlyRow { year, months }
        })
        .collect()
}

/// Fraction of winning trades (`return_pct > 0`) out of all trades. No trades ⇒ `0.0`.
///
/// **Gross** (see the module docs): `return_pct` is a price-only round-trip return.
#[must_use]
pub fn win_rate(trades: &[TradeRow]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let wins = trades.iter().filter(|t| t.return_pct > 0.0).count();
    wins as f64 / trades.len() as f64
}

/// Profit factor: `Σ gains / |Σ losses|` over the trades' `return_pct`. No losing trades ⇒
/// [`f64::INFINITY`] (documented convention); no trades at all ⇒ `0.0`.
///
/// **Gross** (see the module docs): computed from price-only round-trip returns.
#[must_use]
pub fn profit_factor(trades: &[TradeRow]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    for t in trades {
        if t.return_pct > 0.0 {
            gains += t.return_pct;
        } else if t.return_pct < 0.0 {
            losses += -t.return_pct;
        }
    }
    if losses == 0.0 {
        return f64::INFINITY;
    }
    gains / losses
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(return_pct: f64) -> TradeRow {
        TradeRow {
            id: "#0".into(),
            symbol: "BTCUSDT".into(),
            side: "LONG".into(),
            entry: "1".into(),
            exit: "1".into(),
            hold: "0h".into(),
            return_pct,
            result: if return_pct >= 0.0 { "WIN" } else { "LOSS" }.into(),
        }
    }

    #[test]
    fn equity_curve_compounds_from_one() {
        let eq = equity_curve(&[0.10, -0.05]);
        assert!((eq[0] - 1.0).abs() < 1e-12);
        assert!((eq[1] - 1.10).abs() < 1e-12);
        assert!((eq[2] - 1.045).abs() < 1e-12); // 1.10 * 0.95
    }

    #[test]
    fn drawdown_zero_at_new_highs_and_negative_below_peak() {
        let dd = drawdown(&equity_curve(&[0.10, -0.05]));
        assert!(dd.iter().all(|d| *d <= 1e-12));
        assert!(*dd.last().unwrap() < -0.03); // below the 1.10 peak
        assert!((max_drawdown(&dd) - *dd.last().unwrap()).abs() < 1e-12);
    }

    #[test]
    fn sharpe_zero_variance_is_zero_not_nan() {
        assert_eq!(sharpe(&[0.0, 0.0, 0.0], 8760.0), 0.0);
        assert_eq!(sharpe(&[0.01], 8760.0), 0.0);
    }

    #[test]
    fn sharpe_known_value() {
        // returns [0.01, -0.01, 0.02, 0.00]; mean=0.005, sample var:
        // devs: .005,-.015,.015,-.005 -> sq: 2.5e-5,2.25e-4,2.25e-4,2.5e-5 sum=5e-4 /3 =1.6667e-4
        // std=0.0129099; sharpe/period=0.005/0.0129099=0.387298; *sqrt(4)=0.774597
        let s = sharpe(&[0.01, -0.01, 0.02, 0.00], 4.0);
        assert!((s - 0.7745966692).abs() < 1e-9, "got {s}");
    }

    #[test]
    fn sortino_uses_downside_only() {
        // returns [0.01,-0.01,0.02,0.00]; mean=0.005; downside=Σ neg^2 /n = (0.01^2)/4 = 2.5e-5
        // dd = 0.005; ratio/period=0.005/0.005=1.0; *sqrt(4)=2.0
        let s = sortino(&[0.01, -0.01, 0.02, 0.00], 4.0);
        assert!((s - 2.0).abs() < 1e-12, "got {s}");
        assert_eq!(sortino(&[0.01, 0.02], 4.0), 0.0); // no downside
    }

    #[test]
    fn cagr_doubling_over_two_years() {
        let eq = vec![1.0, 2.0, 4.0]; // 4x over 2 years -> 2x/yr -> 1.0
        assert!((cagr(&eq, 2.0) - 1.0).abs() < 1e-12);
        assert_eq!(cagr(&eq, 0.0), 0.0);
        assert_eq!(cagr(&[1.0, 0.0], 1.0), 0.0); // wipeout
    }

    #[test]
    fn monthly_returns_buckets_and_compounds() {
        // Jan 2021 has two returns (+10%, -5% -> 1.1*0.95-1 = 0.045); Feb 2021 one (+2%).
        let jan1 = 18628_i64 * 86_400_000; // 2021-01-01
        let jan2 = jan1 + 86_400_000;
        let feb1 = jan1 + 31 * 86_400_000; // 2021-02-01
        let rows = monthly_returns(&[0.10, -0.05, 0.02], &[jan1, jan2, feb1]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].year, 2021);
        assert!((rows[0].months[0] - 0.045).abs() < 1e-12);
        assert!((rows[0].months[1] - 0.02).abs() < 1e-12);
        assert_eq!(rows[0].months[2], 0.0); // March: no data
    }

    #[test]
    fn win_rate_and_profit_factor() {
        assert_eq!(win_rate(&[trade(1.0)]), 1.0);
        assert_eq!(win_rate(&[]), 0.0);
        assert!((win_rate(&[trade(1.0), trade(-1.0)]) - 0.5).abs() < 1e-12);
        // +2 gain vs -1 loss -> 2.0
        assert!((profit_factor(&[trade(2.0), trade(-1.0)]) - 2.0).abs() < 1e-12);
        assert_eq!(profit_factor(&[trade(1.0), trade(2.0)]), f64::INFINITY);
        assert_eq!(profit_factor(&[]), 0.0);
    }
}
