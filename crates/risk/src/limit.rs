//! Risk-limit kinds, validated cap value types, and per-kind violation outcomes.

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use qe_domain::Notional;

use crate::RiskError;

/// The kinds of hard cap the order path is born with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// Maximum notional per order.
    MaxNotional,
    /// Maximum leverage.
    MaxLeverage,
    /// Maximum gross (absolute) exposure across positions.
    MaxGrossExposure,
    /// Maximum net (signed) exposure across positions.
    MaxNetExposure,
    /// Minimum distance to liquidation (as a fraction of price).
    LiquidationDistanceFloor,
    /// Maximum margin utilisation (fraction of available margin).
    MarginUtilisationCeiling,
    /// Maximum participation as a fraction of rolling hourly ADV (`order_qty / ADV`) — QE-447.
    MaxParticipation,
    /// Maximum per-vintage drawdown.
    DrawdownCap,
}

/// What happens when a limit is violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitOutcome {
    /// Shrink the order to fit the cap and continue.
    Clamp,
    /// Refuse this order but keep trading.
    Reject,
    /// Flatten and halt (kill).
    Halt,
}

impl LimitKind {
    /// The default outcome policy for this limit kind.
    ///
    /// Order-level sizing caps clamp; portfolio/margin-level breaches reject (don't silently resize a
    /// portfolio breach into a smaller order); a per-vintage drawdown breach halts the vintage.
    #[must_use]
    pub const fn default_outcome(self) -> LimitOutcome {
        match self {
            LimitKind::MaxNotional | LimitKind::MaxLeverage => LimitOutcome::Clamp,
            LimitKind::MaxGrossExposure
            | LimitKind::MaxNetExposure
            | LimitKind::LiquidationDistanceFloor
            | LimitKind::MarginUtilisationCeiling
            | LimitKind::MaxParticipation => LimitOutcome::Reject,
            LimitKind::DrawdownCap => LimitOutcome::Halt,
        }
    }

    /// Stable identifier for logging / config.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LimitKind::MaxNotional => "max_notional",
            LimitKind::MaxLeverage => "max_leverage",
            LimitKind::MaxGrossExposure => "max_gross_exposure",
            LimitKind::MaxNetExposure => "max_net_exposure",
            LimitKind::LiquidationDistanceFloor => "liquidation_distance_floor",
            LimitKind::MarginUtilisationCeiling => "margin_utilisation_ceiling",
            LimitKind::MaxParticipation => "max_participation",
            LimitKind::DrawdownCap => "drawdown_cap",
        }
    }
}

/// A non-negative leverage multiple. Validated on construction and at the serde boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Leverage(Decimal);

impl Leverage {
    /// Construct a leverage, rejecting negatives.
    ///
    /// # Errors
    /// [`RiskError::NegativeLeverage`] if `value < 0`.
    pub fn new(value: Decimal) -> Result<Self, RiskError> {
        if value < Decimal::ZERO {
            return Err(RiskError::NegativeLeverage(value.to_string()));
        }
        Ok(Leverage(value))
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }
}

impl Serialize for Leverage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        rust_decimal::serde::str::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Leverage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = rust_decimal::serde::str::deserialize(d)?;
        Leverage::new(value).map_err(serde::de::Error::custom)
    }
}

/// A fraction in the inclusive range `[0, 1]`. Validated on construction and at the serde boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fraction(Decimal);

impl Fraction {
    /// Zero — the lower bound of the valid `[0, 1]` range.
    pub const ZERO: Fraction = Fraction(Decimal::ZERO);

    /// One — the upper bound of the valid `[0, 1]` range.
    pub const ONE: Fraction = Fraction(Decimal::ONE);

    /// Construct a fraction, rejecting values outside `[0, 1]`.
    ///
    /// # Errors
    /// [`RiskError::FractionOutOfRange`] if `value < 0` or `value > 1`.
    pub fn new(value: Decimal) -> Result<Self, RiskError> {
        if value < Decimal::ZERO || value > Decimal::ONE {
            return Err(RiskError::FractionOutOfRange(value.to_string()));
        }
        Ok(Fraction(value))
    }

    /// The underlying decimal.
    #[must_use]
    pub fn get(self) -> Decimal {
        self.0
    }
}

impl Serialize for Fraction {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        rust_decimal::serde::str::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Fraction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = rust_decimal::serde::str::deserialize(d)?;
        Fraction::new(value).map_err(serde::de::Error::custom)
    }
}

/// The configured cap set. Every field is optional: `None` means "no cap of this kind".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RiskLimits {
    /// Maximum notional per order.
    pub max_notional: Option<Notional>,
    /// Maximum leverage.
    pub max_leverage: Option<Leverage>,
    /// Maximum gross exposure.
    pub max_gross_exposure: Option<Notional>,
    /// Maximum net exposure.
    pub max_net_exposure: Option<Notional>,
    /// Minimum distance to liquidation.
    pub liquidation_distance_floor: Option<Fraction>,
    /// Maximum margin utilisation.
    pub margin_utilisation_ceiling: Option<Fraction>,
    /// Maximum participation as a fraction of rolling hourly ADV (`order_qty / ADV`) — QE-447.
    /// `None` (default) disables the guard: no order is ever rejected by the participation cap.
    pub max_participation: Option<Fraction>,
    /// Maximum per-vintage drawdown.
    pub drawdown_cap: Option<Fraction>,
}

/// A named limit violation and the outcome it triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitBreach {
    /// Which limit was violated.
    pub kind: LimitKind,
    /// What the violation triggers.
    pub outcome: LimitOutcome,
    /// Human-readable detail (observed vs cap).
    pub detail: String,
}

impl LimitBreach {
    /// A breach using the limit kind's default outcome policy.
    #[must_use]
    pub fn with_default_outcome(kind: LimitKind, detail: impl Into<String>) -> Self {
        LimitBreach {
            kind,
            outcome: kind.default_outcome(),
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn default_outcomes_match_policy() {
        assert_eq!(
            LimitKind::MaxNotional.default_outcome(),
            LimitOutcome::Clamp
        );
        assert_eq!(
            LimitKind::MaxLeverage.default_outcome(),
            LimitOutcome::Clamp
        );
        assert_eq!(
            LimitKind::MaxGrossExposure.default_outcome(),
            LimitOutcome::Reject
        );
        assert_eq!(
            LimitKind::LiquidationDistanceFloor.default_outcome(),
            LimitOutcome::Reject
        );
        assert_eq!(
            LimitKind::MarginUtilisationCeiling.default_outcome(),
            LimitOutcome::Reject
        );
        assert_eq!(
            LimitKind::MaxParticipation.default_outcome(),
            LimitOutcome::Reject
        );
        assert_eq!(LimitKind::DrawdownCap.default_outcome(), LimitOutcome::Halt);
    }

    #[test]
    fn fraction_bound_consts_match_constructor() {
        // The `ZERO`/`ONE` bound consts equal the validated constructor at the range endpoints, so the
        // order path can use them instead of `Fraction::new(..).expect(..)`.
        assert_eq!(Fraction::ZERO, Fraction::new(Decimal::ZERO).unwrap());
        assert_eq!(Fraction::ONE, Fraction::new(Decimal::ONE).unwrap());
        assert_eq!(Fraction::ZERO.get(), Decimal::ZERO);
        assert_eq!(Fraction::ONE.get(), Decimal::ONE);
    }

    #[test]
    fn leverage_rejects_negative_on_construction_and_serde() {
        assert!(Leverage::new(dec("-1")).is_err());
        assert!(Leverage::new(dec("12.5")).is_ok());
        assert!(serde_json::from_str::<Leverage>("\"-1\"").is_err());
        let lev: Leverage = serde_json::from_str("\"12.5\"").unwrap();
        assert_eq!(lev.get(), dec("12.5"));
        assert_eq!(serde_json::to_string(&lev).unwrap(), "\"12.5\"");
    }

    #[test]
    fn fraction_rejects_out_of_range_on_construction_and_serde() {
        assert!(Fraction::new(dec("-0.01")).is_err());
        assert!(Fraction::new(dec("1.01")).is_err());
        assert!(Fraction::new(dec("0")).is_ok());
        assert!(Fraction::new(dec("1")).is_ok());
        assert!(serde_json::from_str::<Fraction>("\"1.5\"").is_err());
        assert!(serde_json::from_str::<Fraction>("\"0.8\"").is_ok());
    }

    #[test]
    fn risk_limits_round_trips() {
        let limits = RiskLimits {
            max_notional: Some(Notional::new(dec("100000"))),
            max_leverage: Some(Leverage::new(dec("10")).unwrap()),
            drawdown_cap: Some(Fraction::new(dec("0.2")).unwrap()),
            ..RiskLimits::default()
        };
        let json = serde_json::to_string(&limits).unwrap();
        assert_eq!(serde_json::from_str::<RiskLimits>(&json).unwrap(), limits);
    }

    #[test]
    fn breach_uses_default_outcome() {
        let b = LimitBreach::with_default_outcome(LimitKind::DrawdownCap, "dd 0.3 > cap 0.2");
        assert_eq!(b.outcome, LimitOutcome::Halt);
    }
}
