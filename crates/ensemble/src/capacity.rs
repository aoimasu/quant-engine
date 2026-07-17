//! Capacity analysis gating ensemble weights (QE-128).
//!
//! Ensemble weights are fiction at size if per-strategy capacity is ignored — a high-turnover scalper
//! may have edge at $10k and none at $1M. [`capacity`] estimates the AUM at which a strategy's
//! size-dependent impact erodes its edge to a retained floor, and [`cap_weights`] water-fills the unit
//! weight budget so no strategy is allocated more capital than its modelled capacity at the configured
//! target AUM.
//!
//! The impact form is QE-109's (`cost = notional · (half_spread + impact · qty)`), but the coefficients
//! are **not** imported from `qe_wfo::friction::SlippageModel` — the search⟂portfolio firewall
//! (QE-001/QE-132) forbids `qe-ensemble → qe-wfo`. Instead both sides **derive** their coefficients from
//! the one upstream [`SlippageCalibration`](qe_risk::SlippageCalibration) (QE-431): capacity reads the
//! canonical per-$ `impact_per_notional` directly and friction converts it to per-contract, so the two can
//! never drift (a coefficient-parity test proves it). Live impact measurement is out of scope.

use qe_risk::SlippageCalibration;

/// Default fraction of gross edge that must remain at capacity (`0` = capacity is where edge hits zero).
pub const DEFAULT_EDGE_RETENTION: f64 = 0.0;

/// A strategy's per-period economics, the inputs to its [`capacity`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyProfile {
    /// Per-period gross expected return (before impact), as a fraction of deployed capital.
    pub gross_edge: f64,
    /// Per-period turnover — the fraction of AUM traded each period.
    pub turnover: f64,
}

/// The impact model used to bound capacity (QE-109's form, parameterised on the portfolio side).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityModel {
    /// Half the bid/ask spread, as a fraction of notional (the spread-cross term).
    pub half_spread: f64,
    /// Size-impact coefficient per dollar of traded notional (the size-dependent term).
    pub impact_coeff: f64,
    /// Fraction of gross edge that must remain at capacity.
    pub edge_retention: f64,
}

impl Default for CapacityModel {
    fn default() -> Self {
        // QE-431: derived from the one content-addressed [`SlippageCalibration`], not authored here — so
        // no magic slippage/impact literal remains on the selection path and capacity can never drift from
        // the friction side, which derives from the same calibration. The coefficients are byte-identical
        // to the pre-QE-431 literals (`half_spread = 1e-4`, `impact_coeff = 2e-9`).
        CapacityModel::from_calibration(&SlippageCalibration::default())
    }
}

impl CapacityModel {
    /// The QE-128 default impact model.
    #[must_use]
    pub fn with_defaults() -> Self {
        CapacityModel::default()
    }

    /// Derive the capacity impact model from the shared [`SlippageCalibration`] (QE-431): `half_spread`
    /// and the per-$ `impact_coeff` are taken directly from the calibration's canonical per-notional
    /// coefficients; `edge_retention` is capacity-specific and keeps its [`DEFAULT_EDGE_RETENTION`].
    #[must_use]
    pub fn from_calibration(cal: &SlippageCalibration) -> Self {
        CapacityModel {
            half_spread: cal.half_spread_f64(),
            impact_coeff: cal.impact_per_notional_f64(),
            edge_retention: DEFAULT_EDGE_RETENTION,
        }
    }

    /// The per-fill slippage cost of trading `notional` dollars under this model, in QE-109's per-notional
    /// form `notional · (half_spread + impact_coeff · notional)` — the same shape friction charges. Used by
    /// the coefficient-parity check that friction & capacity agree for identical inputs (QE-431).
    #[must_use]
    pub fn slippage_cost(&self, notional: f64) -> f64 {
        notional * (self.half_spread + self.impact_coeff * notional)
    }
}

/// Modelled capacity (in dollars) of a strategy: the AUM `W*` at which its net per-period edge
///
/// ```text
/// net(W) = gross_edge − turnover·half_spread − impact_coeff·turnover²·W
/// ```
///
/// falls to the retained floor `edge_retention · gross_edge` (QE-128/D1):
///
/// ```text
/// W* = (gross_edge·(1 − edge_retention) − turnover·half_spread) / (impact_coeff · turnover²)
/// ```
///
/// Returns `0.0` if the spread-cross alone already erodes the usable edge (uneconomic at any size), and
/// `f64::INFINITY` if there is no size impact (`impact_coeff·turnover² = 0` ⇒ no size limit). Because the
/// size term scales with `turnover²`, capacity falls quadratically in turnover.
#[must_use]
pub fn capacity(profile: &StrategyProfile, model: &CapacityModel) -> f64 {
    let turnover = profile.turnover.max(0.0);
    let usable_edge = profile.gross_edge * (1.0 - model.edge_retention);
    let numerator = usable_edge - turnover * model.half_spread;
    if numerator <= 0.0 {
        return 0.0; // uneconomic even at zero size
    }
    let impact_term = model.impact_coeff * turnover * turnover;
    if impact_term <= 0.0 {
        return f64::INFINITY; // no size impact ⇒ unbounded capacity
    }
    numerator / impact_term
}

/// Cap ensemble `weights` so no strategy is allocated more than its `capacities` permit at `target_aum`
/// (QE-128/D2). Each strategy's max weight is `capacity_i / target_aum`; the unit weight budget is
/// distributed proportionally to the input weights by **water-filling** — any strategy whose share would
/// exceed its cap is fixed at the cap and its freed budget redistributed to the uncapped strategies,
/// repeated until stable. If the caps cannot absorb the whole budget the remainder stays uninvested
/// (the returned weights sum to `< 1`). `weights` and `capacities` must be the same length; a
/// non-positive `target_aum` disables capping.
#[must_use]
pub fn cap_weights(weights: &[f64], capacities: &[f64], target_aum: f64) -> Vec<f64> {
    let n = weights.len().min(capacities.len());
    let mut alloc = vec![0.0; weights.len()];
    if n == 0 {
        return alloc;
    }
    if target_aum <= 0.0 {
        // No AUM scale ⇒ no capacity bound; pass the weights through.
        alloc[..n].copy_from_slice(&weights[..n]);
        return alloc;
    }

    // Per-strategy max weight = capacity / AUM, clamped to [0, 1].
    let caps: Vec<f64> = (0..n)
        .map(|i| (capacities[i] / target_aum).clamp(0.0, 1.0))
        .collect();

    let total_budget: f64 = weights[..n]
        .iter()
        .map(|w| w.max(0.0))
        .sum::<f64>()
        .min(1.0);
    let mut remaining = total_budget;
    let mut active: Vec<usize> = (0..n)
        .filter(|&i| weights[i] > 0.0 && caps[i] > 0.0)
        .collect();

    loop {
        let active_w: f64 = active.iter().map(|&i| weights[i]).sum();
        if active.is_empty() || active_w <= 0.0 || remaining <= 0.0 {
            break;
        }
        // Which active strategies would exceed their cap at the proportional share?
        let newly_capped: Vec<usize> = active
            .iter()
            .copied()
            .filter(|&i| remaining * weights[i] / active_w >= caps[i])
            .collect();
        if newly_capped.is_empty() {
            // Everyone fits — give each its proportional share and finish.
            for &i in &active {
                alloc[i] = remaining * weights[i] / active_w;
            }
            break;
        }
        for &i in &newly_capped {
            alloc[i] = caps[i];
            remaining -= caps[i];
        }
        active.retain(|i| !newly_capped.contains(i));
    }
    alloc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    #[test]
    fn default_is_derived_from_the_shared_calibration_no_magic_literal() {
        // QE-431 AC3: the selection-path capacity model authors no slippage/impact literal — it is exactly
        // the one derived from `SlippageCalibration::default()` (the single source of truth).
        let cal = SlippageCalibration::default();
        let m = CapacityModel::default();
        assert_eq!(m, CapacityModel::from_calibration(&cal));
        // Byte-identical to the pre-QE-431 literals.
        approx(m.half_spread, 0.0001);
        approx(m.impact_coeff, 2e-9);
        approx(m.edge_retention, DEFAULT_EDGE_RETENTION);
    }

    #[test]
    fn capacity_falls_quadratically_with_turnover() {
        let model = CapacityModel::with_defaults();
        let slow = capacity(
            &StrategyProfile {
                gross_edge: 0.001,
                turnover: 0.1,
            },
            &model,
        );
        let fast = capacity(
            &StrategyProfile {
                gross_edge: 0.001,
                turnover: 2.0,
            },
            &model,
        );
        assert!(slow.is_finite() && fast.is_finite());
        assert!(
            slow > fast,
            "lower turnover ⇒ higher capacity: slow={slow} fast={fast}"
        );
        // 20× the turnover ⇒ roughly 1/400 the capacity (the impact term ∝ turnover²; the spread term
        // shifts it slightly), so the gap is at least two orders of magnitude.
        assert!(
            slow > fast * 100.0,
            "capacity gap should be ≫ linear: slow={slow} fast={fast}"
        );
    }

    #[test]
    fn uneconomic_and_zero_impact_guards() {
        // Spread-cross alone eats the edge (huge turnover, tiny edge) ⇒ capacity 0.
        let none = capacity(
            &StrategyProfile {
                gross_edge: 0.0001,
                turnover: 100.0,
            },
            &CapacityModel::with_defaults(),
        );
        assert_eq!(none, 0.0);
        // No size impact ⇒ unbounded capacity.
        let unbounded = capacity(
            &StrategyProfile {
                gross_edge: 0.001,
                turnover: 1.0,
            },
            &CapacityModel {
                impact_coeff: 0.0,
                ..CapacityModel::with_defaults()
            },
        );
        assert_eq!(unbounded, f64::INFINITY);
    }

    #[test]
    fn high_turnover_weight_is_capped_at_capacity_at_target_aum() {
        let model = CapacityModel::with_defaults();
        // Strategy 0: high-turnover scalper. gross 0.001, turnover 2 → capacity $100k.
        let scalper = StrategyProfile {
            gross_edge: 0.001,
            turnover: 2.0,
        };
        // Strategy 1: low-turnover, huge capacity.
        let slow = StrategyProfile {
            gross_edge: 0.001,
            turnover: 0.1,
        };
        let cap_scalper = capacity(&scalper, &model);
        let cap_slow = capacity(&slow, &model);
        approx(cap_scalper, 100_000.0); // (0.001 − 2·0.0001) / (2e-9·4) = 0.0008 / 8e-9
        let target_aum = 1_000_000.0; // $1M: above the scalper's $100k capacity

        let weights = [0.5, 0.5]; // equal nominal weights
        let capped = cap_weights(&weights, &[cap_scalper, cap_slow], target_aum);

        // The scalper is capped down to capacity / AUM = 0.1, strictly below its nominal 0.5 …
        approx(capped[0], cap_scalper / target_aum);
        assert!(capped[0] < weights[0]);
        // … its dollar allocation equals its modelled capacity …
        approx(capped[0] * target_aum, cap_scalper);
        // … and the freed weight flows to the high-capacity strategy (which is not capped).
        assert!(capped[1] > weights[1]);
        approx(capped[0] + capped[1], 1.0); // fully invested (slow strategy has the capacity)
    }

    #[test]
    fn no_capping_below_capacity() {
        let model = CapacityModel::with_defaults();
        let p = StrategyProfile {
            gross_edge: 0.001,
            turnover: 0.5,
        };
        let cap = capacity(&p, &model);
        let weights = [0.5, 0.5];
        // Target AUM far below the (shared) capacity ⇒ both caps ≥ 1 ⇒ no change.
        let target_aum = cap / 1000.0;
        let capped = cap_weights(&weights, &[cap, cap], target_aum);
        approx(capped[0], 0.5);
        approx(capped[1], 0.5);
    }

    #[test]
    fn capacity_constrained_ensemble_leaves_cash_uninvested() {
        // Both strategies are tiny-capacity at this AUM, so the caps cannot absorb the full budget.
        let target_aum = 1_000_000.0;
        let capped = cap_weights(&[0.5, 0.5], &[50_000.0, 50_000.0], target_aum);
        approx(capped[0], 0.05);
        approx(capped[1], 0.05);
        approx(capped[0] + capped[1], 0.1); // 90% of AUM stays in cash — capacity-constrained
    }
}
