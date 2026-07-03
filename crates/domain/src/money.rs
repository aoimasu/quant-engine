//! Exact decimal money — no binary floating point.
//!
//! [`Price`], [`Qty`], and [`Notional`] wrap [`rust_decimal::Decimal`], a 96-bit fixed-point
//! decimal. Addition and subtraction are therefore **exact and associative** — there is no float
//! error to accumulate. For inputs within `Decimal`'s 28-significant-digit range, the only place a
//! value is rounded is [`Price::notional`] (`price × qty`), which takes an explicit
//! [`RoundingPolicy`] and target scale, so rounding is a deliberate, named decision rather than a
//! silent loss. (Beyond 28 digits `Decimal`'s own `*` rounds/saturates — see [`Price::notional`].)
//!
//! `Price` and `Qty` are non-negative by construction *and* by deserialization: their `Deserialize`
//! runs the validating constructor, so a negative value cannot enter via serde either.

use std::fmt;
use std::ops::{Add, Sub};

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::DomainError;

/// A non-negative price, quoted in the instrument's quote currency.
///
/// Serialises as a decimal string (exact); deserialisation re-runs [`Price::new`], so a negative
/// price is rejected at the serde boundary too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price(Decimal);

/// A non-negative quantity (base-asset size / number of contracts).
///
/// Serialises as a decimal string (exact); deserialisation re-runs [`Qty::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qty(Decimal);

/// A signed money amount — notional exposure or realised PnL, so it may be negative (no validation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Notional(#[serde(with = "rust_decimal::serde::str")] Decimal);

impl Serialize for Price {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rust_decimal::serde::str::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = rust_decimal::serde::str::deserialize(deserializer)?;
        Price::new(value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Qty {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        rust_decimal::serde::str::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Qty {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = rust_decimal::serde::str::deserialize(deserializer)?;
        Qty::new(value).map_err(serde::de::Error::custom)
    }
}

impl Price {
    /// Zero — a valid price.
    pub const ZERO: Price = Price(Decimal::ZERO);

    /// Construct a price, rejecting negative values.
    ///
    /// # Errors
    /// [`DomainError::NegativeMoney`] if `value < 0`.
    pub fn new(value: Decimal) -> Result<Self, DomainError> {
        if value < Decimal::ZERO {
            return Err(DomainError::NegativeMoney {
                kind: "price",
                value: value.to_string(),
            });
        }
        Ok(Price(value))
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }

    /// `price × qty`, rounded to `scale` decimal places using `policy`.
    ///
    /// For inputs whose exact product fits in `Decimal`'s 28 significant digits, this is the only
    /// rounding point: the product is exact, then rounded once, deliberately. **Precondition:**
    /// beyond 28 digits `Decimal`'s `*` itself rounds (banker's), and it **panics** on 96-bit
    /// magnitude overflow — realistic crypto precision (≤ 8 dp) stays well inside this bound.
    #[must_use]
    pub fn notional(self, qty: Qty, scale: u32, policy: RoundingPolicy) -> Notional {
        let product = self.0 * qty.0;
        Notional(product.round_dp_with_strategy(scale, policy.into()))
    }
}

impl Qty {
    /// Zero — a valid quantity.
    pub const ZERO: Qty = Qty(Decimal::ZERO);

    /// Construct a quantity, rejecting negative values.
    ///
    /// # Errors
    /// [`DomainError::NegativeMoney`] if `value < 0`.
    pub fn new(value: Decimal) -> Result<Self, DomainError> {
        if value < Decimal::ZERO {
            return Err(DomainError::NegativeMoney {
                kind: "qty",
                value: value.to_string(),
            });
        }
        Ok(Qty(value))
    }

    /// Construct a quantity from the **magnitude** of a signed decimal — total and infallible.
    ///
    /// Returns `|value|`, which is non-negative by construction, so the `Qty` invariant always holds.
    /// This is the proof-carrying replacement for `Qty::new(value.abs()).expect(..)` on the order path:
    /// where a magnitude has already been derived from a signed quantity (a delta, a signed position),
    /// there is no error case to reconstruct.
    #[must_use]
    pub fn abs_of(value: Decimal) -> Qty {
        Qty(value.abs())
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }
}

/// `+` is exact (decimal addition) and total: the sum of two non-negative quantities is non-negative,
/// so the result is always a valid [`Qty`] without re-validation. Panics only on 96-bit decimal
/// overflow (as [`Notional`]'s `Add` does) — well outside realistic fill/position magnitudes.
impl Add for Qty {
    type Output = Qty;
    fn add(self, rhs: Self) -> Self {
        Qty(self.0 + rhs.0)
    }
}

impl Notional {
    /// Zero.
    pub const ZERO: Notional = Notional(Decimal::ZERO);

    /// Construct a (possibly negative) notional amount.
    #[must_use]
    pub fn new(value: Decimal) -> Self {
        Notional(value)
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }

    /// Exact addition that returns `None` on decimal overflow instead of panicking.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Notional)
    }

    /// Exact subtraction that returns `None` on decimal overflow instead of panicking.
    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Notional)
    }
}

/// `+` is exact (decimal addition). Panics only on 96-bit decimal overflow — use
/// [`Notional::checked_add`] where overflow is possible.
impl Add for Notional {
    type Output = Notional;
    fn add(self, rhs: Self) -> Self {
        Notional(self.0 + rhs.0)
    }
}

/// `-` is exact (decimal subtraction). Panics only on 96-bit decimal overflow — use
/// [`Notional::checked_sub`] where overflow is possible.
impl Sub for Notional {
    type Output = Notional;
    fn sub(self, rhs: Self) -> Self {
        Notional(self.0 - rhs.0)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Qty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Notional {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How [`Price::notional`] rounds the `price × qty` product to its target scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum RoundingPolicy {
    /// Round half to even (banker's rounding) — the finance default, unbiased over many roundings.
    #[default]
    HalfEven,
    /// Round half away from zero.
    HalfUp,
    /// Truncate toward zero.
    Down,
    /// Round away from zero.
    Up,
}

impl From<RoundingPolicy> for RoundingStrategy {
    fn from(policy: RoundingPolicy) -> Self {
        match policy {
            RoundingPolicy::HalfEven => RoundingStrategy::MidpointNearestEven,
            RoundingPolicy::HalfUp => RoundingStrategy::MidpointAwayFromZero,
            RoundingPolicy::Down => RoundingStrategy::ToZero,
            RoundingPolicy::Up => RoundingStrategy::AwayFromZero,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).expect("valid decimal literal")
    }

    #[test]
    fn rejects_negative_price_and_qty() {
        assert!(matches!(
            Price::new(dec("-0.01")),
            Err(DomainError::NegativeMoney { kind: "price", .. })
        ));
        assert!(matches!(
            Qty::new(dec("-1")),
            Err(DomainError::NegativeMoney { kind: "qty", .. })
        ));
        assert!(Price::new(Decimal::ZERO).is_ok());
    }

    #[test]
    fn qty_abs_of_takes_magnitude_and_is_total() {
        // A signed value's magnitude is always a valid (non-negative) Qty.
        assert_eq!(Qty::abs_of(dec("-2.5")), Qty::new(dec("2.5")).unwrap());
        assert_eq!(Qty::abs_of(dec("2.5")), Qty::new(dec("2.5")).unwrap());
        assert_eq!(Qty::abs_of(Decimal::ZERO), Qty::ZERO);
    }

    #[test]
    fn qty_add_is_total_and_exact() {
        // Sum of two non-negative quantities is a valid Qty; addition is exact in decimal.
        assert_eq!(
            Qty::new(dec("0.1")).unwrap() + Qty::new(dec("0.2")).unwrap(),
            Qty::new(dec("0.3")).unwrap()
        );
        assert_eq!(
            Qty::ZERO + Qty::new(dec("1.5")).unwrap(),
            Qty::new(dec("1.5")).unwrap()
        );
    }

    #[test]
    fn notional_is_exact_when_no_rounding_needed() {
        // 0.1 + 0.2 is exact in decimal (unlike binary float).
        let sum = Notional::new(dec("0.1")) + Notional::new(dec("0.2"));
        assert_eq!(sum.get(), dec("0.3"));
    }

    #[test]
    fn banker_and_half_up_differ_on_a_midpoint() {
        let price = Price::new(dec("2.5")).unwrap();
        let qty = Qty::new(dec("1")).unwrap(); // product = 2.5, round to 0 dp
        assert_eq!(
            price.notional(qty, 0, RoundingPolicy::HalfEven).get(),
            dec("2")
        );
        assert_eq!(
            price.notional(qty, 0, RoundingPolicy::HalfUp).get(),
            dec("3")
        );
        // 3.5 -> HalfEven rounds to 4 (nearest even), HalfUp to 4 as well; 2.5 is the discriminator.
        let p35 = Price::new(dec("3.5")).unwrap();
        assert_eq!(
            p35.notional(qty, 0, RoundingPolicy::HalfEven).get(),
            dec("4")
        );
    }

    #[test]
    fn decimal_serialises_as_exact_string() {
        let price = Price::new(dec("12345.6789")).unwrap();
        let json = serde_json::to_string(&price).unwrap();
        assert_eq!(json, "\"12345.6789\"");
        assert_eq!(serde_json::from_str::<Price>(&json).unwrap(), price);
    }

    #[test]
    fn deserialize_rejects_negative_price_and_qty() {
        // The serde boundary must not bypass the non-negative invariant.
        assert!(serde_json::from_str::<Price>("\"-5.0\"").is_err());
        assert!(serde_json::from_str::<Qty>("\"-0.0001\"").is_err());
        // Valid values still round-trip.
        assert_eq!(
            serde_json::from_str::<Price>("\"5.0\"").unwrap(),
            Price::new(dec("5.0")).unwrap()
        );
    }

    #[test]
    fn checked_add_and_sub_return_none_on_overflow() {
        let max = Notional::new(Decimal::MAX);
        assert!(max.checked_add(Notional::new(Decimal::ONE)).is_none());
        let min = Notional::new(Decimal::MIN);
        assert!(min.checked_sub(Notional::new(Decimal::ONE)).is_none());
        // In-range arithmetic still succeeds.
        assert_eq!(
            Notional::new(dec("2")).checked_add(Notional::new(dec("3"))),
            Some(Notional::new(dec("5")))
        );
    }

    // Generate non-negative decimals with scale <= 8 so products stay exact (scale <= 16 <= 28).
    prop_compose! {
        fn small_nonneg()(mantissa in 0i64..1_000_000_000i64, scale in 0u32..=8) -> Decimal {
            Decimal::new(mantissa, scale)
        }
    }
    prop_compose! {
        fn small_signed()(mantissa in -1_000_000_000i64..1_000_000_000i64, scale in 0u32..=8) -> Decimal {
            Decimal::new(mantissa, scale)
        }
    }

    fn any_policy() -> impl Strategy<Value = RoundingPolicy> {
        prop_oneof![
            Just(RoundingPolicy::HalfEven),
            Just(RoundingPolicy::HalfUp),
            Just(RoundingPolicy::Down),
            Just(RoundingPolicy::Up),
        ]
    }

    proptest! {
        #[test]
        fn notional_addition_is_associative(a in small_signed(), b in small_signed(), c in small_signed()) {
            let (a, b, c) = (Notional::new(a), Notional::new(b), Notional::new(c));
            prop_assert_eq!((a + b) + c, a + (b + c));
        }

        #[test]
        fn notional_addition_is_commutative(a in small_signed(), b in small_signed()) {
            prop_assert_eq!(Notional::new(a) + Notional::new(b), Notional::new(b) + Notional::new(a));
        }

        #[test]
        fn notional_sub_inverts_add(a in small_signed(), b in small_signed()) {
            let (a, b) = (Notional::new(a), Notional::new(b));
            prop_assert_eq!((a + b) - b, a);
        }

        #[test]
        fn rounding_stays_within_one_ulp_and_target_scale(
            p in small_nonneg(),
            q in small_nonneg(),
            scale in 0u32..=8,
            policy in any_policy(),
        ) {
            let price = Price::new(p).unwrap();
            let qty = Qty::new(q).unwrap();
            let exact = p * q; // scale <= 16, exact within Decimal's 28-digit range
            let rounded = price.notional(qty, scale, policy).get();
            prop_assert!(rounded.scale() <= scale, "scale {} > target {}", rounded.scale(), scale);
            let ulp = Decimal::new(1, scale); // 10^-scale
            prop_assert!((rounded - exact).abs() < ulp, "|{rounded} - {exact}| >= {ulp}");
        }
    }
}
