//! Capacity analysis gating ensemble weights (QE-128).
//!
//! Ensemble weights are fiction at size if per-strategy capacity is ignored — a high-turnover scalper
//! may have edge at $10k and none at $1M. [`capacity`] estimates the AUM at which a strategy's
//! size-dependent impact erodes its edge to a retained floor, and [`cap_weights`] water-fills the unit
//! weight budget so no strategy is allocated more capital than its modelled capacity at the configured
//! target AUM.
//!
//! The impact form is QE-440's **concave √-in-participation** law
//! (`cost = notional · (half_spread + impact_coeff · (traded/ADV)^β)`), but the coefficients are **not**
//! imported from `qe_wfo::friction::SlippageModel` — the search⟂portfolio firewall (QE-001/QE-132) forbids
//! `qe-ensemble → qe-wfo`. Instead both sides **derive** their coefficients from the one upstream
//! [`SlippageCalibration`](qe_risk::SlippageCalibration) (QE-431): the participation `impact_coeff` and β
//! are dimensionless, so both sides read them **verbatim** — no per-contract vs per-$ conversion (QE-440
//! resolves the reviewer's unit flag), so the two can never drift (a coefficient-parity test proves it).
//! Live impact measurement is out of scope.

use qe_risk::SlippageCalibration;

/// Default fraction of gross edge that must remain at capacity (`0` = capacity is where edge hits zero).
pub const DEFAULT_EDGE_RETENTION: f64 = 0.0;

/// Default EWMA variance decay constant `λ` for [`inverse_vol_seed`] (QE-443): the RiskMetrics-standard
/// `0.94`. This is the **single** free parameter of the inverse-vol seed — the estimation knob the Max
/// Dama panel flagged (variance is the one moment a short EWMA predicts well, but seeding by it
/// reintroduces an estimated variance that pure `1/N` avoids). Kept low-parameter and deterministic; the
/// seed is **opt-in** and defaults off (see [`SeedWeighting`]), so this value never moves a golden unless a
/// caller deliberately enables inverse-vol seeding.
pub const DEFAULT_EWMA_DECAY: f64 = 0.94;

/// A strategy's per-period economics, the inputs to its [`capacity`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyProfile {
    /// Per-period gross expected return (before impact), as a fraction of deployed capital.
    pub gross_edge: f64,
    /// Per-period turnover — the fraction of AUM traded each period.
    pub turnover: f64,
    /// Rolling ADV in **dollars** of the traded instrument (QE-440), keying the participation impact
    /// `u = traded_notional / adv_notional`. Non-finite / non-positive ⇒ no modellable size cap.
    pub adv_notional: f64,
}

/// The impact model used to bound capacity — QE-440's **concave (√-in-participation)** form, keyed off the
/// same participation coefficient friction uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityModel {
    /// Half the bid/ask spread, as a fraction of notional (the spread-cross term).
    pub half_spread: f64,
    /// Participation impact coefficient — the impact fraction of notional at `u = 1` (100 % of ADV).
    /// Dimensionless and asset-portable (shared verbatim with friction via the calibration).
    pub impact_coeff: f64,
    /// Impact exponent β — the concavity of impact in participation (`u^β`, `β < 1`).
    pub impact_exponent: f64,
    /// Fraction of gross edge that must remain at capacity.
    pub edge_retention: f64,
}

impl Default for CapacityModel {
    fn default() -> Self {
        // QE-431/QE-440: derived from the one content-addressed [`SlippageCalibration`], not authored here
        // — so no magic slippage/impact literal remains on the selection path and capacity can never drift
        // from the friction side, which keys impact off the **same** participation coefficient + β.
        CapacityModel::from_calibration(&SlippageCalibration::default())
    }
}

impl CapacityModel {
    /// The default impact model.
    #[must_use]
    pub fn with_defaults() -> Self {
        CapacityModel::default()
    }

    /// Derive the capacity impact model from the shared [`SlippageCalibration`] (QE-431/QE-440): the
    /// `half_spread`, participation `impact_coeff`, and exponent β are taken verbatim (dimensionless, no
    /// per-$ conversion); `edge_retention` is capacity-specific and keeps its [`DEFAULT_EDGE_RETENTION`].
    #[must_use]
    pub fn from_calibration(cal: &SlippageCalibration) -> Self {
        CapacityModel {
            half_spread: cal.half_spread_f64(),
            impact_coeff: cal.impact_coeff_f64(),
            impact_exponent: cal.impact_exponent_f64(),
            edge_retention: DEFAULT_EDGE_RETENTION,
        }
    }

    /// The per-fill slippage cost of trading `notional` dollars against `adv_notional` dollars of rolling
    /// ADV, in the participation form `notional · (half_spread + impact_coeff · (notional/adv_notional)^β)`
    /// — the same shape friction charges. Used by the coefficient-parity check that friction & capacity
    /// agree for identical inputs (QE-431/QE-440). A non-positive `adv_notional` charges the spread-cross
    /// only.
    ///
    /// Determinism note: the `powf` is `f64`; capacity feeds the sealed ensemble weights, which are rounded
    /// to `hash_stable` (12 dp) before hashing, neutralising sub-ULP cross-platform `powf` drift.
    #[must_use]
    pub fn slippage_cost(&self, notional: f64, adv_notional: f64) -> f64 {
        let participation = if adv_notional > 0.0 {
            notional / adv_notional
        } else {
            0.0
        };
        let impact = if participation > 0.0 {
            self.impact_coeff * participation.powf(self.impact_exponent)
        } else {
            0.0
        };
        notional * (self.half_spread + impact)
    }
}

/// Modelled capacity (in dollars) of a strategy under the QE-440 concave impact model: the AUM `W*` at
/// which its net per-period edge
///
/// ```text
/// net(W) = gross_edge − turnover·half_spread − turnover·impact_coeff·(turnover·W / ADV$)^β
/// ```
///
/// (traded notional per period `= turnover·W`, participation `u = turnover·W / ADV$`) falls to the
/// retained floor `edge_retention · gross_edge` (QE-128/D1). Solving for `W`:
///
/// ```text
/// W* = (ADV$ / turnover) · [ (gross_edge·(1 − edge_retention) − turnover·half_spread)
///                            / (turnover · impact_coeff) ]^(1/β)
/// ```
///
/// Returns `0.0` if the spread-cross alone already erodes the usable edge (uneconomic at any size), and
/// `f64::INFINITY` if there is no modellable size cap (`turnover·impact_coeff = 0`, or a non-finite /
/// non-positive `ADV$`). Because participation carries `W` through a `β < 1` power, capacity scales
/// **linearly with ADV** and falls **super-linearly** with turnover.
///
/// Determinism note: the `powf(1/β)` is `f64`; capacity feeds the sealed ensemble weights, which are
/// rounded to `hash_stable` (12 dp) before hashing, neutralising sub-ULP cross-platform `powf` drift.
#[must_use]
pub fn capacity(profile: &StrategyProfile, model: &CapacityModel) -> f64 {
    let turnover = profile.turnover.max(0.0);
    let usable_edge =
        profile.gross_edge * (1.0 - model.edge_retention) - turnover * model.half_spread;
    if usable_edge <= 0.0 {
        return 0.0; // uneconomic even at zero size (spread-cross alone eats the edge)
    }
    let impact_slope = turnover * model.impact_coeff;
    if impact_slope <= 0.0 || !profile.adv_notional.is_finite() || profile.adv_notional <= 0.0 {
        return f64::INFINITY; // no modellable size cap
    }
    // usable_edge = turnover·impact_coeff · u^β  ⇒  u = (usable_edge / impact_slope)^(1/β),
    // and u = turnover·W / ADV$  ⇒  W* = (ADV$ / turnover) · u.
    let participation = (usable_edge / impact_slope).powf(1.0 / model.impact_exponent);
    (profile.adv_notional / turnover) * participation
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

/// The single-`λ` EWMA (exponentially-weighted moving average) **variance** of a per-period return series
/// (QE-443, Dama §6.2 method 10 — variance is predictable via a short EWMA). A one-pass, fixed-order walk
/// oldest→newest: the mean and variance are each recursively blended by `decay` (`λ`), so recent
/// observations weigh more. Deterministic and schedule-independent (no parallel reduction). Returns `0.0`
/// for a series of `< 2` points (no estimable dispersion). `decay` is clamped to `[0, 1)`.
///
/// ```text
/// mean_t = λ·mean_{t-1} + (1−λ)·r_t
/// var_t  = λ·var_{t-1}  + (1−λ)·(r_t − mean_{t-1})^2
/// ```
///
/// (the deviation uses the *prior* mean, the standard EWMA-variance recursion, so a single observation
/// contributes no variance).
#[must_use]
pub fn ewma_variance(returns: &[f64], decay: f64) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let lambda = decay.clamp(0.0, 1.0);
    let mut mean = returns[0];
    let mut var = 0.0;
    for &r in &returns[1..] {
        let dev = r - mean;
        var = lambda * var + (1.0 - lambda) * dev * dev;
        mean = lambda * mean + (1.0 - lambda) * r;
    }
    var
}

/// Inverse-volatility (EWMA) risk-parity **seed** weights for a set of members' per-period return series
/// (QE-443). Each member's volatility is `vol_i = sqrt(`[`ewma_variance`]`(series_i, decay))`; the seed is
/// `w_i = (1/vol_i) / Σ_j (1/vol_j)`, so the vector sums to 1 and a **higher-vol** member gets a **lower**
/// seed weight (Dama §6.2: when volatilities differ, unequal weights cancel risk better than `1/N`).
///
/// This is a **seed only** — the caller layers the existing QE-128 [`cap_weights`] water-fill on top
/// (`cap_weights(inverse_vol_seed(series, decay), capacities, target_aum)`). It is deliberately **not** a
/// default: the panel judged inverse-vol a genuine trade-off, not a strict win over the OOS-robust `1/N`
/// (it reintroduces an estimated variance), so it is opt-in via [`SeedWeighting`].
///
/// **Reductions to `1/N`.** Equal-volatility members seed to exactly `1/N`. As a **degenerate guard**, if
/// any member's vol is non-finite or `≤ 0` (a flat or too-short series has no estimable variance), or the
/// inverse-vol sum is non-finite/non-positive, the whole vector falls back to equal weight `1/N` — an
/// unmodellable member vol defeats risk parity, so the OOS-robust `1/N` is preserved deterministically
/// rather than papered over with an epsilon. Empty input ⇒ empty.
///
/// Determinism note: the `1/vol` normalisation is `f64`; where the seed feeds the sealed vintage the caller
/// rounds the final (post-water-fill) weights to `hash_stable` (12 dp), the same treatment the capacity
/// weights already receive, so sub-ULP `sqrt`/division drift cannot change the sealed bytes.
#[must_use]
pub fn inverse_vol_seed(series: &[Vec<f64>], decay: f64) -> Vec<f64> {
    let k = series.len();
    if k == 0 {
        return Vec::new();
    }
    let equal = || vec![1.0 / k as f64; k];
    let inv_vols: Vec<f64> = series
        .iter()
        .map(|s| {
            let vol = ewma_variance(s, decay).sqrt();
            if vol.is_finite() && vol > 0.0 {
                1.0 / vol
            } else {
                f64::NAN // marks a member with no estimable, positive vol
            }
        })
        .collect();
    if inv_vols.iter().any(|v| !v.is_finite()) {
        return equal(); // any unmodellable member vol ⇒ preserve OOS-robust 1/N
    }
    let sum: f64 = inv_vols.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return equal();
    }
    inv_vols.into_iter().map(|v| v / sum).collect()
}

/// How the member weight **budget is seeded** before the QE-128 [`cap_weights`] water-fill (QE-443).
/// Defaults to [`SeedWeighting::Equal`] — the OOS-robust `1/N` the engine has always used — so the sealed
/// weights, the vintage id, and every golden are unchanged unless a caller deliberately opts in to
/// [`SeedWeighting::InverseVol`]. The panel resolution was a **split**: inverse-vol is a genuine
/// risk-cancellation vs variance-estimation trade-off, not a strict improvement, so it is offered, not
/// forced.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SeedWeighting {
    /// Equal weight `1/N` — the default, byte-identical to the historical seed.
    #[default]
    Equal,
    /// Inverse-volatility (EWMA) risk-parity seed with a single decay constant `λ` (see
    /// [`inverse_vol_seed`]). Opt-in.
    InverseVol {
        /// The EWMA variance decay constant `λ` (see [`DEFAULT_EWMA_DECAY`]).
        decay: f64,
    },
}

impl SeedWeighting {
    /// The seed weight budget for `k` members with per-period return `series` (aligned, `series.len() == k`
    /// for the inverse-vol path). [`SeedWeighting::Equal`] returns exactly `vec![1/k; k]` (byte-identical to
    /// the historical literal); [`SeedWeighting::InverseVol`] returns [`inverse_vol_seed`]. `k == 0` ⇒ empty.
    #[must_use]
    pub fn seed(&self, series: &[Vec<f64>], k: usize) -> Vec<f64> {
        if k == 0 {
            return Vec::new();
        }
        match *self {
            SeedWeighting::Equal => vec![1.0 / k as f64; k],
            SeedWeighting::InverseVol { decay } => inverse_vol_seed(series, decay),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} !~ {b}");
    }

    /// Rolling ADV ($) chosen so the scalper (gross 0.001, turnover 2) caps at exactly $100k under the
    /// default model: `W* = (ADV/turnover)·(usable/impact_slope)^(1/β)` with usable = 8e-4,
    /// impact_slope = 0.02, β = 0.5 ⇒ `W* = (1.25e8/2)·(0.04)^2 = 6.25e7·0.0016 = 1e5`.
    const ADV: f64 = 1.25e8;

    fn profile(gross_edge: f64, turnover: f64) -> StrategyProfile {
        StrategyProfile {
            gross_edge,
            turnover,
            adv_notional: ADV,
        }
    }

    #[test]
    fn default_is_derived_from_the_shared_calibration_no_magic_literal() {
        // QE-431/QE-440 AC: the selection-path capacity model authors no slippage/impact literal — it is
        // exactly the one derived from `SlippageCalibration::default()` (the single source of truth), with
        // the participation-keyed coefficient + β taken verbatim.
        let cal = SlippageCalibration::default();
        let m = CapacityModel::default();
        assert_eq!(m, CapacityModel::from_calibration(&cal));
        approx(m.half_spread, 0.0001);
        approx(m.impact_coeff, 0.01); // participation coefficient (1% at 100% ADV)
        approx(m.impact_exponent, 0.5); // √-law
        approx(m.edge_retention, DEFAULT_EDGE_RETENTION);
    }

    #[test]
    fn capacity_falls_superlinearly_with_turnover() {
        let model = CapacityModel::with_defaults();
        let slow = capacity(&profile(0.001, 0.1), &model);
        let fast = capacity(&profile(0.001, 2.0), &model);
        assert!(slow.is_finite() && fast.is_finite());
        assert!(
            slow > fast,
            "lower turnover ⇒ higher capacity: slow={slow} fast={fast}"
        );
        // Participation carries W through a β<1 power, so the gap is far steeper than linear in turnover.
        assert!(
            slow > fast * 100.0,
            "capacity gap should be ≫ linear: slow={slow} fast={fast}"
        );
    }

    #[test]
    fn capacity_scales_linearly_with_adv() {
        // QE-440: capacity is ∝ ADV (the new participation input) at fixed economics.
        let model = CapacityModel::with_defaults();
        let base = capacity(&profile(0.001, 0.5), &model);
        let doubled = capacity(
            &StrategyProfile {
                gross_edge: 0.001,
                turnover: 0.5,
                adv_notional: ADV * 2.0,
            },
            &model,
        );
        approx(doubled, base * 2.0);
    }

    #[test]
    fn uneconomic_zero_impact_and_missing_adv_guards() {
        // Spread-cross alone eats the edge (huge turnover, tiny edge) ⇒ capacity 0.
        let none = capacity(&profile(0.0001, 100.0), &CapacityModel::with_defaults());
        assert_eq!(none, 0.0);
        // No size impact ⇒ unbounded capacity.
        let unbounded = capacity(
            &profile(0.001, 1.0),
            &CapacityModel {
                impact_coeff: 0.0,
                ..CapacityModel::with_defaults()
            },
        );
        assert_eq!(unbounded, f64::INFINITY);
        // Missing ADV ⇒ no modellable size cap ⇒ unbounded.
        let no_adv = capacity(
            &StrategyProfile {
                gross_edge: 0.001,
                turnover: 1.0,
                adv_notional: 0.0,
            },
            &CapacityModel::with_defaults(),
        );
        assert_eq!(no_adv, f64::INFINITY);
    }

    #[test]
    fn high_turnover_weight_is_capped_at_capacity_at_target_aum() {
        let model = CapacityModel::with_defaults();
        // Strategy 0: high-turnover scalper. gross 0.001, turnover 2 → capacity $100k (see `ADV`).
        let scalper = profile(0.001, 2.0);
        // Strategy 1: low-turnover, huge capacity.
        let slow = profile(0.001, 0.1);
        let cap_scalper = capacity(&scalper, &model);
        let cap_slow = capacity(&slow, &model);
        approx(cap_scalper, 100_000.0);
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
        let p = profile(0.001, 0.5);
        let cap = capacity(&p, &model);
        let weights = [0.5, 0.5];
        // Target AUM far below the (shared) capacity ⇒ both caps ≥ 1 ⇒ no change.
        let target_aum = cap / 1000.0;
        let capped = cap_weights(&weights, &[cap, cap], target_aum);
        approx(capped[0], 0.5);
        approx(capped[1], 0.5);
    }

    // --- QE-443: inverse-vol (EWMA) seed weighting ---------------------------------------------------

    /// A deterministic alternating series with per-step amplitude `amp` (mean ≈ 0, vol ∝ amp).
    fn wobble(amp: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    #[test]
    fn ewma_variance_grows_with_amplitude_and_is_zero_for_flat() {
        approx(ewma_variance(&[0.01; 8], DEFAULT_EWMA_DECAY), 0.0); // flat ⇒ no dispersion
        approx(ewma_variance(&[], DEFAULT_EWMA_DECAY), 0.0);
        approx(ewma_variance(&[0.5], DEFAULT_EWMA_DECAY), 0.0); // single point ⇒ 0
        let lo = ewma_variance(&wobble(0.01, 32), DEFAULT_EWMA_DECAY);
        let hi = ewma_variance(&wobble(0.04, 32), DEFAULT_EWMA_DECAY);
        assert!(
            hi > lo && lo > 0.0,
            "higher amplitude ⇒ higher EWMA variance"
        );
    }

    #[test]
    fn inverse_vol_seed_gives_higher_vol_member_lower_weight() {
        // AC: a higher-vol member gets a strictly lower seed weight.
        let series = vec![wobble(0.01, 64), wobble(0.05, 64)];
        let w = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        approx(w.iter().sum::<f64>(), 1.0);
        assert!(
            w[0] > w[1],
            "low-vol member 0 must outweigh high-vol member 1: {w:?}"
        );
        // And by roughly the vol ratio (≈5×), a sanity check the weighting is inverse-vol not arbitrary.
        assert!(w[0] > 4.0 * w[1], "seed should be ~inverse to vol: {w:?}");
    }

    #[test]
    fn inverse_vol_seed_reduces_to_equal_for_equal_vol_members() {
        // AC: equal-vol members reduce to exactly 1/N.
        let series = vec![wobble(0.02, 48), wobble(0.02, 48), wobble(0.02, 48)];
        let w = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        for wi in &w {
            approx(*wi, 1.0 / 3.0);
        }
    }

    #[test]
    fn inverse_vol_seed_degenerate_members_fall_back_to_equal() {
        // A flat (zero-vol) member has no estimable variance ⇒ deterministic 1/N fallback (not NaN/inf).
        let series = vec![wobble(0.02, 32), vec![0.0; 32]];
        let w = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        approx(w[0], 0.5);
        approx(w[1], 0.5);
        // Too-short members (no dispersion) also fall back.
        let short = vec![vec![0.01], vec![0.02]];
        let w2 = inverse_vol_seed(&short, DEFAULT_EWMA_DECAY);
        approx(w2[0], 0.5);
        approx(w2[1], 0.5);
        assert!(inverse_vol_seed(&[], DEFAULT_EWMA_DECAY).is_empty());
    }

    #[test]
    fn inverse_vol_seed_is_deterministic() {
        let series = vec![wobble(0.013, 40), wobble(0.041, 40), wobble(0.007, 40)];
        let a = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        let b = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        assert_eq!(a, b, "same inputs must give byte-identical seed weights");
    }

    #[test]
    fn seed_weighting_equal_is_byte_identical_to_the_literal() {
        // AC: option OFF (the default) is byte-identical to the historical 1/k literal, regardless of series.
        assert_eq!(SeedWeighting::default(), SeedWeighting::Equal);
        let series = vec![wobble(0.01, 20), wobble(0.09, 20), wobble(0.03, 20)];
        let seeded = SeedWeighting::Equal.seed(&series, 3);
        assert_eq!(seeded, vec![1.0 / 3.0; 3]);
        assert!(SeedWeighting::Equal.seed(&series, 0).is_empty());
    }

    #[test]
    fn capacity_water_fill_layers_on_top_of_inverse_vol_seed() {
        // AC: the existing cap_weights water-fill still layers on top of an inverse-vol seed — a member
        // whose seeded share exceeds its capacity/AUM is capped there and the freed budget redistributed.
        let model = CapacityModel::with_defaults();
        let scalper = profile(0.001, 2.0); // capacity $100k (see `ADV`)
        let slow = profile(0.001, 0.1); // huge capacity
        let cap_scalper = capacity(&scalper, &model);
        let cap_slow = capacity(&slow, &model);
        let target_aum = 1_000_000.0;

        // Make the (capacity-bound) scalper the LOW-vol member so inverse-vol seeds it *above* its cap —
        // proving the water-fill still binds on top of the inverse-vol seed.
        let series = vec![wobble(0.005, 64), wobble(0.05, 64)];
        let seed = inverse_vol_seed(&series, DEFAULT_EWMA_DECAY);
        assert!(seed[0] > seed[1], "scalper seeded above 0.5 by inverse-vol");
        let capped = cap_weights(&seed, &[cap_scalper, cap_slow], target_aum);

        // Scalper still capped at capacity/AUM = 0.1, strictly below its (>0.5) inverse-vol seed …
        approx(capped[0], cap_scalper / target_aum);
        assert!(capped[0] < seed[0]);
        // … its dollar allocation equals its modelled capacity, and freed budget flows to the slow member.
        approx(capped[0] * target_aum, cap_scalper);
        assert!(capped[1] > seed[1]);
        approx(capped[0] + capped[1], 1.0);
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
