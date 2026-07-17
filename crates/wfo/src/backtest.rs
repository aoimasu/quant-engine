//! Strategy backtester — the fitness engine (QE-120).
//!
//! Turns a [`Genome`] into a [`NoiseRobustFitness`] by walking a bar series: the per-bar signal
//! ([`Genome::decide`], QE-110) fills at the **next bar** (no look-ahead), costs (fees + slippage +
//! funding, QE-109) hit a `Decimal` cash/mark ledger so every return is **net-of-cost**, and the net
//! return series is summarised as geometric time-average fitness over windows (QE-113). A genome with
//! fewer than `min_trades` entries is **rejected as noise** (`fitness.mean = −∞`), and the SE-aware
//! [`should_replace`](crate::fitness::should_replace) the result feeds never churns an elite on a noisy
//! single draw. Elite robustness gates are QE-124.

use qe_determinism::{seed_rng, DetRng};
use qe_domain::{Direction, Side};
use qe_risk::ShockConfig;
use qe_signal::FeatureVector;
use rand_core::RngCore;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::fitness::NoiseRobustFitness;
use crate::friction::{FrictionConfig, Liquidity};
use crate::genome::{Decision, Genome, PositionState};

/// Default minimum entry count below which a genome is rejected as noise.
pub const DEFAULT_MIN_TRADES: usize = 10;

/// Default number of contiguous sub-windows the net return series is split into for the noise estimate.
pub const DEFAULT_WINDOWS: usize = 4;

/// Rolling-ADV lookback in bars (QE-440): ≈ one day of hourly bars. The participation impact keys off
/// `qty / ADV`, where `ADV` is the trailing mean bar volume over this window (inclusive of the fill bar,
/// so there is no look-ahead).
pub const DEFAULT_ADV_WINDOW: usize = 24;

/// Basis-points denominator for `size_bps` (QE-110: `size_bps` is bps of allowed capital).
const BPS_DENOMINATOR: i64 = 10_000;

/// One backtest bar: the decision features, the reference price (fills + mark), and an optional funding
/// rate accrued against the held position at this bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    /// Quantised feature vector the genome decides on (QE-108).
    pub features: FeatureVector,
    /// Reference price for fills and marking (`> 0`).
    pub price: Decimal,
    /// Bar volume in contracts (QE-440), feeding the rolling ADV that keys the participation impact.
    pub volume: Decimal,
    /// Historical funding rate accrued at this bar, if a funding stamp lands here (signed fraction).
    pub funding_rate: Option<Decimal>,
}

/// Backtest configuration.
#[derive(Debug, Clone)]
pub struct BacktestConfig {
    /// Fees / slippage model + cost multiplier (QE-109).
    pub friction: FrictionConfig,
    /// Minimum entries; below this the genome is rejected as noise (QE-120/D4).
    pub min_trades: usize,
    /// Contiguous sub-windows for the noise-robust fitness (`≥ 1`; `≥ 2` for a real SE).
    pub windows: usize,
    /// Bar-level tail-aware scenario shocks injected into the *sizing fitness* (QE-441). `None` (the
    /// default) is the pre-QE-441 raw-historical path — the reporting / holdout / DSR / Kelly-sizer
    /// backtests keep it. The MAP-Elites / DE **selection** fitness runs with `Some` of the frozen,
    /// content-addressed [`ShockConfig`] so a larger size produces a larger shocked drawdown and
    /// `log_growth` self-selects a lower, tail-aware leverage. Frozen/seeded ⇒ byte-reproducible.
    pub shocks: Option<ShockConfig>,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        BacktestConfig {
            friction: FrictionConfig::default(),
            min_trades: DEFAULT_MIN_TRADES,
            windows: DEFAULT_WINDOWS,
            shocks: None,
        }
    }
}

/// The outcome of a backtest.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestResult {
    /// Per-bar net-of-cost returns (baseline bar 0 excluded).
    pub returns: Vec<f64>,
    /// Number of entry fills (flat → position).
    pub trades: usize,
    /// Final equity minus the unit starting capital (net of all costs).
    pub net_pnl: Decimal,
    /// Realised funding cashflow accrued against the held position over the run (signed; negative when
    /// paid). Decomposed out of `net_pnl` for QE-403 net-of-cost visibility; already included in it.
    pub funding: Decimal,
    /// Whether the genome cleared the minimum-trade gate.
    pub accepted: bool,
    /// Noise-robust geometric fitness; `mean = −∞` when rejected as noise.
    pub fitness: NoiseRobustFitness,
}

impl BacktestResult {
    /// The scalar archive fitness (QE-118 stores `f64`) — the mean per-window log-growth.
    #[must_use]
    pub fn elite_fitness(&self) -> f64 {
        self.fitness.mean
    }
}

/// One closed round-trip recorded by [`backtest_with_trades`]: the entry and exit fills of a single
/// position, from flat → position (entry) to position → flat (close). Only *completed* round-trips are
/// recorded — a position still open at the last bar produces no `TradeFill`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TradeFill {
    /// Bar index of the entry fill (flat → position).
    pub entry_idx: usize,
    /// Bar index of the close fill (position → flat).
    pub exit_idx: usize,
    /// Position side (Long/Short) — the *position* side, not the per-fill order side.
    pub side: Direction,
    /// Fill price at entry (`> 0`).
    pub entry_px: Decimal,
    /// Fill price at exit (`> 0`).
    pub exit_px: Decimal,
    /// Signed **gross** price return of the round-trip (`Long: (exit−entry)/entry`,
    /// `Short: (entry−exit)/entry`). A winning trade is `> 0`. Deliberately price-only: net-of-cost
    /// accounting lives in the aggregate [`BacktestResult::returns`] / [`BacktestResult::net_pnl`].
    pub return_frac: f64,
}

/// An order scheduled by bar `i`'s decision, to fill at bar `i+1`.
#[derive(Debug, Clone, Copy)]
enum Pending {
    /// Open a position in this direction (sized from equity at fill time).
    Enter(Direction),
    /// Close the whole open position.
    Close,
}

/// Split `returns` into up to `k` contiguous, non-empty sub-windows (sizes differ by ≤ 1).
fn split_windows(returns: &[f64], k: usize) -> Vec<Vec<f64>> {
    if returns.is_empty() {
        return Vec::new();
    }
    let k = k.max(1).min(returns.len());
    let base = returns.len() / k;
    let rem = returns.len() % k;
    let mut out = Vec::with_capacity(k);
    let mut idx = 0;
    for w in 0..k {
        let size = base + usize::from(w < rem);
        if size == 0 {
            continue;
        }
        out.push(returns[idx..idx + size].to_vec());
        idx += size;
    }
    out
}

/// Backtest `genome` over `bars` with `cfg` — the QE-120 fitness engine. Convenience wrapper over
/// [`backtest_with_trades`] that **discards** the per-trade records; the returned [`BacktestResult`]
/// (`returns` / `net_pnl` / `accepted` / `fitness`) is byte-for-byte identical.
#[must_use]
pub fn backtest(genome: &Genome, bars: &[Bar], cfg: &BacktestConfig) -> BacktestResult {
    backtest_with_trades(genome, bars, cfg).0
}

/// Backtest `genome` over `bars` with `cfg`, additionally recording one [`TradeFill`] per **closed**
/// round-trip. The [`BacktestResult`] is exactly what [`backtest`] returns — the trade recorder is
/// purely additive and never touches the cash/mark ledger, `returns`, or `fitness`.
#[must_use]
pub fn backtest_with_trades(
    genome: &Genome,
    bars: &[Bar],
    cfg: &BacktestConfig,
) -> (BacktestResult, Vec<TradeFill>) {
    let size_frac = Decimal::from(genome.risk.size_bps) / Decimal::from(BPS_DENOMINATOR);

    let mut cash = Decimal::ONE; // unit starting capital
    let mut pos_qty = Decimal::ZERO; // signed position
    let mut equity_prev = Decimal::ONE;
    let mut entry_bar: Option<usize> = None;
    let mut pending: Option<Pending> = None;

    let mut returns: Vec<f64> = Vec::with_capacity(bars.len().saturating_sub(1));
    let mut trades = 0usize;
    let mut funding_accrued = Decimal::ZERO; // QE-403: realised funding, decomposed out of net P&L

    // Trade recorder: the open entry (idx, side, fill price) between a paired entry and close, plus the
    // completed round-trips. `open` is `Some` iff a position is currently held (entries are flat-only
    // and at most one fill lands per bar, so entries and closes strictly alternate).
    let mut open: Option<(usize, Direction, Decimal)> = None;
    let mut fills: Vec<TradeFill> = Vec::new();

    // Rolling ADV window (QE-440): trailing sum + count of bar volumes, inclusive of the current bar
    // (no look-ahead). `adv = sum / count` is exact `Decimal`, so the participation impact is
    // byte-reproducible.
    let mut adv_window: std::collections::VecDeque<Decimal> =
        std::collections::VecDeque::with_capacity(DEFAULT_ADV_WINDOW);
    let mut adv_sum = Decimal::ZERO;

    // QE-441: bar-level tail-aware scenario shocks. Seed one portable ChaCha8 RNG per call from the
    // FROZEN, pre-registered `shock.seed` (deliberately not the run seed), so `backtest` stays a pure
    // function of `(genome, bars, cfg)` — byte-reproducible independent of thread count and across
    // repeated calls. Draws are consumed in bar order below.
    let mut shock_rng: Option<DetRng> = cfg.shocks.as_ref().map(|s| seed_rng(s.seed));

    for (i, bar) in bars.iter().enumerate() {
        let price = bar.price;

        // Roll the ADV window forward with this bar's volume before pricing any fill at this bar.
        if adv_window.len() == DEFAULT_ADV_WINDOW {
            if let Some(oldest) = adv_window.pop_front() {
                adv_sum -= oldest;
            }
        }
        adv_window.push_back(bar.volume);
        adv_sum += bar.volume;
        let adv = adv_sum / Decimal::from(adv_window.len());

        // (1) Execute the order pending from the previous bar's decision, at this bar's price.
        if let Some(order) = pending.take() {
            if price > Decimal::ZERO {
                match order {
                    Pending::Enter(dir) => {
                        let notional = size_frac * equity_prev;
                        let qty = notional / price;
                        if qty > Decimal::ZERO {
                            let side = match dir {
                                Direction::Long => Side::Buy,
                                Direction::Short => Side::Sell,
                            };
                            apply_fill(&mut cash, &mut pos_qty, side, qty, price, adv, cfg);
                            entry_bar = Some(i);
                            trades += 1;
                            open = Some((i, dir, price));
                        }
                    }
                    Pending::Close => {
                        let qty = pos_qty.abs();
                        if qty > Decimal::ZERO {
                            let side = if pos_qty > Decimal::ZERO {
                                Side::Sell
                            } else {
                                Side::Buy
                            };
                            apply_fill(&mut cash, &mut pos_qty, side, qty, price, adv, cfg);
                            entry_bar = None;
                            if let Some((entry_idx, dir, entry_px)) = open.take() {
                                let return_frac = match dir {
                                    Direction::Long => (price - entry_px) / entry_px,
                                    Direction::Short => (entry_px - price) / entry_px,
                                }
                                .to_f64()
                                .unwrap_or(0.0);
                                fills.push(TradeFill {
                                    entry_idx,
                                    exit_idx: i,
                                    side: dir,
                                    entry_px,
                                    exit_px: price,
                                    return_frac,
                                });
                            }
                        }
                    }
                }
            }
        }

        // (2) Funding accrual against the held position (QE-109 sign: longs pay shorts when rate > 0).
        if let Some(rate) = bar.funding_rate {
            let flow = -pos_qty * price * rate;
            cash += flow;
            funding_accrued += flow;
        }

        // (2b) QE-441: inject a bar-level synthetic shock (gap / funding-spike / ADL) onto the HELD
        // notional, BEFORE the bar is marked. Two rolls are consumed EVERY bar (fire? + shape),
        // unconditionally, so the shock schedule is a pure function of `(seed, bar index)` and is
        // position-independent — every genome hits the same shock bars. The loss is an exact `Decimal`
        // fraction of the held notional (`|pos_qty·price|·e = size_frac·equity_prev·e`), so it scales
        // linearly with size: a larger `size_bps` takes a larger drawdown (and past a leverage threshold
        // drives the bar return ≤ −1 → ruin), pulling the fitness-maximising leverage down (tail-aware).
        if let (Some(shock), Some(rng)) = (cfg.shocks.as_ref(), shock_rng.as_mut()) {
            let fire_roll = rng.next_u64();
            let shape_roll = rng.next_u64();
            if shock.fires(fire_roll) && pos_qty != Decimal::ZERO {
                let notional_abs = (pos_qty * price).abs();
                let shock_loss = notional_abs * shock.adverse_fraction(shape_roll);
                cash -= shock_loss;
            }
        }

        // (3) Mark equity and record the net-of-cost per-bar return.
        let equity = cash + pos_qty * price;
        if i > 0 {
            let r = if equity_prev.is_zero() {
                -1.0
            } else {
                (equity / equity_prev - Decimal::ONE)
                    .to_f64()
                    .unwrap_or(0.0)
            };
            returns.push(r);
        }
        equity_prev = equity;

        // (4) Decide for the next bar (fills at i+1). No same-bar fill ⇒ no look-ahead.
        let position = match (pos_qty, entry_bar) {
            (q, Some(j)) if q > Decimal::ZERO => {
                PositionState::held(Direction::Long, (i - j) as u16)
            }
            (q, Some(j)) if q < Decimal::ZERO => {
                PositionState::held(Direction::Short, (i - j) as u16)
            }
            _ => PositionState::flat(),
        };
        pending = match genome.decide(&bar.features, position) {
            Decision::Enter(dir) => Some(Pending::Enter(dir)),
            Decision::Exit => Some(Pending::Close),
            Decision::Hold => None,
        };
    }

    let net_pnl = equity_prev - Decimal::ONE;
    let accepted = trades >= cfg.min_trades && !returns.is_empty();
    let fitness = if accepted {
        NoiseRobustFitness::from_windows(&split_windows(&returns, cfg.windows))
    } else {
        // Rejected as noise: a non-finite mean can never become or displace an elite.
        NoiseRobustFitness {
            mean: f64::NEG_INFINITY,
            std_error: 0.0,
            n: returns.len(),
        }
    };

    let result = BacktestResult {
        returns,
        trades,
        net_pnl,
        funding: funding_accrued,
        accepted,
        fitness,
    };
    (result, fills)
}

/// Apply one fill to the cash/mark ledger: move cash by the signed notional and the (multiplied) costs,
/// and update the signed position. Costs reduce cash, so returns are net-of-cost.
fn apply_fill(
    cash: &mut Decimal,
    pos_qty: &mut Decimal,
    side: Side,
    qty: Decimal,
    price: Decimal,
    adv: Decimal,
    cfg: &BacktestConfig,
) {
    let notional_abs = (qty * price).abs();
    let fee = cfg.friction.fees.fee(notional_abs, Liquidity::Taker) * cfg.friction.cost_multiplier;
    let slip =
        cfg.friction.slippage.cost(notional_abs, qty.abs(), adv) * cfg.friction.cost_multiplier;
    let signed_qty = match side {
        Side::Buy => qty,
        Side::Sell => -qty,
    };
    *cash -= signed_qty * price;
    *cash -= fee + slip;
    *pos_qty += signed_qty;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitness::{should_replace, DEFAULT_K_SIGMA};
    use crate::friction::FrictionConfig;
    use crate::genome::{Clause, ExitParams, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION};
    use qe_signal::{CatalogueConfig, FeatureSchema, QState};

    fn schema() -> FeatureSchema {
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    /// A long-only genome: enter long when feature 0's state is high `[3,4]`; exit after `hold` bars.
    fn long_genome(hold: u16, size_bps: u16) -> Genome {
        let mut long = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        long[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 3,
            hi: 4,
        };
        let disabled = RuleSet {
            clauses: [Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            }; CLAUSES_PER_SET],
            min_satisfied: 1,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses: long,
                min_satisfied: 1,
            },
            short_entry: disabled,
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps },
        }
    }

    fn bar(schema: &FeatureSchema, time_ms: i64, price: Decimal, state0: u16) -> Bar {
        let mut states = vec![None; schema.len()];
        states[0] = Some(QState::from_index(state0));
        Bar {
            features: FeatureVector { time_ms, states },
            price,
            volume: Decimal::from(1000), // constant fixture volume ⇒ rolling ADV = 1000 contracts
            funding_rate: None,
        }
    }

    /// An up-trending series that makes the long genome trade repeatedly: feature 0 is "high" in bursts
    /// (triggering entries) and price drifts up so long trades profit gross.
    fn uptrend_bars(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                // High state every other pair of bars → an entry roughly every ~4 bars.
                let state0 = if (i / 2) % 2 == 0 { 4 } else { 0 };
                let price = Decimal::from(100 + i as i64); // +1 per bar
                bar(schema, i as i64 * 60_000, price, state0)
            })
            .collect()
    }

    #[test]
    fn fitness_is_net_of_cost() {
        let s = schema();
        let bars = uptrend_bars(&s, 160);
        let g = long_genome(2, 5_000);

        let cheap = BacktestConfig::default();
        let dear = BacktestConfig {
            friction: FrictionConfig::default().with_multiplier(Decimal::from(20)),
            ..BacktestConfig::default()
        };
        let lo = backtest(&g, &bars, &cheap);
        let hi = backtest(&g, &bars, &dear);

        assert!(
            lo.accepted && hi.accepted,
            "genome should clear the trade gate"
        );
        assert!(lo.trades >= cheap.min_trades);
        // `decide` is cost-blind, so the trade sequence is identical — the fitness drop is pure cost
        // drag, not fewer trades. Asserting equal trade counts makes the net-of-cost proof airtight.
        assert_eq!(
            lo.trades, hi.trades,
            "cost must not change the trade sequence"
        );
        // Higher costs strictly drag both fitness and net P&L.
        assert!(
            hi.fitness.mean < lo.fitness.mean,
            "cost should lower fitness: {} !< {}",
            hi.fitness.mean,
            lo.fitness.mean
        );
        assert!(hi.net_pnl < lo.net_pnl, "cost should lower net P&L");
    }

    #[test]
    fn size_impact_strictly_lowers_high_turnover_fitness() {
        // QE-403 AC: with size-impact > 0, a high-turnover / large-size genome's fitness STRICTLY drops
        // relative to impact == 0. `decide` is cost-blind, so the trade sequence is identical — the drop
        // is pure size-dependent slippage drag, not fewer trades.
        use crate::friction::SlippageModel;

        let s = schema();
        let bars = uptrend_bars(&s, 160);
        // Full-size (10 000 bps = 1×), short holding period ⇒ many round-trips ⇒ high turnover.
        let g = long_genome(1, 10_000);

        let no_impact = BacktestConfig {
            friction: FrictionConfig {
                slippage: SlippageModel {
                    impact_coeff: Decimal::ZERO,
                    ..SlippageModel::default()
                },
                ..FrictionConfig::default()
            },
            ..BacktestConfig::default()
        };
        // A deliberately visible participation coefficient so the drag is unambiguous (any coeff > 0 drops
        // fitness; a large one just makes the strict inequality robust to floating-point noise).
        let with_impact = BacktestConfig {
            friction: FrictionConfig {
                slippage: SlippageModel {
                    impact_coeff: Decimal::new(5, 1), // 0.5 impact fraction at 100% participation
                    ..SlippageModel::default()
                },
                ..FrictionConfig::default()
            },
            ..BacktestConfig::default()
        };

        let base = backtest(&g, &bars, &no_impact);
        let taxed = backtest(&g, &bars, &with_impact);

        assert!(
            base.accepted && taxed.accepted,
            "genome must clear the gate"
        );
        assert_eq!(
            base.trades, taxed.trades,
            "size-impact must not change the trade sequence (cost-blind decisions)"
        );
        assert!(
            taxed.fitness.mean < base.fitness.mean,
            "size-impact must strictly lower fitness: {} !< {}",
            taxed.fitness.mean,
            base.fitness.mean
        );
        assert!(
            taxed.net_pnl < base.net_pnl,
            "size-impact must strictly lower net P&L"
        );
    }

    #[test]
    fn under_trade_genome_is_rejected_as_noise() {
        let s = schema();
        let bars = uptrend_bars(&s, 160);
        // A genome whose entry band [1,2] is never the feature-0 state (which is only 0 or 4) → no trades.
        let mut never = long_genome(2, 5_000);
        never.long_entry.clauses[0].lo = 1;
        never.long_entry.clauses[0].hi = 2;

        let res = backtest(&never, &bars, &BacktestConfig::default());
        assert_eq!(res.trades, 0);
        assert!(!res.accepted);
        assert_eq!(res.fitness.mean, f64::NEG_INFINITY);

        // A rejected genome never displaces a finite incumbent.
        let incumbent = NoiseRobustFitness {
            mean: 0.01,
            std_error: 0.001,
            n: 4,
        };
        assert!(!should_replace(&incumbent, &res.fitness, DEFAULT_K_SIGMA));
    }

    #[test]
    fn profitable_uptrend_has_positive_fitness_and_ruin_is_absorbing() {
        let s = schema();
        let bars = uptrend_bars(&s, 160);
        let g = long_genome(2, 5_000);
        let res = backtest(&g, &bars, &BacktestConfig::default());
        assert!(res.accepted);
        assert!(
            res.fitness.mean > 0.0,
            "an up-trend long should profit net of cost"
        );
        assert!(res.net_pnl > Decimal::ZERO);

        // A ruinous crash bar (price → ~0 while long) drives fitness to −∞ (ruin is absorbing).
        let mut crash = uptrend_bars(&s, 40);
        // Force an entry then collapse the price the bar after the fill.
        for b in crash.iter_mut().take(4) {
            b.features.states[0] = Some(QState::from_index(4));
        }
        crash[6].price = Decimal::new(1, 2); // 0.01 — a near-total wipeout while holding long
        let g_hold = long_genome(20, 10_000); // hold through the crash at full 1x leverage
        let crashed = backtest(
            &g_hold,
            &crash,
            &BacktestConfig {
                min_trades: 1,
                windows: 2,
                ..BacktestConfig::default()
            },
        );
        assert_eq!(crashed.fitness.mean, f64::NEG_INFINITY);
    }

    #[test]
    fn replacement_respects_standard_error() {
        let s = schema();
        let bars = uptrend_bars(&s, 200);
        let g = long_genome(2, 5_000);
        let incumbent = backtest(&g, &bars, &BacktestConfig::default()).fitness;

        // The backtester produced a real noise estimate.
        assert_eq!(incumbent.n, DEFAULT_WINDOWS);
        assert!(incumbent.std_error > 0.0, "varying returns ⇒ a positive SE");

        // A challenger inside the SE band must NOT replace (no replace-on-noise).
        let noisy = NoiseRobustFitness {
            mean: incumbent.mean + 0.5 * incumbent.std_error,
            ..incumbent
        };
        assert!(!should_replace(&incumbent, &noisy, DEFAULT_K_SIGMA));

        // A challenger well outside the band DOES replace.
        let robust = NoiseRobustFitness {
            mean: incumbent.mean + 5.0 * incumbent.std_error + 0.01,
            ..incumbent
        };
        assert!(should_replace(&incumbent, &robust, DEFAULT_K_SIGMA));
    }

    /// A rising series engineered to fire the long genome's entry **exactly once**: feature-0 is high
    /// only on bar 0 (so only bar 0's decision enters), price drifts up, and the time-based exit closes
    /// the position a few bars later ⇒ exactly one closed, winning round-trip.
    fn single_entry_uptrend(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let state0 = if i == 0 { 4 } else { 0 };
                let price = Decimal::from(100 + i as i64); // +1 per bar → a long trade profits gross
                bar(schema, i as i64 * 60_000, price, state0)
            })
            .collect()
    }

    #[test]
    fn single_winning_round_trip_records_one_trade() {
        let s = schema();
        let bars = single_entry_uptrend(&s, 8);
        let g = long_genome(2, 5_000); // exit 2 bars after entry
        let cfg = BacktestConfig {
            min_trades: 1,
            windows: 2,
            ..BacktestConfig::default()
        };

        let (res, fills) = backtest_with_trades(&g, &bars, &cfg);

        // Exactly one entry ⇒ the aggregate counter and the recorded fills agree.
        assert_eq!(
            res.trades, 1,
            "the engineered series must enter exactly once"
        );
        assert_eq!(fills.len(), 1, "one closed round-trip ⇒ one TradeFill");

        let t = fills[0];
        assert_eq!(t.side, Direction::Long);
        assert!(t.entry_idx < t.exit_idx, "exit must be after entry");
        assert!(t.entry_px > Decimal::ZERO && t.exit_px > t.entry_px);
        assert!(
            t.return_frac > 0.0,
            "a rising-price long round-trip must have return_frac > 0, got {}",
            t.return_frac
        );
        // return_frac is the signed gross price return of the round-trip.
        let expected = ((t.exit_px - t.entry_px) / t.entry_px).to_f64().unwrap();
        assert!((t.return_frac - expected).abs() < 1e-12);
    }

    /// A short-only genome: enter short when feature 0's state is high `[3,4]`; exit after `hold` bars.
    /// The `short_entry` bank mirrors `long_genome`'s long bank, with the long bank disabled.
    fn short_genome(hold: u16, size_bps: u16) -> Genome {
        let mut short = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        short[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 3,
            hi: 4,
        };
        let disabled = RuleSet {
            clauses: [Clause {
                enabled: false,
                feature: 0,
                lo: 0,
                hi: 0,
            }; CLAUSES_PER_SET],
            min_satisfied: 1,
        };
        Genome {
            version: REP_VERSION,
            long_entry: disabled,
            short_entry: RuleSet {
                clauses: short,
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps },
        }
    }

    /// The falling-price mirror of [`single_entry_uptrend`]: feature-0 is high only on bar 0 (so only
    /// bar 0's decision enters) and price drifts **down**, so a single short round-trip profits gross.
    fn single_entry_downtrend(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let state0 = if i == 0 { 4 } else { 0 };
                let price = Decimal::from(200 - i as i64); // −1 per bar → a short trade profits gross
                bar(schema, i as i64 * 60_000, price, state0)
            })
            .collect()
    }

    #[test]
    fn single_winning_short_round_trip_records_one_trade() {
        let s = schema();
        let bars = single_entry_downtrend(&s, 8);
        let g = short_genome(2, 5_000); // exit 2 bars after entry
        let cfg = BacktestConfig {
            min_trades: 1,
            windows: 2,
            ..BacktestConfig::default()
        };

        let (res, fills) = backtest_with_trades(&g, &bars, &cfg);

        // Exactly one entry ⇒ the aggregate counter and the recorded fills agree.
        assert_eq!(
            res.trades, 1,
            "the engineered series must enter exactly once"
        );
        assert_eq!(fills.len(), 1, "one closed round-trip ⇒ one TradeFill");

        let t = fills[0];
        assert_eq!(t.side, Direction::Short);
        assert!(t.entry_idx < t.exit_idx, "exit must be after entry");
        // Falling price: the short exits below where it entered.
        assert!(t.exit_px > Decimal::ZERO && t.exit_px < t.entry_px);
        assert!(
            t.return_frac > 0.0,
            "a short on a falling series is a winner (return_frac > 0), got {}",
            t.return_frac
        );
        // return_frac is the signed gross price return of the short round-trip: (entry − exit) / entry.
        let expected = ((t.entry_px - t.exit_px) / t.entry_px).to_f64().unwrap();
        assert!((t.return_frac - expected).abs() < 1e-12);
    }

    #[test]
    fn backtest_delegates_to_with_trades() {
        let s = schema();
        let bars = uptrend_bars(&s, 160);
        let g = long_genome(2, 5_000);
        let cfg = BacktestConfig::default();
        // `backtest` must be exactly the trade-discarding projection of `backtest_with_trades` — the
        // hot-path result is unchanged by construction.
        assert_eq!(
            backtest(&g, &bars, &cfg),
            backtest_with_trades(&g, &bars, &cfg).0
        );
    }

    #[test]
    fn open_position_at_end_records_no_trade() {
        let s = schema();
        // Enter once and never exit (huge holding cap, series ends while still long).
        let bars = single_entry_uptrend(&s, 6);
        let g = long_genome(1_000, 5_000);
        let cfg = BacktestConfig {
            min_trades: 1,
            windows: 2,
            ..BacktestConfig::default()
        };
        let (res, fills) = backtest_with_trades(&g, &bars, &cfg);
        assert_eq!(res.trades, 1, "one entry fill");
        assert!(
            fills.is_empty(),
            "an unclosed position is not a completed round-trip"
        );
    }

    // --- QE-441 bar-level tail-aware scenario shocks ------------------------------------------------
    use crate::fitness::log_growth;
    use qe_risk::ShockConfig;

    /// A flat-price series that fires the long genome's entry once (feature-0 high only at bar 0) and
    /// holds through the rest, so the only P&L is entry cost + injected shocks (no price drift).
    fn flat_hold_bars(schema: &FeatureSchema, n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let state0 = if i == 0 { 4 } else { 0 };
                bar(schema, i as i64 * 60_000, Decimal::from(100), state0)
            })
            .collect()
    }

    /// The worst peak-to-trough drawdown of an equity curve built from per-bar returns (a positive
    /// fraction). Local test helper (the wfo crate has no drawdown util).
    fn max_drawdown(returns: &[f64]) -> f64 {
        let (mut equity, mut peak, mut worst) = (1.0_f64, 1.0_f64, 0.0_f64);
        for &r in returns {
            equity *= 1.0 + r;
            peak = peak.max(equity);
            let dd = if peak > 0.0 { 1.0 - equity / peak } else { 1.0 };
            worst = worst.max(dd);
        }
        worst
    }

    fn shocks(seed: u64, freq: u32, gap: Decimal, fund: Decimal, adl: Decimal) -> ShockConfig {
        ShockConfig::new(seed, freq, gap, fund, 8, adl)
    }

    fn shock_cfg(shocks: Option<ShockConfig>) -> BacktestConfig {
        BacktestConfig {
            min_trades: 1,
            windows: 1,
            shocks,
            ..BacktestConfig::default()
        }
    }

    /// AC: a **larger size** produces a **strictly deeper shocked drawdown** (same window + seed). On a
    /// flat series the only loss is the injected shock, which scales with the held notional (= size), so a
    /// bigger `size_bps` loses strictly more.
    #[test]
    fn larger_size_produces_a_deeper_shocked_drawdown() {
        let s = schema();
        let bars = flat_hold_bars(&s, 60);
        let cfg = shock_cfg(Some(shocks(
            7,
            150_000,
            Decimal::new(10, 2), // 0.10 gap
            Decimal::new(5, 3),  // 0.005 funding/period
            Decimal::new(5, 2),  // 0.05 adl
        )));

        let small = backtest(&long_genome(1_000, 1_000), &bars, &cfg); // 0.1×
        let large = backtest(&long_genome(1_000, 8_000), &bars, &cfg); // 0.8×

        assert!(small.accepted && large.accepted, "both must trade");
        // Same shock bars (position-independent schedule); the larger position takes the bigger hit.
        assert!(
            max_drawdown(&large.returns) > max_drawdown(&small.returns),
            "larger size must deepen the shocked drawdown: large {} !> small {}",
            max_drawdown(&large.returns),
            max_drawdown(&small.returns)
        );
        assert!(
            large.net_pnl < small.net_pnl,
            "larger size must lose strictly more to shocks"
        );
    }

    /// The `size_bps` that maximises `log_growth` over a sweep, for the given shock config.
    fn argmax_size(bars: &[Bar], sizes: &[u16], shocks: Option<ShockConfig>) -> u16 {
        let cfg = shock_cfg(shocks);
        sizes
            .iter()
            .copied()
            .max_by(|&a, &b| {
                let fa = log_growth(&backtest(&long_genome(1_000, a), bars, &cfg).returns);
                let fb = log_growth(&backtest(&long_genome(1_000, b), bars, &cfg).returns);
                fa.total_cmp(&fb)
            })
            .unwrap()
    }

    /// AC: the fitness-maximising `size_bps` is **strictly lower with shocks than without** — the
    /// tail-aware Kelly pull-down. Construction: a pure uptrend (no down bars ⇒ without shocks, more size
    /// is always better ⇒ the optimum sits at the top of the sweep); injecting bar-level shocks adds
    /// size-scaled losses that create an interior optimum below the top.
    #[test]
    fn tail_aware_shocks_pull_the_optimal_size_below_the_no_shock_optimum() {
        let s = schema();
        let bars = single_entry_uptrend(&s, 60); // +1/bar, enter once, hold
        let sizes = [500u16, 1_000, 2_000, 4_000, 8_000];

        let opt_none = argmax_size(&bars, &sizes, None);
        let opt_shock = argmax_size(
            &bars,
            &sizes,
            Some(shocks(
                42,
                150_000,
                Decimal::new(20, 2), // 0.20 gap
                Decimal::new(10, 3), // 0.010 funding/period
                Decimal::new(10, 2), // 0.10 adl
            )),
        );

        assert_eq!(
            opt_none,
            *sizes.last().unwrap(),
            "on a pure uptrend the no-shock optimum must be the largest size"
        );
        assert!(
            opt_shock < opt_none,
            "tail-aware shocks must pull the optimal size down: shock {opt_shock} !< none {opt_none}"
        );
    }

    /// AC: the pull-down is **monotone in severity** — a heavier shock set (deeper magnitudes, SAME seed +
    /// frequency ⇒ identical shock bars) pulls the optimum at-or-below a milder one, and both at-or-below
    /// the no-shock optimum.
    #[test]
    fn optimal_size_is_monotone_non_increasing_in_shock_severity() {
        let s = schema();
        let bars = single_entry_uptrend(&s, 60);
        let sizes = [500u16, 1_000, 2_000, 4_000, 8_000];

        let opt_none = argmax_size(&bars, &sizes, None);
        // Mild and severe share seed + frequency, so the SAME bars are shocked — only the depth differs.
        let opt_mild = argmax_size(
            &bars,
            &sizes,
            Some(shocks(
                42,
                150_000,
                Decimal::new(5, 2),  // 0.05 gap
                Decimal::new(25, 4), // 0.0025 funding/period
                Decimal::new(25, 3), // 0.025 adl
            )),
        );
        let opt_severe = argmax_size(
            &bars,
            &sizes,
            Some(shocks(
                42,
                150_000,
                Decimal::new(20, 2), // 0.20 gap
                Decimal::new(10, 3), // 0.010 funding/period
                Decimal::new(10, 2), // 0.10 adl
            )),
        );

        assert!(
            opt_mild <= opt_none && opt_severe <= opt_mild,
            "optimum must be monotone non-increasing in severity: none {opt_none} ≥ mild {opt_mild} ≥ severe {opt_severe}"
        );
        assert!(
            opt_severe < opt_none,
            "a fat-tailed shock set must pull the optimum strictly below the no-shock optimum"
        );
    }

    /// AC: shocks are **seeded / reproducible** — same config ⇒ identical returns; a different
    /// `ShockConfig::seed` ⇒ different shock bars ⇒ different returns; and `shocks: None` reproduces the
    /// pre-QE-441 raw-historical path byte-for-byte.
    #[test]
    fn shocks_are_seeded_reproducible_and_off_by_default() {
        let s = schema();
        let bars = single_entry_uptrend(&s, 60);
        let g = long_genome(1_000, 8_000);

        let a = shock_cfg(Some(shocks(
            99,
            150_000,
            Decimal::new(10, 2),
            Decimal::new(5, 3),
            Decimal::new(5, 2),
        )));
        // Same seed ⇒ byte-identical result (pure function of genome/bars/cfg).
        assert_eq!(backtest(&g, &bars, &a), backtest(&g, &bars, &a));

        // A different seed shocks different bars ⇒ a different net path.
        let b = shock_cfg(Some(shocks(
            100,
            150_000,
            Decimal::new(10, 2),
            Decimal::new(5, 3),
            Decimal::new(5, 2),
        )));
        assert_ne!(
            backtest(&g, &bars, &a).returns,
            backtest(&g, &bars, &b).returns,
            "a different shock seed must change the shocked path"
        );

        // `shocks: None` (the default) is the untouched historical path — strictly better than any shocked
        // run here (shocks only ever subtract), and unaffected by the shock seed.
        let none = shock_cfg(None);
        assert!(
            backtest(&g, &bars, &none).net_pnl > backtest(&g, &bars, &a).net_pnl,
            "the no-shock path must not be dragged by shocks"
        );
    }

    #[test]
    fn backtest_is_pure_and_has_no_same_bar_fill() {
        let s = schema();
        let bars = uptrend_bars(&s, 120);
        let g = long_genome(2, 5_000);
        let cfg = BacktestConfig::default();
        // Pure function of (genome, bars, cfg).
        assert_eq!(backtest(&g, &bars, &cfg), backtest(&g, &bars, &cfg));

        // No look-ahead: a one-bar series can never fill (the decision needs a *next* bar), so a genome
        // that would enter immediately still books zero trades.
        let one = vec![bar(&s, 0, Decimal::from(100), 4)];
        let res = backtest(
            &g,
            &one,
            &BacktestConfig {
                min_trades: 1,
                ..cfg
            },
        );
        assert_eq!(res.trades, 0);
    }
}
