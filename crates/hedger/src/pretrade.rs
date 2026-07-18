//! Pre-trade risk check (QE-215) — the margin/leverage governor at the netting→hedger boundary.
//!
//! Enforces the QE-009 [`RiskLimits`] on a [`TargetPosition`] **before** it leaves the planner: max notional,
//! max leverage, gross/net caps, the **liquidation-distance floor**, and the margin-utilisation ceiling. Each
//! configured cap that a target breaches produces a [`LimitBreach`] carrying the kind's contract outcome, and
//! the governor reduces them to a [`PreTradeVerdict`] by **severity** — `Halt` > `Reject` > clamp/`Send`:
//!
//! - **Clamp** caps (`MaxNotional`, `MaxLeverage`) shrink the sendable magnitude to the tightest cap.
//! - **Reject** caps (`MaxGross`, `MaxNet`, `LiquidationDistanceFloor`, `MarginUtilisationCeiling`,
//!   `MaxParticipation`) refuse the order outright (send **no** new target — keep trading, position
//!   unchanged); a `Reject` outranks a clamp.
//!
//! The QE-447 **%ADV participation guard** (`MaxParticipation`) is one of the Reject caps: it rejects a
//! delta-close order whose participation `|order notional| / ADV` exceeds a configured fraction of the rolling
//! hourly ADV supplied via [`PreTradeGovernor::with_adv`]. It defaults **off** (`RiskLimits.max_participation`
//! is `None`); a configured cap with unknown/non-positive ADV and a live order fails **closed**.
//! - **Halt** (contract-general; no pre-trade kind defaults to it) flattens-and-halts.
//!
//! The per-vintage `DrawdownCap` (→ `Halt`) is **not** a per-order pre-trade check — it is the QE-212 breaker
//! + QE-216 kill path — so it is not enforced here. Out of scope: out-of-band kill (QE-216).

// Order-emission path (QE-268): reject `unwrap`/`expect`/`panic` — a panic here is a live-trading fault.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rust_decimal::Decimal;

use qe_domain::Notional;
use qe_risk::{Fraction, LimitBreach, LimitKind, LimitOutcome, RiskLimits};

use qe_runtime_core::{CapitalView, TargetPosition};

/// What the governor decides for a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreTradeVerdict {
    /// Send this (possibly clamped) absolute target.
    Send(Notional),
    /// Refuse this order — send no new target, keep trading (position unchanged).
    Reject,
    /// Flatten and halt (kill).
    Halt,
}

/// The governor's decision: the verdict plus every cap the target breached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreTradeDecision {
    /// The action to take.
    pub verdict: PreTradeVerdict,
    /// Every configured cap the target breached (empty when the target is within all caps).
    pub breaches: Vec<LimitBreach>,
}

/// The pre-trade margin/leverage governor — enforces [`RiskLimits`] on a target before it is sent.
pub struct PreTradeGovernor {
    limits: RiskLimits,
    /// Venue maintenance-margin rate — needed by the liquidation-distance and margin-utilisation models.
    maintenance_margin_rate: Fraction,
    /// Rolling hourly ADV in **dollars** of the traded instrument (QE-440 unit), used by the QE-447 %ADV
    /// participation guard: participation `= |order notional| / ADV`. `None` means "ADV unknown at this
    /// plan" — with a configured participation cap and a live order that is **fail-closed** (reject), the
    /// same convention the liquidation/margin caps use for a degenerate required input.
    adv: Option<Notional>,
}

/// Severity rank for reducing a set of outcomes (`Halt` most severe).
fn severity(outcome: LimitOutcome) -> u8 {
    match outcome {
        LimitOutcome::Clamp => 0,
        LimitOutcome::Reject => 1,
        LimitOutcome::Halt => 2,
    }
}

/// The most severe outcome among the breaches, or `None` if there are none.
fn most_severe(breaches: &[LimitBreach]) -> Option<LimitOutcome> {
    breaches
        .iter()
        .map(|b| b.outcome)
        .max_by_key(|&o| severity(o))
}

impl PreTradeGovernor {
    /// A governor enforcing `limits`, using `maintenance_margin_rate` for the liquidation/margin models.
    #[must_use]
    pub fn new(limits: RiskLimits, maintenance_margin_rate: Fraction) -> Self {
        Self {
            limits,
            maintenance_margin_rate,
            // Default: no ADV supplied. With the participation cap defaulting to `None` too (QE-447), the
            // guard is fully inert unless *both* an ADV and a cap are configured — so existing call sites
            // and goldens are unchanged.
            adv: None,
        }
    }

    /// Supply the rolling hourly ADV (dollars, QE-440 unit) the QE-447 participation guard divides against.
    /// A rolling ADV changes each hour, so it is set per plan cycle here rather than baked into `new`.
    /// `None` leaves ADV unknown (fail-closed under a configured participation cap with a live order).
    #[must_use]
    pub fn with_adv(mut self, adv: Option<Notional>) -> Self {
        self.adv = adv;
        self
    }

    /// Check `target` against the configured caps given the current `capital` view, and decide.
    #[must_use]
    pub fn check(&self, target: TargetPosition, capital: CapitalView) -> PreTradeDecision {
        let notional = target.notional.get();
        let mag = notional.abs();
        // True gross exposure (`long + short`), unsigned — what the gross cap must be checked against (QE-418).
        // For a single instrument this equals `mag`; a hedged book makes it exceed `mag`.
        let gross = target.gross.get().abs();
        let equity = capital.equity.get();
        let avail = capital.available_margin.get();
        let mmr = self.maintenance_margin_rate.get();

        let mut breaches = Vec::new();
        // The sendable magnitude after applying every clamp cap (min of the caps hit).
        let mut clamp_mag = mag;

        // --- Clamp caps: shrink the order to fit. ---
        if let Some(cap) = self.limits.max_notional {
            let c = cap.get();
            if mag > c {
                breaches.push(LimitBreach::with_default_outcome(
                    LimitKind::MaxNotional,
                    format!("notional {mag} > max {c}"),
                ));
                clamp_mag = clamp_mag.min(c);
            }
        }
        if let Some(lev) = self.limits.max_leverage {
            if equity > Decimal::ZERO {
                let cap_notional = lev.get() * equity;
                if mag > cap_notional {
                    breaches.push(LimitBreach::with_default_outcome(
                        LimitKind::MaxLeverage,
                        format!("leverage {} > max {}", mag / equity, lev.get()),
                    ));
                    clamp_mag = clamp_mag.min(cap_notional);
                }
            } else if mag > Decimal::ZERO {
                // A position with no positive equity is infinite leverage — clamp to flat.
                breaches.push(LimitBreach::with_default_outcome(
                    LimitKind::MaxLeverage,
                    format!("leverage undefined (equity {equity}) with notional {mag}"),
                ));
                clamp_mag = Decimal::ZERO;
            }
        }

        // --- Reject caps: unsafe in a way that must not be silently resized. ---
        if let Some(cap) = self.limits.max_gross_exposure {
            let c = cap.get();
            // QE-418: check the gross cap against true gross exposure (`long + short`), not the net magnitude.
            if gross > c {
                breaches.push(LimitBreach::with_default_outcome(
                    LimitKind::MaxGrossExposure,
                    format!("gross {gross} > max {c}"),
                ));
            }
        }
        if let Some(cap) = self.limits.max_net_exposure {
            let c = cap.get();
            if mag > c {
                breaches.push(LimitBreach::with_default_outcome(
                    LimitKind::MaxNetExposure,
                    format!("net {mag} > max {c}"),
                ));
            }
        }
        if let Some(floor) = self.limits.liquidation_distance_floor {
            if mag > Decimal::ZERO {
                // Adverse price fraction to liquidation: margin ratio minus maintenance rate.
                let distance = equity / mag - mmr;
                if distance < floor.get() {
                    breaches.push(LimitBreach::with_default_outcome(
                        LimitKind::LiquidationDistanceFloor,
                        format!("liq distance {distance} < floor {}", floor.get()),
                    ));
                }
            }
        }
        if let Some(ceiling) = self.limits.margin_utilisation_ceiling {
            if avail > Decimal::ZERO {
                // Share of available margin the position's maintenance requirement consumes.
                let util = (mag * mmr) / avail;
                if util > ceiling.get() {
                    breaches.push(LimitBreach::with_default_outcome(
                        LimitKind::MarginUtilisationCeiling,
                        format!("margin util {util} > ceiling {}", ceiling.get()),
                    ));
                }
            } else if mag > Decimal::ZERO {
                breaches.push(LimitBreach::with_default_outcome(
                    LimitKind::MarginUtilisationCeiling,
                    format!("no available margin ({avail}) with notional {mag}"),
                ));
            }
        }
        // QE-447: %ADV participation guard. Reject a delta-close order whose participation
        // `|order notional| / ADV` exceeds the configured fraction of a rolling hourly ADV. A flat order
        // (`mag == 0`) has zero participation and is always safe — no division is performed. When the cap is
        // configured but ADV is unknown / non-positive / the division overflows with a live order, we
        // **fail closed** (reject), matching the liquidation/margin caps' handling of a degenerate required
        // input. `checked_div` keeps the order path panic-free (QE-268) — never the panicking `/`.
        if let Some(cap) = self.limits.max_participation {
            if mag > Decimal::ZERO {
                match self.adv.map(Notional::get) {
                    Some(adv) if adv > Decimal::ZERO => match mag.checked_div(adv) {
                        Some(participation) if participation > cap.get() => {
                            breaches.push(LimitBreach::with_default_outcome(
                                LimitKind::MaxParticipation,
                                format!("participation {participation} > cap {}", cap.get()),
                            ));
                        }
                        Some(_) => {}
                        None => breaches.push(LimitBreach::with_default_outcome(
                            LimitKind::MaxParticipation,
                            format!("participation undefined (notional {mag} / adv {adv})"),
                        )),
                    },
                    _ => breaches.push(LimitBreach::with_default_outcome(
                        LimitKind::MaxParticipation,
                        format!("adv unknown/non-positive with notional {mag}"),
                    )),
                }
            }
        }

        let verdict = match most_severe(&breaches) {
            // Currently unreachable: no enforced pre-trade cap defaults to Halt (DrawdownCap → Halt is the
            // QE-212 breaker / QE-216 kill path). Kept so adding a Halt-kind later needs no change here.
            Some(LimitOutcome::Halt) => PreTradeVerdict::Halt,
            Some(LimitOutcome::Reject) => PreTradeVerdict::Reject,
            Some(LimitOutcome::Clamp) => {
                // Re-apply the original sign to the clamped magnitude (a flat 0 stays 0).
                let signed = if notional.is_sign_negative() {
                    -clamp_mag
                } else {
                    clamp_mag
                };
                PreTradeVerdict::Send(Notional::new(signed))
            }
            None => PreTradeVerdict::Send(target.notional),
        };
        PreTradeDecision { verdict, breaches }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::Direction;
    use qe_risk::Leverage;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn n(s: &str) -> Notional {
        Notional::new(dec(s))
    }
    fn frac(s: &str) -> Fraction {
        Fraction::new(dec(s)).unwrap()
    }
    fn lev(s: &str) -> Leverage {
        Leverage::new(dec(s)).unwrap()
    }
    fn target(s: &str) -> TargetPosition {
        // Single-instrument target: gross == |net| by construction (preserves pre-QE-418 behaviour).
        TargetPosition::single(n(s))
    }
    fn capital(equity: &str, margin: &str) -> CapitalView {
        CapitalView {
            equity: n(equity),
            available_margin: n(margin),
        }
    }
    fn breach_of(dec: &PreTradeDecision, kind: LimitKind) -> bool {
        dec.breaches.iter().any(|b| b.kind == kind)
    }

    /// A target inside every cap is sent unchanged, with no breaches.
    #[test]
    fn within_all_caps_sends_target_unchanged() {
        let limits = RiskLimits {
            max_notional: Some(n("100000")),
            max_leverage: Some(lev("10")),
            max_gross_exposure: Some(n("100000")),
            max_net_exposure: Some(n("100000")),
            liquidation_distance_floor: Some(frac("0.02")),
            margin_utilisation_ceiling: Some(frac("0.8")),
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0.005"));
        let decision = gov.check(target("5000"), capital("10000", "10000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Send(n("5000")));
        assert!(decision.breaches.is_empty());
    }

    /// AC (clamp): an oversized notional is clamped to the cap, sign preserved.
    #[test]
    fn oversized_notional_is_clamped() {
        let limits = RiskLimits {
            max_notional: Some(n("8000")),
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0"));

        let long = gov.check(target("12000"), capital("100000", "100000"));
        assert_eq!(long.verdict, PreTradeVerdict::Send(n("8000")));
        assert!(breach_of(&long, LimitKind::MaxNotional));

        // Sign is preserved for a short.
        let short = gov.check(target("-12000"), capital("100000", "100000"));
        assert_eq!(short.verdict, PreTradeVerdict::Send(n("-8000")));
    }

    /// Excess leverage clamps the notional to `max_leverage × equity`.
    #[test]
    fn excess_leverage_is_clamped() {
        let limits = RiskLimits {
            max_leverage: Some(lev("3")),
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0"));
        // equity 10_000, max leverage 3 → cap notional 30_000; ask for 50_000.
        let decision = gov.check(target("50000"), capital("10000", "10000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Send(n("30000")));
        assert!(breach_of(&decision, LimitKind::MaxLeverage));
    }

    /// AC (headline): a target with an unsafe liquidation distance is rejected, not sent.
    #[test]
    fn unsafe_liquidation_distance_is_rejected() {
        let limits = RiskLimits {
            liquidation_distance_floor: Some(frac("0.05")),
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0.005"));
        // equity 10_000, notional 500_000 → E/mag = 0.02, minus mmr 0.005 = 0.015 < floor 0.05.
        let decision = gov.check(target("500000"), capital("10000", "1000000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&decision, LimitKind::LiquidationDistanceFloor));

        // A safely-sized target passes the floor (E/mag = 0.5 − 0.005 = 0.495 ≥ 0.05).
        let safe = gov.check(target("20000"), capital("10000", "1000000"));
        assert!(!breach_of(&safe, LimitKind::LiquidationDistanceFloor));
    }

    /// Each Reject cap (gross, net, margin utilisation) independently yields `Reject`.
    #[test]
    fn gross_and_net_and_margin_breaches_reject() {
        let gross = PreTradeGovernor::new(
            RiskLimits {
                max_gross_exposure: Some(n("1000")),
                ..RiskLimits::default()
            },
            frac("0"),
        )
        .check(target("2000"), capital("100000", "100000"));
        assert_eq!(gross.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&gross, LimitKind::MaxGrossExposure));

        let net = PreTradeGovernor::new(
            RiskLimits {
                max_net_exposure: Some(n("1000")),
                ..RiskLimits::default()
            },
            frac("0"),
        )
        .check(target("-2000"), capital("100000", "100000"));
        assert_eq!(net.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&net, LimitKind::MaxNetExposure));

        // Margin utilisation: (mag 100_000 × mmr 0.01) / avail 500 = 2.0 > ceiling 0.5.
        let margin = PreTradeGovernor::new(
            RiskLimits {
                margin_utilisation_ceiling: Some(frac("0.5")),
                ..RiskLimits::default()
            },
            frac("0.01"),
        )
        .check(target("100000"), capital("100000", "500"));
        assert_eq!(margin.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&margin, LimitKind::MarginUtilisationCeiling));
    }

    /// AC (QE-418): a hedged book — `long = short = X`, so net 0 but gross 2X — breaches a `MaxGross < 2X`
    /// cap while **passing** the net cap. The gross cap must see true gross exposure, not `|net|`.
    #[test]
    fn hedged_book_breaches_gross_cap_while_passing_net_cap() {
        let x = dec("10000");
        // net 0, gross 2X = 20_000.
        let hedged = TargetPosition {
            notional: n("0"),
            gross: Notional::new(x * dec("2")),
        };
        let gov = PreTradeGovernor::new(
            RiskLimits {
                max_gross_exposure: Some(n("15000")), // < 2X → breached
                max_net_exposure: Some(n("1")),       // net 0 ≤ 1 → passes
                ..RiskLimits::default()
            },
            frac("0"),
        );
        let decision = gov.check(hedged, capital("100000", "100000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Reject);
        assert!(
            breach_of(&decision, LimitKind::MaxGrossExposure),
            "gross 2X must breach the MaxGross < 2X cap"
        );
        assert!(
            !breach_of(&decision, LimitKind::MaxNetExposure),
            "net 0 must pass the net cap — the gross breach is not the net cap firing"
        );

        // Same gross, but under a cap that admits it → no gross breach (proves the cap, not the magnitude).
        let loose = PreTradeGovernor::new(
            RiskLimits {
                max_gross_exposure: Some(n("25000")), // > 2X → passes
                ..RiskLimits::default()
            },
            frac("0"),
        )
        .check(hedged, capital("100000", "100000"));
        assert!(!breach_of(&loose, LimitKind::MaxGrossExposure));
    }

    /// Single-instrument parity (QE-418): with `gross == |net|`, the gross cap fires exactly as it did before
    /// — for both a long and a short — so no existing single-instrument behaviour changes.
    #[test]
    fn single_instrument_gross_equals_net_magnitude() {
        let gov = PreTradeGovernor::new(
            RiskLimits {
                max_gross_exposure: Some(n("1000")),
                ..RiskLimits::default()
            },
            frac("0"),
        );
        for signed in ["2000", "-2000"] {
            // `single` sets gross = |net| = 2000 > cap 1000 → breach, regardless of side.
            let decision = gov.check(target(signed), capital("100000", "100000"));
            assert_eq!(decision.verdict, PreTradeVerdict::Reject, "side {signed}");
            assert!(breach_of(&decision, LimitKind::MaxGrossExposure));
        }
        // A single-instrument target within the cap does not breach gross.
        assert!(!breach_of(
            &gov.check(target("500"), capital("100000", "100000")),
            LimitKind::MaxGrossExposure
        ));
    }

    /// A `Reject` breach outranks a `Clamp` breach: a target that is both oversized and unsafe is rejected,
    /// not clamped-and-sent.
    #[test]
    fn reject_outranks_clamp() {
        let limits = RiskLimits {
            max_notional: Some(n("100000")),                // clamp
            liquidation_distance_floor: Some(frac("0.05")), // reject
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0.005"));
        // 500_000 > max_notional 100_000 (clamp) AND E/mag 0.02 − 0.005 < 0.05 (reject).
        let decision = gov.check(target("500000"), capital("10000", "1000000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&decision, LimitKind::MaxNotional));
        assert!(breach_of(&decision, LimitKind::LiquidationDistanceFloor));
    }

    /// The severity reducer prefers `Halt` over `Reject`/`Clamp` (contract-general Halt path).
    #[test]
    fn verdict_severity_prefers_halt() {
        let breaches = vec![
            LimitBreach::with_default_outcome(LimitKind::MaxNotional, "clamp"), // Clamp
            LimitBreach::with_default_outcome(LimitKind::MaxGrossExposure, "reject"), // Reject
            LimitBreach::with_default_outcome(LimitKind::DrawdownCap, "halt"),  // Halt
        ];
        assert_eq!(most_severe(&breaches), Some(LimitOutcome::Halt));
        assert_eq!(
            most_severe(&breaches[..2]),
            Some(LimitOutcome::Reject),
            "without a Halt, Reject wins"
        );
        assert_eq!(most_severe(&[]), None);
    }

    /// A flat target passes every cap, even tight ones.
    #[test]
    fn flat_target_passes() {
        let limits = RiskLimits {
            max_notional: Some(n("1")),
            liquidation_distance_floor: Some(frac("0.5")),
            margin_utilisation_ceiling: Some(frac("0.01")),
            ..RiskLimits::default()
        };
        let gov = PreTradeGovernor::new(limits, frac("0.01"));
        let decision = gov.check(target("0"), capital("10000", "0"));
        assert_eq!(decision.verdict, PreTradeVerdict::Send(Notional::ZERO));
        assert!(decision.breaches.is_empty());
        // Sanity: a flat target has no direction.
        assert_eq!(target("0").direction(), None::<Direction>);
    }

    /// The catastrophic-account edges: a live position with no positive equity / no available margin. These
    /// are the paths a risk governor most needs to prove correct.
    #[test]
    fn degenerate_capital_is_handled_safely() {
        // (a) Zero/negative equity with a leverage cap → leverage is infinite → clamp to flat.
        let lev_gov = PreTradeGovernor::new(
            RiskLimits {
                max_leverage: Some(lev("3")),
                ..RiskLimits::default()
            },
            frac("0"),
        );
        for equity in ["0", "-100"] {
            let decision = lev_gov.check(target("5000"), capital(equity, "100000"));
            assert_eq!(
                decision.verdict,
                PreTradeVerdict::Send(Notional::ZERO),
                "no positive equity clamps the position to flat (equity {equity})"
            );
            assert!(breach_of(&decision, LimitKind::MaxLeverage));
        }

        // (b) Zero/negative equity with a liquidation-distance floor → distance ≤ −mmr < floor → reject.
        let liq_gov = PreTradeGovernor::new(
            RiskLimits {
                liquidation_distance_floor: Some(frac("0.05")),
                ..RiskLimits::default()
            },
            frac("0.005"),
        );
        for equity in ["0", "-100"] {
            let decision = liq_gov.check(target("5000"), capital(equity, "100000"));
            assert_eq!(decision.verdict, PreTradeVerdict::Reject);
            assert!(breach_of(&decision, LimitKind::LiquidationDistanceFloor));
        }

        // (c) No available margin with a position and a margin-utilisation ceiling → reject.
        let margin_gov = PreTradeGovernor::new(
            RiskLimits {
                margin_utilisation_ceiling: Some(frac("0.5")),
                ..RiskLimits::default()
            },
            frac("0.01"),
        );
        for avail in ["0", "-50"] {
            let decision = margin_gov.check(target("5000"), capital("100000", avail));
            assert_eq!(decision.verdict, PreTradeVerdict::Reject);
            assert!(breach_of(&decision, LimitKind::MarginUtilisationCeiling));
        }
    }

    /// A governor carrying only a participation cap and a rolling hourly ADV.
    fn participation_gov(cap: &str, adv: Option<&str>) -> PreTradeGovernor {
        PreTradeGovernor::new(
            RiskLimits {
                max_participation: Some(frac(cap)),
                ..RiskLimits::default()
            },
            frac("0"),
        )
        .with_adv(adv.map(n))
    }

    /// AC (headline): a delta-close order exceeding the configured %ADV is **Rejected**, with the participation
    /// value carried in the breach detail; a within-threshold order passes.
    #[test]
    fn over_participation_is_rejected_within_passes() {
        // ADV 1_000_000; cap 1% → participation limit 10_000 notional.
        let gov = participation_gov("0.01", Some("1000000"));

        // 25_000 / 1_000_000 = 2.5% > 1% → reject.
        let over = gov.check(target("25000"), capital("100000", "100000"));
        assert_eq!(over.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&over, LimitKind::MaxParticipation));
        let detail = &over
            .breaches
            .iter()
            .find(|b| b.kind == LimitKind::MaxParticipation)
            .map(|b| b.detail.clone())
            .unwrap_or_default();
        assert!(
            detail.contains("0.025"),
            "breach detail must carry the participation 0.025, got: {detail}"
        );

        // 5_000 / 1_000_000 = 0.5% ≤ 1% → no participation breach, sent unchanged.
        let within = gov.check(target("5000"), capital("100000", "100000"));
        assert_eq!(within.verdict, PreTradeVerdict::Send(n("5000")));
        assert!(!breach_of(&within, LimitKind::MaxParticipation));

        // Sign is irrelevant — participation is on the magnitude; a short of the same size also breaches.
        let short = gov.check(target("-25000"), capital("100000", "100000"));
        assert_eq!(short.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&short, LimitKind::MaxParticipation));
    }

    /// Default `None` participation cap = no guard: even a huge order with ADV present is never rejected by the
    /// participation guard — current behaviour is unchanged unless the cap is configured.
    #[test]
    fn default_none_participation_is_no_guard() {
        // No `max_participation` in the limits; ADV supplied but must be ignored (cap is the gate).
        let gov = PreTradeGovernor::new(RiskLimits::default(), frac("0")).with_adv(Some(n("1")));
        let decision = gov.check(target("1000000"), capital("100000", "100000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Send(n("1000000")));
        assert!(!breach_of(&decision, LimitKind::MaxParticipation));
        assert!(decision.breaches.is_empty());
    }

    /// Fail-safe (no panic, no divide-by-zero): a configured cap with unknown / zero / negative ADV and a live
    /// order **fails closed** (reject); a flat order with unknown ADV passes (zero participation).
    #[test]
    fn participation_fail_safe_on_zero_or_unknown_adv() {
        // (a) ADV unknown (None) with a live order → reject, no panic.
        let unknown =
            participation_gov("0.01", None).check(target("5000"), capital("100000", "100000"));
        assert_eq!(unknown.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&unknown, LimitKind::MaxParticipation));

        // (b) ADV zero / negative with a live order → reject, no divide-by-zero.
        for adv in ["0", "-1000"] {
            let decision = participation_gov("0.01", Some(adv))
                .check(target("5000"), capital("100000", "100000"));
            assert_eq!(decision.verdict, PreTradeVerdict::Reject, "adv {adv}");
            assert!(breach_of(&decision, LimitKind::MaxParticipation));
        }

        // (c) A flat order has zero participation → passes even with unknown ADV (no division performed).
        let flat = participation_gov("0.01", None).check(target("0"), capital("100000", "100000"));
        assert_eq!(flat.verdict, PreTradeVerdict::Send(Notional::ZERO));
        assert!(!breach_of(&flat, LimitKind::MaxParticipation));
    }

    /// Policy parity with the sibling caps: the participation breach is a `Reject`, so it outranks a
    /// co-occurring `MaxNotional` clamp (the order is rejected, not clamped-and-sent) — same reducer.
    #[test]
    fn participation_reject_outranks_clamp() {
        let gov = PreTradeGovernor::new(
            RiskLimits {
                max_notional: Some(n("100000")),       // clamp
                max_participation: Some(frac("0.01")), // reject
                ..RiskLimits::default()
            },
            frac("0"),
        )
        .with_adv(Some(n("1000000")));
        // 500_000 > max_notional 100_000 (clamp) AND 500_000 / 1_000_000 = 50% > 1% (reject).
        let decision = gov.check(target("500000"), capital("100000", "100000"));
        assert_eq!(decision.verdict, PreTradeVerdict::Reject);
        assert!(breach_of(&decision, LimitKind::MaxNotional));
        assert!(breach_of(&decision, LimitKind::MaxParticipation));
    }
}
