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

/// Calendar days per year — the annualisation horizon `q` for a **daily** return series (QE-468). The
/// reported headline Sharpe aggregates per-bar returns to daily buckets, then annualises by this many
/// (Lo-adjusted) units, so a per-bar and a daily figure can never be silently compared.
pub const DAYS_PER_YEAR: f64 = 365.0;

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
    let last = *equity.last().expect("equity.len() >= 2 verified above");
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

/// Aggregate per-bar `returns[i]` into the calendar **day** of `times[i]` (epoch-ms, UTC), compounding
/// within a day (`Π(1+r) − 1`) — the same return convention [`equity_curve`] / [`monthly_returns`] use,
/// so the daily series is a coarse-clock resample of the exact same net returns (QE-468). `times` must be
/// aligned to `returns`; extra `times` are ignored, and a bar whose time is missing ends the walk. The
/// output is ordered ascending by UTC day (deterministic — `BTreeMap`-keyed).
///
/// This is the coarse clock the honest headline Sharpe/Sortino annualise from: `√t` scaling assumes IID
/// returns, and per-bar crypto returns are strongly autocorrelated, so aggregating to daily before
/// annualising (by `√365`, Lo-adjusted) removes the `√ppy` inflation of the per-bar figure.
#[must_use]
pub fn daily_returns(returns: &[f64], times: &[i64]) -> Vec<f64> {
    use std::collections::BTreeMap;
    // UTC day index (epoch-ms / ms-per-day) -> compounded growth factor for that day (starts at 1.0).
    const MS_PER_DAY: i64 = 86_400_000;
    let mut acc: BTreeMap<i64, f64> = BTreeMap::new();
    for (i, r) in returns.iter().enumerate() {
        let Some(&t) = times.get(i) else { break };
        // Floor-divide to a UTC day index that is correct for pre-epoch (negative) timestamps too.
        let day = t.div_euclid(MS_PER_DAY);
        let factor = acc.entry(day).or_insert(1.0);
        *factor *= 1.0 + r;
    }
    acc.into_values().map(|f| f - 1.0).collect()
}

/// Sample autocorrelation `ρ_k` of `returns` at lag `k` (biased estimator: the lag-`k` cross-product sum
/// over the **full** sum of squared deviations, the standard ACF normalisation). Returns `0.0` for a lag
/// that is zero, `≥ returns.len()` (no overlapping pairs), a series shorter than two points, or a
/// zero-variance series (`ρ_k` undefined ⇒ no contribution to the Lo factor, never `NaN`).
#[must_use]
pub fn autocorrelation(returns: &[f64], k: usize) -> f64 {
    let n = returns.len();
    if k == 0 || k >= n || n < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / n as f64;
    let denom = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>();
    if denom <= 0.0 {
        return 0.0;
    }
    let num: f64 = (k..n)
        .map(|t| (returns[t] - mean) * (returns[t - k] - mean))
        .sum();
    num / denom
}

/// Lo (2002) autocorrelation-adjusted annualised Sharpe (QE-468): the per-period Sharpe `mean/stdev`
/// scaled by Lo's factor
///
/// ```text
/// √q / √(1 + 2·Σ_{k=1}^{q-1} (1 − k/q)·ρ_k)
/// ```
///
/// where `ρ_k` are the sample [`autocorrelation`]s of `returns` and `q` is the annualisation horizon in
/// those units (e.g. `365` for a daily series). With **zero** autocorrelation the sum vanishes and the
/// factor collapses to the naïve `√q` (so this reduces to [`sharpe`]); **positive** autocorrelation makes
/// the denominator `> 1`, lowering the annualised figure — the honest correction for non-IID returns.
///
/// Lags are capped at `min(⌊q⌋−1, n−1)` (beyond `n−1` there are no overlapping pairs). Degenerate guards
/// (never `NaN`): fewer than two points or zero variance ⇒ `0.0`; a non-positive Lo denominator
/// (pathological strong negative autocorrelation) falls back to the naïve `√q` factor.
#[must_use]
pub fn sharpe_lo_annualised(returns: &[f64], q: f64) -> f64 {
    let n = returns.len();
    if n < 2 || q <= 0.0 {
        return 0.0;
    }
    let nf = n as f64;
    let mean = returns.iter().sum::<f64>() / nf;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (nf - 1.0);
    if var <= 0.0 {
        return 0.0;
    }
    let sr_period = mean / var.sqrt();

    // Σ_{k=1}^{min(q-1, n-1)} (1 − k/q)·ρ_k.
    let max_lag = ((q as usize).saturating_sub(1)).min(n - 1);
    let mut acf_sum = 0.0;
    for k in 1..=max_lag {
        acf_sum += (1.0 - k as f64 / q) * autocorrelation(returns, k);
    }
    let lo_denom = 1.0 + 2.0 * acf_sum;
    // A non-positive denominator would make `√` NaN; fall back to the naïve √q factor.
    let factor = if lo_denom > 0.0 {
        q.sqrt() / lo_denom.sqrt()
    } else {
        q.sqrt()
    };
    sr_period * factor
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

    /// Deterministic, well-mixed pseudo-random value in `[-0.5, 0.5)` (splitmix64 on the index) — used to
    /// build white-noise / AR(1) return series with controllable autocorrelation for the Lo-factor tests.
    fn prng(i: usize) -> f64 {
        let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z as f64) / (u64::MAX as f64) - 0.5
    }

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
    fn reported_daily_lo_sharpe_below_naive_per_bar_sqrt_ppy() {
        // QE-468 regression: for a synthetic *autocorrelated* per-bar (hourly) series, the honest
        // reported headline — daily-aggregated + Lo-adjusted, annualised by √365 — is STRICTLY below the
        // legacy per-bar `√ppy` figure (`sharpe(per_bar, 8760)`), proving the `√ppy` inflation is removed.
        const HOUR_MS: i64 = 3_600_000;
        const PPY_H1: f64 = 8760.0;
        // 30 days of hourly bars with runs of same-sign returns (positive serial dependence) + drift.
        let n = 30 * 24;
        let per_bar: Vec<f64> = (0..n)
            .map(|i| 0.0004 + if (i / 12) % 2 == 0 { 0.0020 } else { -0.0012 })
            .collect();
        let times: Vec<i64> = (0..n).map(|i| i as i64 * HOUR_MS).collect();

        let naive_per_bar = sharpe(&per_bar, PPY_H1);
        let daily = daily_returns(&per_bar, &times);
        let reported = sharpe_lo_annualised(&daily, DAYS_PER_YEAR);

        assert!(
            naive_per_bar > 0.0,
            "sanity: the naïve figure is a positive edge"
        );
        assert!(
            reported < naive_per_bar,
            "honest reported Sharpe must be below the √ppy figure: reported={reported} !< naive={naive_per_bar}"
        );
    }

    #[test]
    fn sharpe_zero_variance_is_zero_not_nan() {
        assert_eq!(sharpe(&[0.0, 0.0, 0.0], 8760.0), 0.0);
        assert_eq!(sharpe(&[0.01], 8760.0), 0.0);
    }

    #[test]
    fn daily_returns_buckets_and_compounds() {
        // Two bars in 2021-01-01 (+10%, -5% -> 1.1*0.95-1 = 0.045); one bar in 2021-01-02 (+2%).
        let day1 = 18_628_i64 * 86_400_000; // 2021-01-01 00:00Z
        let day1b = day1 + 3_600_000; // same UTC day, +1h
        let day2 = day1 + 86_400_000; // 2021-01-02 00:00Z
        let daily = daily_returns(&[0.10, -0.05, 0.02], &[day1, day1b, day2]);
        assert_eq!(daily.len(), 2, "two distinct UTC days");
        assert!(
            (daily[0] - 0.045).abs() < 1e-12,
            "day 1 compounds: {}",
            daily[0]
        );
        assert!((daily[1] - 0.02).abs() < 1e-12, "day 2: {}", daily[1]);
    }

    #[test]
    fn autocorrelation_out_of_range_and_lag_zero_are_zero() {
        let r = [0.01, -0.01, 0.02, 0.00];
        assert_eq!(autocorrelation(&r, 0), 0.0);
        assert_eq!(autocorrelation(&r, 4), 0.0); // k >= n
        assert_eq!(autocorrelation(&r, 9), 0.0);
        assert_eq!(autocorrelation(&[0.0, 0.0, 0.0], 1), 0.0); // zero variance -> 0, not NaN
    }

    #[test]
    fn autocorrelation_detects_positive_serial_dependence() {
        // A slowly alternating trend (runs of same sign) has positive lag-1 autocorrelation.
        let r: Vec<f64> = (0..40)
            .map(|i| if (i / 5) % 2 == 0 { 0.01 } else { -0.01 })
            .collect();
        assert!(autocorrelation(&r, 1) > 0.0, "runs => positive lag-1 ACF");
    }

    #[test]
    fn sharpe_lo_zero_autocorr_matches_naive() {
        // A white-noise series (well-mixed splitmix draws + a small drift) has near-zero autocorrelation
        // at every lag, so Lo's `Σ(1−k/q)ρ_k` ≈ 0 and the factor collapses to the naïve `√q`. A small `q`
        // (few lags) + a long series keeps the finite-sample ACF sampling error small.
        let r: Vec<f64> = (0..4000).map(|i| 0.001 + 0.01 * prng(i)).collect();
        let q = 4.0;
        let naive = sharpe(&r, q);
        let lo = sharpe_lo_annualised(&r, q);
        assert!(
            (lo - naive).abs() / naive.abs() < 0.05,
            "white noise: Lo ≈ naïve √q (naive={naive}, lo={lo})"
        );
    }

    #[test]
    fn sharpe_lo_positive_autocorr_lowers_vs_naive() {
        // A genuinely persistent process — AR(1) with a positive coefficient (`ρ_k = φ^k > 0` for all k)
        // plus a positive drift — makes Lo's denominator exceed 1, so the annualised Sharpe is strictly
        // below the naïve `√q` figure.
        let phi = 0.6;
        let mut prev = 0.0;
        let r: Vec<f64> = (0..600)
            .map(|i| {
                let x = 0.0008 + phi * prev + 0.01 * prng(i);
                prev = x - 0.0008; // carry the mean-zero AR component forward
                x
            })
            .collect();
        let q = 52.0;
        let naive = sharpe(&r, q);
        let lo = sharpe_lo_annualised(&r, q);
        assert!(
            naive > 0.0 && lo > 0.0,
            "both positive here (naive={naive}, lo={lo})"
        );
        assert!(
            lo < naive,
            "positive autocorrelation must lower the annualised Sharpe: lo={lo} !< naive={naive}"
        );
    }

    #[test]
    fn sharpe_lo_zero_variance_is_zero_not_nan() {
        // Mirrors `sharpe_zero_variance_is_zero_not_nan` (metrics.rs) for the Lo-adjusted headline.
        assert_eq!(sharpe_lo_annualised(&[0.0, 0.0, 0.0], DAYS_PER_YEAR), 0.0);
        assert_eq!(sharpe_lo_annualised(&[0.01], DAYS_PER_YEAR), 0.0);
        assert!(sharpe_lo_annualised(&[0.01, -0.01, 0.02, 0.00], DAYS_PER_YEAR).is_finite());
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
