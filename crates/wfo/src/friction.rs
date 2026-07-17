//! Execution-friction & funding model (QE-109) — backtest realism for linear perps.
//!
//! Fees and funding are first-order P&L on USDT-M perps; a frictionless backtest biases the archive
//! toward fee-losing high-turnover and net-negative-after-funding trend strategies. This module is
//! the **configurable cost primitive** the backtester (QE-120) and the validation report (QE-133)
//! drive: a signed, average-cost position walked over a fill/funding event stream, returning a
//! **decomposed** `gross / fees / slippage / funding` P&L. All money is exact `rust_decimal`.

use rust_decimal::{Decimal, MathematicalOps};

use qe_domain::Side;
use qe_risk::SlippageCalibration;

/// Whether a fill took or made liquidity (selects the fee rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liquidity {
    /// Crossed the spread (taker).
    Taker,
    /// Rested and was hit (maker).
    Maker,
}

/// Taker/maker fee rates as fractions of notional. Default = Binance USDT-M **VIP0**
/// (taker `0.05%`, maker `0.02%`); a tier is just a different schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSchedule {
    /// Taker rate (fraction of notional).
    pub taker: Decimal,
    /// Maker rate (fraction of notional).
    pub maker: Decimal,
}

impl Default for FeeSchedule {
    fn default() -> Self {
        FeeSchedule {
            taker: Decimal::new(5, 4), // 0.0005 = 0.05%
            maker: Decimal::new(2, 4), // 0.0002 = 0.02%
        }
    }
}

impl FeeSchedule {
    /// The rate for a liquidity role.
    #[must_use]
    pub fn rate(&self, liquidity: Liquidity) -> Decimal {
        match liquidity {
            Liquidity::Taker => self.taker,
            Liquidity::Maker => self.maker,
        }
    }

    /// Fee on `notional_abs` (already non-negative) at the role's rate.
    #[must_use]
    pub fn fee(&self, notional_abs: Decimal, liquidity: Liquidity) -> Decimal {
        notional_abs * self.rate(liquidity)
    }
}

/// Spread-cross + **concave (√-in-participation)** size impact (QE-440). Cost on a fill is
/// `notional_abs · (half_spread + impact_coeff · (qty_abs/adv)^β)`, where `adv` is the rolling ADV (in
/// the same contract unit as `qty`) so participation `u = qty/adv` is dimensionless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlippageModel {
    /// Half the bid/ask spread, as a fraction of price (the spread-cross term).
    pub half_spread: Decimal,
    /// Participation impact coefficient — the impact fraction of notional at `u = 1` (100 % of ADV).
    /// Dimensionless and asset-portable (shared verbatim with capacity via the calibration).
    pub impact_coeff: Decimal,
    /// Impact exponent β — the concavity of impact in participation (`u^β`, `β < 1`).
    pub impact_exponent: Decimal,
}

impl Default for SlippageModel {
    fn default() -> Self {
        // QE-431/QE-440: the default is **derived** from the one content-addressed [`SlippageCalibration`],
        // not authored here — so no magic slippage/impact literal remains on the selection path (the train
        // search runs on `BacktestConfig::default().friction`) and friction can never drift from the
        // capacity side, which derives the **same** participation-keyed coefficient from the same
        // calibration.
        SlippageModel::from_calibration(&SlippageCalibration::default())
    }
}

impl SlippageModel {
    /// Derive the friction slippage model from the shared [`SlippageCalibration`] (QE-431/QE-440): the
    /// `half_spread`, participation `impact_coeff`, and exponent β are all taken verbatim — no
    /// per-contract conversion (participation is dimensionless), so friction and capacity key impact off
    /// the identical coefficient.
    #[must_use]
    pub fn from_calibration(cal: &SlippageCalibration) -> Self {
        SlippageModel {
            half_spread: cal.half_spread,
            impact_coeff: cal.impact_coeff,
            impact_exponent: cal.impact_exponent,
        }
    }

    /// Slippage cost for a fill of `qty_abs` (notional `notional_abs`) against rolling ADV `adv` (in the
    /// same contract unit as `qty`). The size term `impact_coeff · (qty/adv)^β` is **concave** in size
    /// (`β < 1`): doubling `qty` at fixed `adv` multiplies the impact fraction by `2^β < 2`. A
    /// non-positive `adv` charges the spread-cross only (participation is undefined without an ADV).
    ///
    /// Deterministic across platforms — `(qty/adv)^β` is `rust_decimal`'s pure-Decimal `powd` (no
    /// hardware `f64`), safe for the sealed/hashed money ledger.
    #[must_use]
    pub fn cost(&self, notional_abs: Decimal, qty_abs: Decimal, adv: Decimal) -> Decimal {
        let participation = if adv > Decimal::ZERO {
            qty_abs / adv
        } else {
            Decimal::ZERO
        };
        let impact = if participation > Decimal::ZERO {
            self.impact_coeff * participation.powd(self.impact_exponent)
        } else {
            Decimal::ZERO
        };
        notional_abs * (self.half_spread + impact)
    }
}

/// The full friction configuration. `cost_multiplier` scales the **assumed** costs (fees + slippage)
/// for the sensitivity sweep — it does **not** touch funding, a realised market cashflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrictionConfig {
    /// Fee schedule.
    pub fees: FeeSchedule,
    /// Slippage model.
    pub slippage: SlippageModel,
    /// Multiplier on fees + slippage (1 = as-modelled).
    pub cost_multiplier: Decimal,
}

impl Default for FrictionConfig {
    fn default() -> Self {
        FrictionConfig {
            fees: FeeSchedule::default(),
            slippage: SlippageModel::default(),
            cost_multiplier: Decimal::ONE,
        }
    }
}

impl FrictionConfig {
    /// A copy with the cost multiplier replaced (used by [`cost_sweep`]).
    #[must_use]
    pub fn with_multiplier(self, cost_multiplier: Decimal) -> Self {
        FrictionConfig {
            cost_multiplier,
            ..self
        }
    }
}

/// A fill, already stamped at the **next bar open** (the fill convention is the caller's; QE-120
/// supplies next-bar-open prices). `qty` is strictly positive; `side` gives the direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// Buy (long) or sell (short).
    pub side: Side,
    /// Filled quantity (> 0).
    pub qty: Decimal,
    /// Fill price.
    pub price: Decimal,
    /// Rolling ADV at the fill (same contract unit as `qty`), keying the participation impact (QE-440).
    /// A non-positive value charges the spread-cross only.
    pub adv: Decimal,
    /// Whether it took or made liquidity.
    pub liquidity: Liquidity,
}

/// A funding stamp (every 8h on Binance USDT-M): the **historical** rate and the mark to value the
/// held position. Cashflow to the trader is `−signed_qty · mark_price · rate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingStamp {
    /// The historical funding rate (signed fraction).
    pub rate: Decimal,
    /// Mark price used to value the position at the stamp.
    pub mark_price: Decimal,
}

/// One event in a backtest stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A fill.
    Fill(Fill),
    /// A funding accrual against the held position.
    Funding(FundingStamp),
}

/// A signed, average-cost position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Position {
    /// Signed quantity (positive = long, negative = short).
    pub qty: Decimal,
    /// Average entry price of the open quantity (0 when flat).
    pub avg_price: Decimal,
}

impl Position {
    /// Apply a fill, returning the **realised gross P&L** from any quantity it closed.
    ///
    /// Average-cost convention: adding in the same direction updates the average; reducing realises
    /// P&L on the closed quantity; a flip realises the whole existing position then reopens the
    /// remainder at the fill price.
    pub fn apply(&mut self, side: Side, qty: Decimal, price: Decimal) -> Decimal {
        let signed = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };

        // Opening from flat, or adding in the same direction → update the weighted average.
        if self.qty.is_zero() || self.qty.is_sign_positive() == signed.is_sign_positive() {
            let new_qty = self.qty + signed;
            self.avg_price = (self.avg_price * self.qty.abs() + price * qty) / new_qty.abs();
            self.qty = new_qty;
            return Decimal::ZERO;
        }

        // Opposite direction → realise on the closed portion.
        let closing = qty.min(self.qty.abs());
        let dir = if self.qty.is_sign_positive() {
            Decimal::ONE
        } else {
            -Decimal::ONE
        };
        let realized = dir * (price - self.avg_price) * closing;

        let remaining = self.qty + signed;
        if remaining.is_zero() {
            self.qty = Decimal::ZERO;
            self.avg_price = Decimal::ZERO;
        } else if remaining.is_sign_positive() == self.qty.is_sign_positive() {
            // Partial reduce — average unchanged.
            self.qty = remaining;
        } else {
            // Flip — reopen the remainder at the fill price.
            self.qty = remaining;
            self.avg_price = price;
        }
        realized
    }
}

/// A decomposed backtest P&L. `net = gross − fees − slippage + funding` (funding is a signed
/// cashflow to the trader: negative when paid).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PnlBreakdown {
    /// Realised gross trading P&L (before costs).
    pub gross: Decimal,
    /// Total fees paid (≥ 0).
    pub fees: Decimal,
    /// Total slippage cost (≥ 0).
    pub slippage: Decimal,
    /// Net funding cashflow to the trader (signed).
    pub funding: Decimal,
}

impl PnlBreakdown {
    /// Net P&L after costs and funding.
    #[must_use]
    pub fn net(&self) -> Decimal {
        self.gross - self.fees - self.slippage + self.funding
    }
}

/// Walk the event stream with `cfg`, returning the decomposed P&L (net of fees, slippage, and
/// funding from the actual stamps).
#[must_use]
pub fn simulate(events: &[Event], cfg: &FrictionConfig) -> PnlBreakdown {
    let mut pos = Position::default();
    let mut pnl = PnlBreakdown::default();

    for event in events {
        match event {
            Event::Fill(f) => {
                let notional_abs = (f.qty * f.price).abs();
                pnl.fees += cfg.fees.fee(notional_abs, f.liquidity) * cfg.cost_multiplier;
                pnl.slippage +=
                    cfg.slippage.cost(notional_abs, f.qty.abs(), f.adv) * cfg.cost_multiplier;
                pnl.gross += pos.apply(f.side, f.qty, f.price);
            }
            Event::Funding(s) => {
                // Cashflow to the trader: longs pay shorts when rate > 0.
                pnl.funding += -pos.qty * s.mark_price * s.rate;
            }
        }
    }
    pnl
}

/// Run `simulate` at each absolute `cost_multiplier` in `multipliers` (e.g. `[1, 2]`), returning
/// `(multiplier, breakdown)` pairs — the cost-sensitivity sweep for the QE-133 report. Only fees +
/// slippage scale; gross and funding are unchanged.
#[must_use]
pub fn cost_sweep(
    events: &[Event],
    base: &FrictionConfig,
    multipliers: &[Decimal],
) -> Vec<(Decimal, PnlBreakdown)> {
    multipliers
        .iter()
        .map(|&m| (m, simulate(events, &base.with_multiplier(m))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    // ADV keeping every fixture order at 1 % participation (`u = 1/100`), so `impact = 0.01·√0.01 =
    // 0.001` — a clean, concave size term.
    const ADV: &str = "100";
    fn buy(qty: &str, price: &str) -> Event {
        Event::Fill(Fill {
            side: Side::Buy,
            qty: d(qty),
            price: d(price),
            adv: d(ADV),
            liquidity: Liquidity::Taker,
        })
    }
    fn sell(qty: &str, price: &str) -> Event {
        Event::Fill(Fill {
            side: Side::Sell,
            qty: d(qty),
            price: d(price),
            adv: d(ADV),
            liquidity: Liquidity::Taker,
        })
    }
    fn funding(rate: &str, mark: &str) -> Event {
        Event::Funding(FundingStamp {
            rate: d(rate),
            mark_price: d(mark),
        })
    }

    #[test]
    fn defaults_are_vip0() {
        let f = FeeSchedule::default();
        assert_eq!(f.taker, d("0.0005"));
        assert_eq!(f.maker, d("0.0002"));
    }

    #[test]
    fn ac1_turnover_one_shows_fee_drag() {
        // Buy 1 @100, sell 1 @100 — flat price, so gross == 0 but two taker fees drag net negative.
        let events = vec![buy("1", "100"), sell("1", "100")];
        let pnl = simulate(&events, &FrictionConfig::default());
        assert_eq!(pnl.gross, Decimal::ZERO);
        // fees = 2 × (100 × 0.0005) = 0.10;
        // participation u = qty/adv = 1/100 = 0.01 ⇒ impact = 0.01·√0.01 = 0.001;
        // slippage = 2 × (100 × (0.0001 half-spread + 0.001 impact)) = 2 × (100 × 0.0011) = 0.22.
        assert_eq!(pnl.fees, d("0.10"));
        assert_eq!(pnl.slippage, d("0.22"));
        assert!(pnl.net() < Decimal::ZERO);
        assert_eq!(pnl.net(), d("-0.32"));
    }

    #[test]
    fn ac2_funding_sign_is_correct_for_direction() {
        // Long held through a positive funding stamp → pays funding (negative cashflow).
        let long = vec![buy("1", "100"), funding("0.0001", "100")];
        let pnl = simulate(&long, &FrictionConfig::default());
        assert_eq!(pnl.funding, d("-0.01")); // -(+1) × 100 × 0.0001
        assert!(pnl.funding < Decimal::ZERO);

        // Short through the same stamp → receives funding (positive).
        let short = vec![sell("1", "100"), funding("0.0001", "100")];
        assert!(simulate(&short, &FrictionConfig::default()).funding > Decimal::ZERO);

        // Negative rate flips the long to a receipt.
        let long_neg = vec![buy("1", "100"), funding("-0.0001", "100")];
        assert!(simulate(&long_neg, &FrictionConfig::default()).funding > Decimal::ZERO);

        // Flat at the stamp → no funding.
        let flat = vec![buy("1", "100"), sell("1", "100"), funding("0.0001", "100")];
        assert_eq!(
            simulate(&flat, &FrictionConfig::default()).funding,
            Decimal::ZERO
        );
    }

    #[test]
    fn ac3_cost_sweep_scales_assumed_costs_only() {
        let events = vec![buy("1", "100"), funding("0.0001", "100"), sell("1", "110")];
        let sweep = cost_sweep(&events, &FrictionConfig::default(), &[d("1"), d("2")]);
        let (m1, p1) = sweep[0];
        let (m2, p2) = sweep[1];
        assert_eq!(m1, d("1"));
        assert_eq!(m2, d("2"));
        // Gross and funding unchanged; fees + slippage exactly double.
        assert_eq!(p1.gross, p2.gross);
        assert_eq!(p1.funding, p2.funding);
        assert_eq!(p2.fees, p1.fees * d("2"));
        assert_eq!(p2.slippage, p1.slippage * d("2"));
        assert!(p2.net() < p1.net());
    }

    #[test]
    fn position_realises_average_cost_pnl() {
        let mut p = Position::default();
        // Long 2 @100 then add 2 @110 → avg 105, qty 4.
        assert_eq!(p.apply(Side::Buy, d("2"), d("100")), Decimal::ZERO);
        assert_eq!(p.apply(Side::Buy, d("2"), d("110")), Decimal::ZERO);
        assert_eq!(p.qty, d("4"));
        assert_eq!(p.avg_price, d("105"));
        // Sell 1 @115 → realise (115-105)×1 = 10; qty 3, avg unchanged.
        assert_eq!(p.apply(Side::Sell, d("1"), d("115")), d("10"));
        assert_eq!(p.qty, d("3"));
        assert_eq!(p.avg_price, d("105"));
        // Sell 5 @120 → close 3 @ (120-105)×3 = 45, flip to short 2 @120.
        assert_eq!(p.apply(Side::Sell, d("5"), d("120")), d("45"));
        assert_eq!(p.qty, d("-2"));
        assert_eq!(p.avg_price, d("120"));
        // Buy 2 @115 closes the short → (120-115)×2 = 10.
        assert_eq!(p.apply(Side::Buy, d("2"), d("115")), d("10"));
        assert_eq!(p.qty, Decimal::ZERO);
    }

    #[test]
    fn maker_is_cheaper_than_taker() {
        let f = FeeSchedule::default();
        assert!(f.fee(d("100"), Liquidity::Maker) < f.fee(d("100"), Liquidity::Taker));
    }

    #[test]
    fn default_is_derived_from_the_shared_calibration_no_magic_literal() {
        // QE-431/QE-440 AC: the selection-path friction model authors no slippage/impact literal — it is
        // exactly the one derived from `SlippageCalibration::default()` (the single source of truth), and
        // it keeps the calibration's participation-keyed coefficient verbatim (no per-contract conversion).
        let cal = SlippageCalibration::default();
        assert_eq!(
            SlippageModel::default(),
            SlippageModel::from_calibration(&cal)
        );
        assert_eq!(SlippageModel::default().half_spread, cal.half_spread);
        assert_eq!(SlippageModel::default().impact_coeff, cal.impact_coeff);
        assert_eq!(
            SlippageModel::default().impact_exponent,
            cal.impact_exponent
        );
    }

    #[test]
    fn cost_is_concave_in_participation_and_reduces_without_adv() {
        // QE-440: at fixed ADV, doubling qty multiplies the *impact* term by 2^β < 2 (concave), unlike the
        // old linear-in-qty term that doubled it. Compare against the spread-only baseline to isolate impact.
        let m = SlippageModel::default();
        let adv = d("1000");
        let spread_only = |notional: Decimal| notional * m.half_spread;
        let q = d("10");
        let n1 = q * d("100");
        let n2 = (q * d("2")) * d("100");
        let impact1 = m.cost(n1, q, adv) - spread_only(n1);
        // Normalise out the notional (which itself doubles) to compare the impact *fraction*.
        let frac1 = impact1 / n1;
        let frac2 = (m.cost(n2, q * d("2"), adv) - spread_only(n2)) / n2;
        assert!(frac2 > frac1, "impact fraction must rise with size");
        let ratio = (frac2 / frac1).round_dp(6);
        assert!(
            ratio < d("2"),
            "concave: doubling qty raises impact fraction by < 2×, got {ratio}"
        );
        // No ADV ⇒ spread-cross only (participation undefined).
        assert_eq!(m.cost(n1, q, Decimal::ZERO), spread_only(n1));
    }
}
