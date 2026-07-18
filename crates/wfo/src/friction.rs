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
    /// Crossed the spread (taker). **The only role any current code path selects** — the engine is a
    /// pure taker (no `post_only`/`OrderType`/limit-order machinery in `edge` or `hedger`).
    Taker,
    /// Rested and was hit (maker). **Unused on the fill path today** — see the [`FeeSchedule`]
    /// adverse-selection invariant before ever selecting this role.
    Maker,
}

/// Taker/maker fee rates as fractions of notional. Default = Binance USDT-M **VIP0**
/// (taker `0.05%`, maker `0.02%`); a tier is just a different schedule.
///
/// # ⚠️ Adverse-selection invariant (QE-449, maxdama §7.6) — the maker rate is a **latent trap**
///
/// The engine is a confirmed **pure taker** today: no `post_only`/`OrderType`/limit-order machinery
/// exists in `edge` or `hedger`, and the backtest/selection path charges [`Liquidity::Taker`]
/// unconditionally (`backtest.rs` `apply_fill`). The `maker` rate below therefore prices **no fill**
/// — it reads as a *free rebate*, and that is the trap.
///
/// **The `maker` rate MUST NOT be used to fill orders without a paired adverse-selection markout.**
/// A resting (maker) fill is systematically *selected against*: it executes precisely when the market
/// is about to move through the resting price, so conditional on a fill the short-horizon mark drift
/// is **adverse in expectation**. If `post_only`/maker fills are ever added to harvest the
/// maker/taker gap, they **MUST** be accompanied by a modelled expected fill-conditional adverse
/// drift (the QE-444 [`SlippageModel::alpha_loss`] directional term is the natural home). Collecting
/// the spread/rebate *without* charging that adverse markout **overstates PnL and inflates a Sharpe
/// that DSR cannot deflate** — the bias is systematic (per-fill), not selection noise, so the
/// absolute-vs-noise-ceiling DSR/PBO/SPA apparatus passes it through undeflated.
///
/// The `apply_fill_charges_the_taker_rate_not_the_maker_rate` test (in `backtest.rs`) guards this on
/// the **production** fill path: it drives a real `backtest()` and asserts the charged fee equals the
/// **taker** rate and never the maker rate, so a future change that starts selecting
/// [`Liquidity::Maker`] in `apply_fill` is forced to trip it and consciously address the markout.
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
    /// Decision-to-fill **alpha-loss** (implementation-shortfall) coefficient (QE-444, maxdama §7.3): the
    /// adverse close→open directional drift charged **in the trade direction** as a fraction of notional,
    /// on top of the symmetric `half_spread`. Derived verbatim from the shared [`SlippageCalibration`].
    /// Default **`0`** (measurement-deferred, QE-435) ⇒ the term is inert and the ledger is byte-identical
    /// to pre-QE-444. See [`SlippageModel::alpha_loss_cost`] / [`SlippageModel::directional_drift`].
    pub alpha_loss: Decimal,
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
            alpha_loss: cal.alpha_loss,
        }
    }

    /// The **signed** per-notional decision-to-fill drift for a fill of `side` (QE-444): `+alpha_loss` for a
    /// **buy** (fill drifted up from the decision price), `−alpha_loss` for a **sell** (drifted down) — an
    /// **odd** function of side, the directional signature that sets alpha-loss apart from the side-blind
    /// (even) `half_spread`.
    #[must_use]
    pub fn directional_drift(&self, side: Side) -> Decimal {
        match side {
            Side::Buy => self.alpha_loss,
            Side::Sell => -self.alpha_loss,
        }
    }

    /// The **adverse** decision-to-fill alpha-loss cost on a fill of `notional_abs` (QE-444): the drift is
    /// signal-aligned (adverse to whichever way the trade points), so the magnitude is
    /// `notional_abs · alpha_loss` in the trade's own direction and always **reduces** net return. Kept
    /// **separate** from [`SlippageModel::cost`] (the symmetric spread + impact) so the directional term is
    /// explicit and never confused with the half-spread. Exact `Decimal`.
    #[must_use]
    pub fn alpha_loss_cost(&self, notional_abs: Decimal) -> Decimal {
        notional_abs * self.alpha_loss
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
                // Symmetric spread + participation impact, plus the QE-444 DIRECTIONAL decision-to-fill
                // alpha-loss (adverse in the trade's direction; inert at the default coefficient 0).
                pnl.slippage += (cfg.slippage.cost(notional_abs, f.qty.abs(), f.adv)
                    + cfg.slippage.alpha_loss_cost(notional_abs))
                    * cfg.cost_multiplier;
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
    fn simulate_over_taker_fills_charges_the_taker_rate_only() {
        // QE-449 latent-trap guard (maxdama §7.6), `simulate` level. This covers the `simulate` event
        // walker (a dev/test helper — the production selection path is `backtest.rs::apply_fill`, guarded
        // separately by `apply_fill_charges_the_taker_rate_not_the_maker_rate`). Every fixture fill carries
        // `Liquidity::Taker`, mirroring `apply_fill`'s unconditional taker role, and no code path selects
        // `Liquidity::Maker` (grep-confirmed: only the enum arm + `maker_is_cheaper_than_taker` reference
        // it). Asserts the fees are the **taker** fees and never the maker fees. See the `FeeSchedule`
        // adverse-selection invariant.
        let events = vec![buy("1", "100"), sell("1", "100")];
        let cfg = FrictionConfig::default();
        let pnl = simulate(&events, &cfg);

        // Two taker fills of notional 100 ⇒ fees = 2 · (100 · taker).
        let taker_fees = d("2") * (d("100") * cfg.fees.taker);
        let maker_fees = d("2") * (d("100") * cfg.fees.maker);
        assert_eq!(
            pnl.fees, taker_fees,
            "backtest path must charge the taker rate"
        );
        assert_ne!(
            pnl.fees, maker_fees,
            "guard is non-vacuous: maker and taker rates differ"
        );

        // And the fixtures themselves only ever carry the taker role — mirroring the backtest ledger,
        // which hardcodes `Liquidity::Taker` in `apply_fill`.
        for ev in &events {
            if let Event::Fill(f) = ev {
                assert_eq!(
                    f.liquidity,
                    Liquidity::Taker,
                    "no current fill selects Liquidity::Maker"
                );
            }
        }
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

    // --- QE-444 decision-to-fill alpha-loss (implementation shortfall) ------------------------------

    #[test]
    fn from_calibration_derives_alpha_loss_and_default_is_inert() {
        // The friction model reads the directional coefficient verbatim from the shared calibration; the
        // default is 0 (measurement-deferred), so the term contributes nothing.
        let cal = SlippageCalibration::default();
        assert_eq!(
            SlippageModel::from_calibration(&cal).alpha_loss,
            cal.alpha_loss
        );
        assert_eq!(SlippageModel::default().alpha_loss, Decimal::ZERO);
        assert_eq!(
            SlippageModel::default().alpha_loss_cost(d("1000")),
            Decimal::ZERO
        );

        let fitted = cal.with_alpha_loss(d("0.002"));
        assert_eq!(
            SlippageModel::from_calibration(&fitted).alpha_loss,
            d("0.002")
        );
    }

    #[test]
    fn alpha_loss_is_directional_odd_in_side_unlike_half_spread() {
        // QE-444: directional drift is +γ for a buy, −γ for a sell (odd in side) — the asymmetry that
        // distinguishes it from the side-blind (even) half_spread. The adverse cost magnitude is symmetric.
        let m = SlippageModel::from_calibration(
            &SlippageCalibration::default().with_alpha_loss(d("0.001")),
        );
        assert_eq!(m.directional_drift(Side::Buy), d("0.001"));
        assert_eq!(m.directional_drift(Side::Sell), d("-0.001"));
        assert_eq!(
            m.directional_drift(Side::Buy),
            -m.directional_drift(Side::Sell)
        );
        assert_eq!(m.alpha_loss_cost(d("1000")), d("1")); // 1000 · 0.001, always adverse
    }

    #[test]
    fn simulate_charges_the_directional_alpha_loss_and_is_inert_at_zero() {
        // Buy 1 @100, sell 1 @100 (two fills). With a non-zero alpha-loss the slippage grows by exactly
        // alpha_loss·notional per fill; at 0 the breakdown is byte-identical to the pre-QE-444 path.
        let events = vec![buy("1", "100"), sell("1", "100")];
        let base = FrictionConfig::default();
        let baseline = simulate(&events, &base);

        let taxed_cfg = FrictionConfig {
            slippage: SlippageModel {
                alpha_loss: d("0.01"),
                ..SlippageModel::default()
            },
            ..FrictionConfig::default()
        };
        let taxed = simulate(&events, &taxed_cfg);
        // Two fills of notional 100 ⇒ extra slippage 2 · (100 · 0.01) = 2.0; gross/fees/funding unchanged.
        assert_eq!(taxed.slippage - baseline.slippage, d("2.0"));
        assert_eq!(taxed.gross, baseline.gross);
        assert_eq!(taxed.fees, baseline.fees);
        assert!(taxed.net() < baseline.net(), "alpha-loss must reduce net");

        // Inert at 0: byte-identical breakdown.
        let zero_cfg = FrictionConfig {
            slippage: SlippageModel {
                alpha_loss: Decimal::ZERO,
                ..SlippageModel::default()
            },
            ..FrictionConfig::default()
        };
        assert_eq!(simulate(&events, &zero_cfg), baseline);
    }
}
