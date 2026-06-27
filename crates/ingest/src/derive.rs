//! Derived fields computed during fusion (QE-104), all on exact `rust_decimal` — no float money.
//!
//! - [`typical_price`] / [`vwap`]: volume-weighted average price over a window.
//! - [`Adjustment`] / [`adjust_bar`]: split / contract-multiplier normalisation.
//! - [`spread_to_underlier`]: the perp-minus-spot spread series.

use rust_decimal::Decimal;

use qe_domain::{Bar, DomainError, Price, Qty};

/// The typical price of a bar: `(high + low + close) / 3`.
#[must_use]
pub fn typical_price(bar: &Bar) -> Decimal {
    (bar.high().get() + bar.low().get() + bar.close().get()) / Decimal::from(3)
}

/// Volume-weighted average price over a window of bars: `Σ(typicalᵢ · volumeᵢ) / Σ volumeᵢ`.
///
/// Returns `None` when the window is empty or carries zero total volume (the average is undefined).
/// Exact: every term is a `Decimal` product/sum, so the result is reproducible and order-stable.
#[must_use]
pub fn vwap(bars: &[Bar]) -> Option<Decimal> {
    let mut num = Decimal::ZERO;
    let mut den = Decimal::ZERO;
    for bar in bars {
        let v = bar.volume().get();
        num += typical_price(bar) * v;
        den += v;
    }
    if den.is_zero() {
        None
    } else {
        Some(num / den)
    }
}

/// A multiplicative price/quantity adjustment — models contract-multiplier changes or splits.
///
/// Applied as `price' = price · price_factor`, `qty' = qty · qty_factor`. [`Adjustment::IDENTITY`]
/// (both factors `1`) is a no-op, the default for crypto perps with no split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adjustment {
    /// Multiplier applied to every price (OHLC).
    pub price_factor: Decimal,
    /// Multiplier applied to volume.
    pub qty_factor: Decimal,
}

impl Adjustment {
    /// The no-op adjustment (`×1` on both axes).
    pub const IDENTITY: Adjustment = Adjustment {
        price_factor: Decimal::ONE,
        qty_factor: Decimal::ONE,
    };
}

impl Default for Adjustment {
    fn default() -> Self {
        Adjustment::IDENTITY
    }
}

/// Apply an [`Adjustment`] to a bar, scaling OHLC by `price_factor` and volume by `qty_factor`.
///
/// The OHLC ordering is preserved under a non-negative `price_factor`, so the result re-validates
/// through [`Bar::new`].
///
/// # Errors
/// [`DomainError`] if a scaled price/qty is negative, or the scaled OHLC fails bar validation.
pub fn adjust_bar(bar: &Bar, adj: Adjustment) -> Result<Bar, DomainError> {
    let p = |price: Price| Price::new(price.get() * adj.price_factor);
    Bar::new(
        bar.open_time(),
        bar.resolution(),
        p(bar.open())?,
        p(bar.high())?,
        p(bar.low())?,
        p(bar.close())?,
        Qty::new(bar.volume().get() * adj.qty_factor)?,
        bar.trades(),
    )
}

/// The spread to the underlier: `perp_close − spot_close` (signed; positive when the perp trades
/// above spot).
#[must_use]
pub fn spread_to_underlier(perp_close: Price, spot_close: Price) -> Decimal {
    perp_close.get() - spot_close.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Resolution, Timestamp};
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn price(s: &str) -> Price {
        Price::new(dec(s)).unwrap()
    }
    fn qty(s: &str) -> Qty {
        Qty::new(dec(s)).unwrap()
    }

    fn bar(open: &str, high: &str, low: &str, close: &str, vol: &str, t_ms: i64) -> Bar {
        Bar::new(
            Timestamp::from_millis(t_ms),
            Resolution::M5,
            price(open),
            price(high),
            price(low),
            price(close),
            qty(vol),
            1,
        )
        .unwrap()
    }

    #[test]
    fn typical_price_is_exact_thirds() {
        // (110 + 95 + 105) / 3 = 310 / 3 (exact decimal division to Decimal's scale).
        let b = bar("100", "110", "95", "105", "1", 0);
        assert_eq!(typical_price(&b), dec("310") / Decimal::from(3));
    }

    #[test]
    fn vwap_matches_hand_computed_reference() {
        // Three bars, typical prices T and volumes V:
        //   b1: typ = (10+8+9)/3   = 9,   vol = 2
        //   b2: typ = (22+18+20)/3 = 20,  vol = 3
        //   b3: typ = (30+30+30)/3 = 30,  vol = 5
        // vwap = (9*2 + 20*3 + 30*5) / (2+3+5) = (18 + 60 + 150) / 10 = 228/10 = 22.8
        let bars = vec![
            bar("9", "10", "8", "9", "2", 0),
            bar("20", "22", "18", "20", "3", 300_000),
            bar("30", "30", "30", "30", "5", 600_000),
        ];
        assert_eq!(vwap(&bars), Some(dec("22.8")));
    }

    #[test]
    fn vwap_is_none_for_empty_or_zero_volume() {
        assert_eq!(vwap(&[]), None);
        let flat = vec![bar("10", "10", "10", "10", "0", 0)];
        assert_eq!(vwap(&flat), None);
    }

    #[test]
    fn identity_adjustment_is_a_no_op() {
        let b = bar("100", "110", "95", "105", "12.5", 0);
        assert_eq!(adjust_bar(&b, Adjustment::IDENTITY).unwrap(), b);
        assert_eq!(adjust_bar(&b, Adjustment::default()).unwrap(), b);
    }

    #[test]
    fn price_factor_scales_ohlc_and_preserves_invariant() {
        let b = bar("100", "110", "95", "105", "12.5", 0);
        let adj = Adjustment {
            price_factor: dec("2"),
            qty_factor: dec("0.5"),
        };
        let out = adjust_bar(&b, adj).unwrap();
        assert_eq!(out.open().get(), dec("200"));
        assert_eq!(out.high().get(), dec("220"));
        assert_eq!(out.low().get(), dec("190"));
        assert_eq!(out.close().get(), dec("210"));
        assert_eq!(out.volume().get(), dec("6.25"));
    }

    #[test]
    fn spread_is_signed_perp_minus_spot() {
        assert_eq!(spread_to_underlier(price("101"), price("100")), dec("1"));
        assert_eq!(spread_to_underlier(price("99"), price("100")), dec("-1"));
    }
}
