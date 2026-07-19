//! Liquidity screen for fetch-all ingest (QE-464 / QE-440 / QE-447).
//!
//! Fetch-all must not silently admit illiquid alts as tradable **at size**. Capacity-eligibility
//! requires per-instrument **rolling ADV / impact calibration** (QE-440): the major-calibrated `$250k`
//! floor is a mirage on a thin name. This screen classifies each candidate:
//!
//! * **Uncalibrated** — no rolling-ADV/impact measurement is available, so capacity-eligibility cannot
//!   be established. Excluded (never assumed tradable at the major floor).
//! * **Thin** — calibrated, but the rolling ADV is below the conservative floor, so trading even a
//!   small deployed notional would breach the `%ADV` participation cap (QE-447). Flagged/excluded.
//! * **Tradable** — calibrated and liquid enough that the deployed notional stays within the
//!   participation cap.
//!
//! The screen is a pure function over already-measured inputs — it does not itself fit ADV/impact
//! (that is QE-440's job) — so it is deterministic and offline-testable.

use rust_decimal::Decimal;

use qe_domain::InstrumentId;

/// Conservative default rolling-ADV floor, in USD, below which a name is treated as **thin** for
/// capacity purposes (QE-464). Picked deliberately conservative — **flagged** for product confirmation
/// (the spec does not fix a `%ADV` thin threshold). At the default `$250k` deployed floor and a 1%
/// participation cap, an ADV below `$25M` already forces a >1% footprint; `$2M` is a firmly-thin cutoff
/// well inside that, so nothing marked `Tradable` here is a capacity mirage.
pub const DEFAULT_MIN_ADV_USD: i64 = 2_000_000;

/// The measured liquidity inputs for one candidate instrument (QE-440 outputs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityInput {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Rolling average daily volume in USD, if a QE-440 calibration exists (`None` ⇒ uncalibrated).
    pub rolling_adv_usd: Option<Decimal>,
}

/// The screen verdict for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityVerdict {
    /// Calibrated and liquid enough to trade the deployed notional within the participation cap.
    Tradable,
    /// Calibrated but below the ADV floor — trading at size breaches the `%ADV` cap (QE-447).
    Thin,
    /// No rolling-ADV/impact calibration — capacity-eligibility cannot be established (QE-440).
    Uncalibrated,
}

impl LiquidityVerdict {
    /// Whether a name with this verdict may be admitted as tradable at size.
    #[must_use]
    pub const fn is_tradable(self) -> bool {
        matches!(self, LiquidityVerdict::Tradable)
    }

    /// Stable identifier for logging / the store flag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LiquidityVerdict::Tradable => "tradable",
            LiquidityVerdict::Thin => "thin",
            LiquidityVerdict::Uncalibrated => "uncalibrated",
        }
    }
}

/// The screened classification of one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenedInstrument {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Its liquidity verdict.
    pub verdict: LiquidityVerdict,
}

/// Screen `candidates` for capacity-eligibility against a `min_adv_usd` floor.
///
/// An instrument with no rolling-ADV/impact calibration is [`LiquidityVerdict::Uncalibrated`]
/// (excluded — capacity cannot be established); one whose ADV is **below** the floor is
/// [`LiquidityVerdict::Thin`] (flagged — trading at size would breach the QE-447 participation cap);
/// otherwise [`LiquidityVerdict::Tradable`]. Deterministic and order-preserving.
#[must_use]
pub fn screen_liquidity(
    candidates: &[LiquidityInput],
    min_adv_usd: Decimal,
) -> Vec<ScreenedInstrument> {
    candidates
        .iter()
        .map(|c| {
            let verdict = match c.rolling_adv_usd {
                // Capacity-eligibility REQUIRES a calibration (QE-440) — no measurement ⇒ excluded.
                None => LiquidityVerdict::Uncalibrated,
                Some(adv) if adv < min_adv_usd => LiquidityVerdict::Thin,
                Some(_) => LiquidityVerdict::Tradable,
            };
            ScreenedInstrument {
                instrument: c.instrument.clone(),
                verdict,
            }
        })
        .collect()
}

/// The subset of `screened` admissible as tradable at size (drops thin + uncalibrated names).
#[must_use]
pub fn tradable_only(screened: &[ScreenedInstrument]) -> Vec<InstrumentId> {
    screened
        .iter()
        .filter(|s| s.verdict.is_tradable())
        .map(|s| s.instrument.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }
    fn input(sym: &str, adv: Option<i64>) -> LiquidityInput {
        LiquidityInput {
            instrument: inst(sym),
            rolling_adv_usd: adv.map(Decimal::from),
        }
    }

    #[test]
    fn screen_requires_calibration_and_flags_thin_names() {
        let floor = Decimal::from(DEFAULT_MIN_ADV_USD);
        let candidates = vec![
            input("BTCUSDT", Some(500_000_000)), // liquid major
            input("THINALT", Some(100_000)),     // calibrated but thin
            input("NOCALUSD", None),             // no ADV/impact calibration
        ];
        let screened = screen_liquidity(&candidates, floor);
        assert_eq!(screened[0].verdict, LiquidityVerdict::Tradable);
        assert_eq!(screened[1].verdict, LiquidityVerdict::Thin);
        assert_eq!(screened[2].verdict, LiquidityVerdict::Uncalibrated);

        // Only the liquid, calibrated name is admitted at size — no capacity mirage on the thin alt.
        assert_eq!(tradable_only(&screened), vec![inst("BTCUSDT")]);
    }

    #[test]
    fn boundary_at_the_floor_is_tradable() {
        let floor = Decimal::from(DEFAULT_MIN_ADV_USD);
        let at = screen_liquidity(&[input("X", Some(DEFAULT_MIN_ADV_USD))], floor);
        assert_eq!(at[0].verdict, LiquidityVerdict::Tradable);
        let below = screen_liquidity(&[input("X", Some(DEFAULT_MIN_ADV_USD - 1))], floor);
        assert_eq!(below[0].verdict, LiquidityVerdict::Thin);
        assert_eq!(LiquidityVerdict::Thin.as_str(), "thin");
    }
}
