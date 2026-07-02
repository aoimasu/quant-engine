//! Position netting (QE-213) — nets per-strategy post-breaker targets into one aggregate per instrument.
//!
//! Each strategy's **target** is its post-breaker signed notional as a fraction of allowed capital:
//! `weight_i × size_bps_i / 10_000`, signed `+` for a long position and `−` for a short (`0` when flat). The
//! netter ([`PositionNetter`]) sums these into a single [`NetTarget`] (`net = long − short`), split by side.
//!
//! **Gated strategies contribute zero by construction.** QE-212's [`BreakerLayer::clamp`] rewrites a gated
//! strategy's decision to [`Decision::Exit`], and the shared [`PositionState::advance`] turns `Exit` into a
//! **flat** position — whose target is `0`. No special-case: a gated strategy simply arrives as a flat leg.
//!
//! Money is [`Decimal`] throughout; the ensemble weight (`f64`) is converted once at the boundary. The dev
//! universe is single-instrument, so netting yields one aggregate; the per-side `long`/`short` split is the
//! per-direction aggregate the QE-212 forward obligation (per-direction breakers) and QE-215 (gross/per-side
//! caps) consume.
//!
//! [`BreakerLayer::clamp`]: crate::live_breakers::BreakerLayer::clamp
//! [`Decision::Exit`]: qe_signal::Decision::Exit

use rust_decimal::Decimal;

use qe_domain::Direction;
use qe_signal::PositionState;

/// Basis points per whole — `size_bps` is basis points of allowed capital.
const BPS_PER_WHOLE: i64 = 10_000;

/// Convert an ensemble weight (`f64`) to `Decimal` deterministically. A non-finite weight maps to `0` (it
/// contributes nothing) — the ensemble's weights are validated finite at seal time, so a `NaN`/`±inf` here is
/// an upstream bug; the `debug_assert` surfaces it in dev/CI while the release fallback stays fail-safe.
fn weight_to_decimal(weight: f64) -> Decimal {
    debug_assert!(
        weight.is_finite(),
        "ensemble weight must be finite, got {weight}"
    );
    Decimal::from_f64_retain(weight).unwrap_or(Decimal::ZERO)
}

/// One strategy's post-breaker leg: its held direction (`None` = flat, including gated), ensemble weight, and
/// per-genome `size_bps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetLeg {
    /// Post-breaker held direction; `None` when flat (a gated strategy is flat here).
    pub direction: Option<Direction>,
    /// Ensemble weight for this strategy.
    pub weight: Decimal,
    /// Target notional as basis points of allowed capital (`RiskParams::size_bps`).
    pub size_bps: u16,
}

impl NetLeg {
    /// Build a leg from a (post-breaker) [`PositionState`], its ensemble `weight` (`f64`), and `size_bps`.
    #[must_use]
    pub fn from_position(position: PositionState, weight: f64, size_bps: u16) -> Self {
        Self {
            direction: position.dir,
            weight: weight_to_decimal(weight),
            size_bps,
        }
    }

    /// The unsigned target magnitude: `weight × size_bps / 10_000` (`0` when flat).
    #[must_use]
    fn magnitude(&self) -> Decimal {
        if self.direction.is_none() {
            return Decimal::ZERO;
        }
        self.weight * Decimal::from(self.size_bps) / Decimal::from(BPS_PER_WHOLE)
    }

    /// The signed target: `+magnitude` for long, `−magnitude` for short, `0` when flat.
    #[must_use]
    pub fn signed_target(&self) -> Decimal {
        match self.direction {
            Some(Direction::Long) => self.magnitude(),
            Some(Direction::Short) => -self.magnitude(),
            None => Decimal::ZERO,
        }
    }
}

/// The aggregate target for one instrument: the net signed target and its per-side split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTarget {
    /// The net signed aggregate target (fraction of allowed capital): `long − short`.
    pub net: Decimal,
    /// Sum of long-leg magnitudes (≥ 0).
    pub long: Decimal,
    /// Sum of short-leg magnitudes (≥ 0).
    pub short: Decimal,
}

impl NetTarget {
    /// Gross exposure: `long + short` (both sides summed, unsigned).
    #[must_use]
    pub fn gross(&self) -> Decimal {
        self.long + self.short
    }
}

/// Nets per-strategy post-breaker targets into one aggregate target per instrument.
pub struct PositionNetter;

impl PositionNetter {
    /// Net a set of post-breaker legs into one [`NetTarget`]. `net` equals `Σ leg.signed_target()`.
    #[must_use]
    pub fn net(legs: &[NetLeg]) -> NetTarget {
        let mut long = Decimal::ZERO;
        let mut short = Decimal::ZERO;
        for leg in legs {
            match leg.direction {
                Some(Direction::Long) => long += leg.magnitude(),
                Some(Direction::Short) => short += leg.magnitude(),
                None => {}
            }
        }
        NetTarget {
            net: long - short,
            long,
            short,
        }
    }

    /// The per-bar entry: net the (post-breaker) `positions` against the vintage's `weights` and per-genome
    /// `sizes` (all aligned to the chromosomes). A gated strategy's position is already flat, so it
    /// contributes zero.
    ///
    /// # Panics
    /// Panics if `positions`, `weights`, and `sizes` are not the same length. This is a **hard** assert (not
    /// `debug_assert`): netting is capital-affecting, so a silent per-leg drop from a `zip` truncation would
    /// mis-size the aggregate target — fail fast instead. Callers pass chromosome-aligned slices.
    #[must_use]
    pub fn net_positions(positions: &[PositionState], weights: &[f64], sizes: &[u16]) -> NetTarget {
        assert!(
            positions.len() == weights.len() && weights.len() == sizes.len(),
            "positions/weights/sizes must be aligned to the vintage's chromosomes (got {}, {}, {})",
            positions.len(),
            weights.len(),
            sizes.len()
        );
        let legs: Vec<NetLeg> = positions
            .iter()
            .zip(weights)
            .zip(sizes)
            .map(|((&p, &w), &s)| NetLeg::from_position(p, w, s))
            .collect();
        Self::net(&legs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::ChromosomeDecision;
    use crate::live_breakers::BreakerLayer;
    use qe_risk::{BreakerThresholds, Fraction, DEFAULT_FAST_WINDOW};
    use qe_signal::Decision;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn frac(s: &str) -> Fraction {
        Fraction::new(d(s)).unwrap()
    }
    fn thresholds() -> BreakerThresholds {
        BreakerThresholds {
            slow_dd: frac("0.05"),
            med_dd: frac("0.12"),
            fast_drop: frac("0.08"),
        }
    }
    fn leg(direction: Option<Direction>, weight: &str, size_bps: u16) -> NetLeg {
        NetLeg {
            direction,
            weight: d(weight),
            size_bps,
        }
    }

    /// AC (half 1): `net` equals the sum of the per-leg signed targets, and the per-side split is correct.
    #[test]
    fn net_equals_sum_of_leg_targets() {
        let legs = [
            leg(Some(Direction::Long), "0.5", 100), // +0.5 * 0.01 = +0.005
            leg(Some(Direction::Short), "0.3", 200), // -0.3 * 0.02 = -0.006
            leg(None, "0.2", 500),                  // flat → 0
            leg(Some(Direction::Long), "0.25", 400), // +0.25 * 0.04 = +0.010
        ];
        let want: Decimal = legs.iter().map(NetLeg::signed_target).sum();
        let net = PositionNetter::net(&legs);
        assert_eq!(net.net, want);
        assert_eq!(net.net, d("0.009")); // 0.005 - 0.006 + 0.010
        assert_eq!(net.long, d("0.015")); // 0.005 + 0.010
        assert_eq!(net.short, d("0.006"));
        assert_eq!(net.gross(), d("0.021"));
    }

    /// AC (half 2): a flat leg contributes zero — netting with it equals netting without it.
    #[test]
    fn flat_leg_contributes_zero() {
        let flat = leg(None, "0.9", 900);
        assert_eq!(flat.signed_target(), Decimal::ZERO);

        let base = [
            leg(Some(Direction::Long), "0.5", 100),
            leg(Some(Direction::Short), "0.5", 100),
        ];
        let with_flat = [base[0], base[1], flat];
        assert_eq!(PositionNetter::net(&base), PositionNetter::net(&with_flat));
    }

    /// AC (end-to-end with QE-212): a strategy gated by the breaker is flat post-breaker, so it contributes
    /// zero to the netted aggregate.
    #[test]
    fn gated_strategy_via_breaker_contributes_zero() {
        // Three strategies; strategy 0 will be gated.
        let weights = [0.5_f64, 0.3, 0.2];
        let sizes = [300_u16, 200, 400];

        // Prior positions: all long (as if held into this bar).
        let prior = [
            PositionState::held(Direction::Long, 3),
            PositionState::held(Direction::Long, 3),
            PositionState::held(Direction::Long, 3),
        ];
        // Raw per-bar decisions: everyone holds.
        let raw = [
            ChromosomeDecision {
                index: 0,
                decision: Decision::Hold,
            },
            ChromosomeDecision {
                index: 1,
                decision: Decision::Hold,
            },
            ChromosomeDecision {
                index: 2,
                decision: Decision::Hold,
            },
        ];

        // Gate strategy 0 via the breaker.
        let mut layer = BreakerLayer::new(vec![thresholds(); 3], frac("0.10"), DEFAULT_FAST_WINDOW);
        layer.observe_strategy(0, d("100"));
        layer.observe_strategy(0, d("50")); // 50% drawdown → gate strategy 0
        assert!(layer.is_gated(0) && !layer.is_gated(1) && !layer.is_gated(2));

        // Clamp, then advance the prior positions by the clamped decisions → post-breaker positions.
        let clamped = layer.clamp(&raw);
        let post: Vec<PositionState> = clamped
            .iter()
            .map(|cd| prior[cd.index].advance(cd.decision))
            .collect();
        assert_eq!(post[0].dir, None, "gated strategy 0 is flat post-breaker");
        assert_eq!(post[1].dir, Some(Direction::Long));

        let net = PositionNetter::net_positions(&post, &weights, &sizes);

        // Aggregate equals the sum over the UNGATED strategies (1 and 2) only.
        let ungated: Decimal = [1usize, 2]
            .iter()
            .map(|&i| NetLeg::from_position(post[i], weights[i], sizes[i]).signed_target())
            .sum();
        assert_eq!(net.net, ungated);
        // Strategy 0 (gated) contributes zero.
        assert_eq!(
            NetLeg::from_position(post[0], weights[0], sizes[0]).signed_target(),
            Decimal::ZERO
        );
    }

    /// Equal-and-opposite legs net to zero while gross exposure is non-zero (netting, not gross-summing).
    #[test]
    fn longs_and_shorts_offset() {
        let legs = [
            leg(Some(Direction::Long), "0.5", 200),
            leg(Some(Direction::Short), "0.5", 200),
        ];
        let net = PositionNetter::net(&legs);
        assert_eq!(net.net, Decimal::ZERO);
        assert!(net.gross() > Decimal::ZERO);
        assert_eq!(net.long, net.short);
    }

    /// A leg's magnitude tracks `weight × size_bps / 10_000` exactly, and doubling the weight doubles it.
    #[test]
    fn weights_and_sizes_scale_the_target() {
        let base = leg(Some(Direction::Long), "0.25", 400);
        assert_eq!(base.signed_target(), d("0.01")); // 0.25 * 0.04

        let doubled = leg(Some(Direction::Long), "0.5", 400);
        assert_eq!(
            doubled.signed_target(),
            base.signed_target() * Decimal::from(2)
        );

        // The production `from_position` (f64 → Decimal via `from_f64_retain`) path is exact: a leg built
        // from the `f64` weight `0.25` nets identically to one built from the `Decimal` literal `0.25`.
        let via_f64 = NetLeg::from_position(PositionState::held(Direction::Long, 0), 0.25, 400);
        assert_eq!(via_f64.signed_target(), base.signed_target());
    }
}
