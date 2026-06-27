//! Regime labelling (QE-125).
//!
//! "Regime-sensitive" optimisation and reporting need regime *tags*. This module labels history along
//! two orthogonal, interpretable axes — **volatility** (`Calm`/`Volatile`) and **trend-vs-chop**
//! (`Trending`/`Choppy`) — and builds a per-regime **expectancy table** for any strategy/ensemble return
//! series. Both consumers — the DE objective (QE-127, in `qe-ensemble`) and validation reporting
//! (QE-133) — import these pure functions over `qe_domain::Bar`; the labeller lives in `qe-signal`
//! because it is the only crate both `qe-wfo` and `qe-ensemble` depend on (the QE-001/QE-132
//! search⟂portfolio firewall keeps `qe-ensemble` off `qe-wfo`).
//!
//! Classification is deterministic (no HMM / EM fit): the volatility axis is a **median split** of the
//! rolling realised volatility, and the trend axis is Kaufman's **efficiency ratio** against a fixed
//! threshold. Strategy *conditioning* on regimes is out of scope (QE-125 produces tags only).

use std::collections::BTreeMap;

use qe_domain::Bar;
use rust_decimal::prelude::ToPrimitive;

/// Default rolling window (bars) for the volatility / efficiency statistics.
pub const DEFAULT_REGIME_WINDOW: usize = 20;
/// Default efficiency-ratio cutoff (`≥` ⇒ `Trending`). The ratio is in `[0, 1]`, so this is
/// asset-independent.
pub const DEFAULT_TREND_THRESHOLD: f64 = 0.5;

/// Volatility axis of a [`Regime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VolState {
    /// At or below the median rolling volatility.
    Calm,
    /// Above the median rolling volatility.
    Volatile,
}

/// Trend-vs-chop axis of a [`Regime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrendState {
    /// Efficiency ratio `≥ trend_threshold` — a directional move.
    Trending,
    /// Efficiency ratio `< trend_threshold` — mean-reverting / sideways.
    Choppy,
}

/// A market regime — the product of the volatility and trend axes (4 regimes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Regime {
    /// Volatility state.
    pub vol: VolState,
    /// Trend-vs-chop state.
    pub trend: TrendState,
}

impl Regime {
    /// Construct a regime from its two axes.
    #[must_use]
    pub fn new(vol: VolState, trend: TrendState) -> Self {
        Regime { vol, trend }
    }
}

/// Configuration for [`label_regimes`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegimeConfig {
    /// Rolling window (bars) for the volatility / efficiency statistics. The first `window` bars are
    /// unlabelled (`None`).
    pub window: usize,
    /// Efficiency-ratio cutoff for `Trending`.
    pub trend_threshold: f64,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        RegimeConfig {
            window: DEFAULT_REGIME_WINDOW,
            trend_threshold: DEFAULT_TREND_THRESHOLD,
        }
    }
}

impl RegimeConfig {
    /// The QE-125 default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        RegimeConfig::default()
    }
}

/// Close price of `bar` as `f64` (prices are positive `Decimal`s; `to_f64` is lossless for the
/// magnitudes here and only fails on non-finite, which a validated `Price` never is).
fn close_f64(bar: &Bar) -> f64 {
    bar.close().get().to_f64().unwrap_or(0.0)
}

/// Label each bar of `bars` along the volatility and trend axes (QE-125/D1).
///
/// Returns one entry per bar; the first `cfg.window` bars are `None` (the rolling statistics are
/// undefined in the warm-up). The volatility axis is a **median split** of the rolling realised
/// volatility (std-dev of log-returns over the window); the trend axis is Kaufman's **efficiency ratio**
/// `|close[i] − close[i−W]| / Σ|close[k] − close[k−1]|` against `cfg.trend_threshold`. Deterministic.
#[must_use]
pub fn label_regimes(bars: &[Bar], cfg: &RegimeConfig) -> Vec<Option<Regime>> {
    let n = bars.len();
    let w = cfg.window.max(1);
    if n <= w {
        return vec![None; n];
    }

    let closes: Vec<f64> = bars.iter().map(close_f64).collect();
    // log-returns; rets[k] is the return into bar k (k ≥ 1). rets[0] is unused.
    let mut rets = vec![0.0f64; n];
    for k in 1..n {
        let (p0, p1) = (closes[k - 1], closes[k]);
        rets[k] = if p0 > 0.0 && p1 > 0.0 {
            (p1 / p0).ln()
        } else {
            0.0
        };
    }

    // Per-bar rolling volatility and efficiency ratio (None in the warm-up).
    let mut vols: Vec<Option<f64>> = vec![None; n];
    let mut effs: Vec<Option<f64>> = vec![None; n];
    for i in w..n {
        // Realised volatility = std-dev of the W log-returns ending at i.
        let window_rets = &rets[i - w + 1..=i];
        let mean = window_rets.iter().sum::<f64>() / w as f64;
        let var = window_rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / w as f64;
        vols[i] = Some(var.sqrt());

        // Efficiency ratio = |net move| / Σ|per-bar move| over the window (price space).
        let net = (closes[i] - closes[i - w]).abs();
        let path: f64 = (i - w + 1..=i)
            .map(|k| (closes[k] - closes[k - 1]).abs())
            .sum();
        effs[i] = Some(if path > 0.0 { net / path } else { 0.0 });
    }

    // Median of the defined rolling vols → the Calm/Volatile split.
    let mut sorted: Vec<f64> = vols.iter().filter_map(|v| *v).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if sorted.is_empty() {
        0.0
    } else if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        let mid = sorted.len() / 2;
        (sorted[mid - 1] + sorted[mid]) / 2.0
    };

    (0..n)
        .map(|i| match (vols[i], effs[i]) {
            (Some(v), Some(e)) => {
                let vol = if v > median {
                    VolState::Volatile
                } else {
                    VolState::Calm
                };
                let trend = if e >= cfg.trend_threshold {
                    TrendState::Trending
                } else {
                    TrendState::Choppy
                };
                Some(Regime { vol, trend })
            }
            _ => None,
        })
        .collect()
}

/// The expectancy of one regime: how a strategy/ensemble performed while that regime held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegimeExpectancy {
    /// The regime this row summarises.
    pub regime: Regime,
    /// Number of labelled bars in this regime.
    pub count: usize,
    /// Mean per-bar return in this regime — the expectancy.
    pub mean_return: f64,
    /// Sum of per-bar returns in this regime.
    pub total_return: f64,
    /// Fraction of bars in this regime with a positive return.
    pub win_rate: f64,
}

/// A per-regime expectancy table for a strategy/ensemble return series (QE-125/D2 — the AC artefact).
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectancyTable {
    /// One row per regime present, in ascending [`Regime`] order.
    pub rows: Vec<RegimeExpectancy>,
    /// Number of aligned bars with no regime label (warm-up / `None`).
    pub unlabelled: usize,
}

impl ExpectancyTable {
    /// The row for `regime`, if present.
    #[must_use]
    pub fn row(&self, regime: Regime) -> Option<&RegimeExpectancy> {
        self.rows.iter().find(|r| r.regime == regime)
    }

    /// Total labelled bars across all rows.
    #[must_use]
    pub fn labelled(&self) -> usize {
        self.rows.iter().map(|r| r.count).sum()
    }
}

/// Build the per-regime [`ExpectancyTable`] from a strategy/ensemble's per-bar `returns` aligned to
/// `labels` (QE-125/D2). `returns[i]` is attributed to the regime `labels[i]`; the two are paired by
/// index over `min(returns.len(), labels.len())`, and unlabelled (`None`) bars are tallied separately so
/// the rows + `unlabelled` reconcile to the aligned length. Works for *any* return series.
#[must_use]
pub fn expectancy_table(returns: &[f64], labels: &[Option<Regime>]) -> ExpectancyTable {
    let n = returns.len().min(labels.len());

    struct Acc {
        count: usize,
        total: f64,
        wins: usize,
    }
    let mut by_regime: BTreeMap<Regime, Acc> = BTreeMap::new();
    let mut unlabelled = 0usize;

    for i in 0..n {
        match labels[i] {
            Some(regime) => {
                let acc = by_regime.entry(regime).or_insert(Acc {
                    count: 0,
                    total: 0.0,
                    wins: 0,
                });
                acc.count += 1;
                acc.total += returns[i];
                if returns[i] > 0.0 {
                    acc.wins += 1;
                }
            }
            None => unlabelled += 1,
        }
    }

    let rows = by_regime
        .into_iter()
        .map(|(regime, acc)| RegimeExpectancy {
            regime,
            count: acc.count,
            mean_return: acc.total / acc.count as f64,
            total_return: acc.total,
            win_rate: acc.wins as f64 / acc.count as f64,
        })
        .collect();

    ExpectancyTable { rows, unlabelled }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
    use rust_decimal::Decimal;

    /// A 5-minute bar at `i` whose OHLC are all `price` (flat candle — only the close drives regimes).
    fn flat_bar(i: usize, price: f64) -> Bar {
        let p = Price::new(Decimal::try_from(price).unwrap()).unwrap();
        Bar::new(
            Timestamp::from_millis(i as i64 * 300_000),
            Resolution::M5,
            p,
            p,
            p,
            p,
            Qty::new(Decimal::ONE).unwrap(),
            1,
        )
        .unwrap()
    }

    fn bars_from_closes(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &p)| flat_bar(i, p))
            .collect()
    }

    /// A smooth uptrend: each bar +0.5%.
    fn uptrend_closes(n: usize) -> Vec<f64> {
        let mut p = 100.0;
        (0..n)
            .map(|_| {
                let out = p;
                p *= 1.005;
                out
            })
            .collect()
    }

    /// A choppy series: alternates up/down by `amp` around a flat level (no net move).
    fn choppy_closes(n: usize, amp: f64) -> Vec<f64> {
        (0..n)
            .map(|i| if i % 2 == 0 { 100.0 } else { 100.0 + amp })
            .collect()
    }

    #[test]
    fn warmup_bars_are_unlabelled() {
        let bars = bars_from_closes(&uptrend_closes(40));
        let cfg = RegimeConfig::with_defaults();
        let labels = label_regimes(&bars, &cfg);
        assert_eq!(labels.len(), 40);
        assert!(labels[..cfg.window].iter().all(|l| l.is_none()));
        assert!(labels[cfg.window..].iter().all(|l| l.is_some()));
    }

    #[test]
    fn smooth_trend_labels_trending_chop_labels_choppy() {
        let cfg = RegimeConfig::with_defaults();

        let trend = label_regimes(&bars_from_closes(&uptrend_closes(120)), &cfg);
        assert!(trend
            .iter()
            .flatten()
            .all(|r| r.trend == TrendState::Trending));

        let chop = label_regimes(&bars_from_closes(&choppy_closes(120, 1.0)), &cfg);
        assert!(chop.iter().flatten().all(|r| r.trend == TrendState::Choppy));
    }

    #[test]
    fn vol_median_split_separates_low_and_high_vol() {
        // A low-vol uptrend (±0.5%) then a high-amplitude chop (±5 → ~5% swings) ⇒ much higher realised
        // volatility. The high-vol segment carries the global-max rolling vols, so its bars sit above the
        // median split and are labelled Volatile; the low-vol segment carries a strictly lower Volatile
        // rate. (A median split fractures any single near-constant-vol segment on noise, so the robust,
        // meaningful claim is the rate ordering between segments, not a per-bar label.)
        let mut closes = uptrend_closes(120);
        closes.extend(choppy_closes(60, 5.0));
        let labels = label_regimes(&bars_from_closes(&closes), &RegimeConfig::with_defaults());

        let vol_rate = |range: std::ops::Range<usize>| -> f64 {
            let deep: Vec<VolState> = range.filter_map(|i| labels[i].map(|r| r.vol)).collect();
            deep.iter().filter(|v| **v == VolState::Volatile).count() as f64 / deep.len() as f64
        };
        let low_seg = vol_rate(30..100); // deep in the calm uptrend
        let high_seg = vol_rate(140..180); // deep in the volatile chop
        assert_eq!(high_seg, 1.0, "the high-vol segment is entirely Volatile");
        assert!(
            high_seg > low_seg,
            "high-vol segment Volatile-rate {high_seg} should exceed low-vol {low_seg}"
        );
    }

    #[test]
    fn per_regime_expectancy_table_distinguishes_regimes() {
        // History: a calm uptrend then a volatile chop.
        let mut closes = uptrend_closes(100);
        closes.extend(choppy_closes(100, 5.0));
        let bars = bars_from_closes(&closes);
        let cfg = RegimeConfig::with_defaults();
        let labels = label_regimes(&bars, &cfg);

        // A long-biased strategy: it earns the bar's log-return in the trend, but is whipsawed (loses
        // half the magnitude) in the chop — any aligned return series works; this is one.
        let returns: Vec<f64> = (0..bars.len())
            .map(|i| {
                let prev = if i == 0 { 100.0 } else { closes[i - 1] };
                let r = (closes[i] / prev).ln();
                match labels[i].map(|x| x.trend) {
                    Some(TrendState::Trending) => r,
                    Some(TrendState::Choppy) => -r.abs(), // whipsawed in chop
                    None => 0.0,
                }
            })
            .collect();

        let table = expectancy_table(&returns, &labels);

        // The table reconciles: labelled rows + unlabelled == aligned length.
        assert_eq!(table.labelled() + table.unlabelled, bars.len());
        assert!(!table.rows.is_empty());

        // Expectancy is higher in the trending regime than the choppy one — the table separates them.
        let trend_mean = table
            .rows
            .iter()
            .filter(|r| r.regime.trend == TrendState::Trending)
            .map(|r| r.mean_return)
            .fold(f64::NEG_INFINITY, f64::max);
        let chop_mean = table
            .rows
            .iter()
            .filter(|r| r.regime.trend == TrendState::Choppy)
            .map(|r| r.mean_return)
            .fold(f64::INFINITY, f64::min);
        assert!(
            trend_mean > chop_mean,
            "trending expectancy {trend_mean} should beat choppy {chop_mean}"
        );
    }

    #[test]
    fn labelling_is_deterministic_and_table_reconciles() {
        let bars = bars_from_closes(&uptrend_closes(90));
        let cfg = RegimeConfig::with_defaults();
        let a = label_regimes(&bars, &cfg);
        let b = label_regimes(&bars, &cfg);
        assert_eq!(a, b);

        let returns = vec![0.01f64; bars.len()];
        let table = expectancy_table(&returns, &a);
        assert_eq!(table.labelled() + table.unlabelled, bars.len());
        // Every per-bar return is +0.01, so every regime row has win_rate 1 and mean 0.01.
        for row in &table.rows {
            assert!((row.mean_return - 0.01).abs() < 1e-12);
            assert!((row.win_rate - 1.0).abs() < 1e-12);
        }
    }
}
