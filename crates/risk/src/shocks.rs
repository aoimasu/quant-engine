//! Frozen, content-addressed bar-level scenario-shock set (QE-441).
//!
//! The single-strategy sizing fitness (`qe_wfo::log_growth`, which sets `size_bps`) sees only the raw
//! historical net path, so it fits leverage to crypto's empirically thin crash sample. QE-441 injects
//! bounded synthetic **gap / funding-spike / ADL** shocks at the **price/bar level** in the backtester —
//! applied to the *held notional* so a larger size produces a larger drawdown — which makes the fitness
//! self-select a lower, **tail-aware** leverage (Dama §6.1 imaginary Black-Swan PnL before optimizing f;
//! §6.4 a heavy left tail pulls Kelly down).
//!
//! The shock **severity/frequency are un-deflated researcher degrees-of-freedom**, so they MUST be
//! **frozen / pre-registered and content-addressed**, not a knob tuned per run to flatter results
//! (panel dissent, Math#2). This type is that frozen set: it rides the vintage lineage like
//! [`SlippageCalibration`](crate::SlippageCalibration) (QE-431) and [`PortfolioSizer`](crate::PortfolioSizer)
//! (QE-433), carrying a [`content_hash`](ShockConfig::content_hash) over its canonical JSON. Its
//! [`seed`](ShockConfig::seed) is a **fixed pre-registered constant** — deliberately **not** derived from
//! the run seed — so the shock set cannot be re-drawn per run.
//!
//! The shock **shapes/magnitudes here deliberately mirror** `qe_ensemble::stress`'s synthetic set
//! (gap `0.10`, funding-spike `0.005 × 8`, ADL `0.05`). They are **replicated** rather than imported to
//! honour the search⟂portfolio firewall (`qe-wfo` must not depend on `qe-ensemble`); the per-bar RNG draw
//! + notional perturbation that *consumes* this config is `qe-wfo`-local (`qe_wfo::backtest`).
//!
//! All magnitudes are exact `Decimal` fractions of notional (never float money); the RNG rolls that key
//! `fires` / `adverse_fraction` are portable `u64`s from `qe_determinism`'s ChaCha8 stream.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Decimal places shock magnitudes are quantized to before hashing/serializing, matching
/// [`SIZER_SCALE`](crate::SIZER_SCALE) / [`SLIPPAGE_SCALE`](crate::SLIPPAGE_SCALE). Quantizing+normalizing
/// keeps every value serialize-idempotent so the content hash is byte-reproducible.
pub const SHOCK_SCALE: u32 = 12;

/// The frozen, pre-registered RNG seed for the bar-level shock schedule. A **fixed constant**, not the
/// run seed — the shock set is pre-registered, so it is identical across every run/vintage.
pub const DEFAULT_SHOCK_SEED: u64 = 0x5148_4535_4b45_4c4c; // "QHE5KELL"

/// Default shock frequency, in shocks per million bars (an integer, so it is exactly content-addressable
/// and portable). `30_000` ≈ 3 % of bars carry a shock — frequent enough to bite leverage, sparse enough
/// that a sanely-sized genome survives (a too-severe set would trip the `−inf` ruin absorber on *every*
/// genome uniformly, the failure mode the panel warned of).
pub const DEFAULT_SHOCK_FREQ_PER_MILLION: u32 = 30_000;

/// Default adverse price gap (fraction of notional) — mirrors `qe_ensemble::stress::DEFAULT_GAP_RETURN`.
pub const DEFAULT_GAP_RETURN: Decimal = Decimal::from_parts(10, 0, 0, false, 2); // 0.10
/// Default per-period funding drag during a funding spike — mirrors `DEFAULT_FUNDING_PER_PERIOD`.
pub const DEFAULT_FUNDING_PER_PERIOD: Decimal = Decimal::from_parts(5, 0, 0, false, 3); // 0.005
/// Default number of periods the funding spike persists — mirrors `DEFAULT_FUNDING_PERIODS`.
pub const DEFAULT_FUNDING_PERIODS: u32 = 8;
/// Default auto-deleveraging haircut (fraction of notional) — mirrors `DEFAULT_ADL_HAIRCUT`.
pub const DEFAULT_ADL_HAIRCUT: Decimal = Decimal::from_parts(5, 0, 0, false, 2); // 0.05

/// Quantize a magnitude to [`SHOCK_SCALE`] and normalize to its minimal scale, so it round-trips
/// byte-identically through serde (an excess-precision `Decimal` would otherwise change the content hash
/// on reload). Same discipline as [`sizer`](crate::sizer) / [`slippage`](crate::slippage).
#[must_use]
fn quantize(d: Decimal) -> Decimal {
    d.round_dp(SHOCK_SCALE).normalize()
}

/// The frozen, content-addressed bar-level shock set (QE-441): a seed + per-bar frequency + the three
/// synthetic-shock magnitudes (gap / funding-spike / ADL), all bounded and pre-registered.
///
/// It is consumed by `qe_wfo::backtest`: for each bar, one portable RNG roll decides whether a shock
/// [`fires`](ShockConfig::fires), a second picks the shape, and the resulting
/// [`adverse_fraction`](ShockConfig::adverse_fraction) is applied to the *held notional* as an exact
/// `Decimal` loss (so the drawdown scales with size). It is sealed into `VintageContent` (a hashed field),
/// pinning the exact shock set that shaped `size_bps` into the vintage's reproducible lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShockConfig {
    /// The portable ChaCha8 seed for the per-bar shock schedule. Frozen (see [`DEFAULT_SHOCK_SEED`]).
    pub seed: u64,
    /// Shock frequency in shocks per million bars (`fires` iff `roll % 1_000_000 < frequency_per_million`).
    /// `0` disables shocks; `≥ 1_000_000` shocks every bar.
    pub frequency_per_million: u32,
    /// Adverse price gap, as a fraction of notional (quantized+normalized).
    pub gap_adverse_return: Decimal,
    /// Per-period funding drag during a funding spike, as a fraction of notional (quantized+normalized).
    pub funding_per_period: Decimal,
    /// Number of periods the funding spike persists (its total drag is `funding_per_period × periods`).
    pub funding_periods: u32,
    /// Auto-deleveraging forced-close haircut, as a fraction of notional (quantized+normalized).
    pub adl_haircut: Decimal,
}

impl Default for ShockConfig {
    /// The frozen, pre-registered default shock set — the one the sizing fitness uses and the vintage
    /// seals. Magnitudes mirror `qe_ensemble::stress`'s synthetic set; the seed is the fixed
    /// [`DEFAULT_SHOCK_SEED`] (not the run seed).
    fn default() -> Self {
        ShockConfig {
            seed: DEFAULT_SHOCK_SEED,
            frequency_per_million: DEFAULT_SHOCK_FREQ_PER_MILLION,
            gap_adverse_return: quantize(DEFAULT_GAP_RETURN),
            funding_per_period: quantize(DEFAULT_FUNDING_PER_PERIOD),
            funding_periods: DEFAULT_FUNDING_PERIODS,
            adl_haircut: quantize(DEFAULT_ADL_HAIRCUT),
        }
    }
}

impl ShockConfig {
    /// Construct from raw magnitudes, quantizing every `Decimal` so the value is serialize-idempotent (a
    /// byte-reproducible content hash). Negative magnitudes are clamped to `0` (a shock is a *loss*; a
    /// negative "adverse" fraction is nonsensical).
    #[must_use]
    pub fn new(
        seed: u64,
        frequency_per_million: u32,
        gap_adverse_return: Decimal,
        funding_per_period: Decimal,
        funding_periods: u32,
        adl_haircut: Decimal,
    ) -> Self {
        ShockConfig {
            seed,
            frequency_per_million,
            gap_adverse_return: quantize(gap_adverse_return.max(Decimal::ZERO)),
            funding_per_period: quantize(funding_per_period.max(Decimal::ZERO)),
            funding_periods,
            adl_haircut: quantize(adl_haircut.max(Decimal::ZERO)),
        }
    }

    /// Whether a shock fires on a bar whose fire-roll is `roll`: `roll % 1_000_000 < frequency_per_million`.
    /// Pure integer arithmetic — portable and byte-reproducible.
    #[must_use]
    pub fn fires(&self, roll: u64) -> bool {
        (roll % 1_000_000) < u64::from(self.frequency_per_million)
    }

    /// The adverse fraction of notional for a firing shock whose shape-roll is `shape_roll`: `0` → gap,
    /// `1` → funding-spike total (`per_period × periods`), `2` → ADL haircut. Exact `Decimal` (no float
    /// money); the three shapes are the frozen, content-addressed magnitudes.
    #[must_use]
    pub fn adverse_fraction(&self, shape_roll: u64) -> Decimal {
        match shape_roll % 3 {
            0 => self.gap_adverse_return,
            1 => self.funding_per_period * Decimal::from(self.funding_periods),
            _ => self.adl_haircut,
        }
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (same pattern as
    /// [`PortfolioSizer::content_hash`](crate::PortfolioSizer::content_hash) /
    /// [`SlippageCalibration::content_hash`](crate::SlippageCalibration::content_hash)). Stable because
    /// every `Decimal` serializes as a canonical string and is quantized+normalized, and the integer
    /// fields serialize verbatim.
    ///
    /// # Panics
    /// Never in practice — this plain struct always serializes.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("ShockConfig always serializes");
        let digest = Sha256::digest(&bytes);
        let mut s = String::with_capacity(digest.len() * 2);
        use std::fmt::Write as _;
        for b in digest {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn default_magnitudes_mirror_the_stress_set() {
        let c = ShockConfig::default();
        assert_eq!(c.gap_adverse_return, d("0.1"));
        assert_eq!(c.funding_per_period, d("0.005"));
        assert_eq!(c.funding_periods, 8);
        assert_eq!(c.adl_haircut, d("0.05"));
        // The seed is the frozen pre-registered constant, NOT a run seed.
        assert_eq!(c.seed, DEFAULT_SHOCK_SEED);
    }

    #[test]
    fn fires_follows_frequency() {
        let c = ShockConfig::default(); // 30_000 / 1_000_000
        assert!(c.fires(0)); // 0 % 1e6 = 0 < 30_000
        assert!(c.fires(29_999));
        assert!(!c.fires(30_000));
        assert!(!c.fires(999_999));
        // A zero-frequency set never fires; a saturated one always does.
        let never = ShockConfig::new(1, 0, d("0.1"), d("0.005"), 8, d("0.05"));
        assert!(!never.fires(0));
        let always = ShockConfig::new(1, 1_000_000, d("0.1"), d("0.005"), 8, d("0.05"));
        assert!(always.fires(999_999));
    }

    #[test]
    fn adverse_fraction_selects_the_three_shapes() {
        let c = ShockConfig::default();
        assert_eq!(c.adverse_fraction(0), d("0.1")); // gap
        assert_eq!(c.adverse_fraction(1), d("0.04")); // funding: 0.005 × 8
        assert_eq!(c.adverse_fraction(2), d("0.05")); // adl
                                                      // The roll is taken mod 3, so it wraps.
        assert_eq!(c.adverse_fraction(3), c.adverse_fraction(0));
    }

    #[test]
    fn content_hash_is_stable_and_field_sensitive() {
        // Content-addressed: identical config ⇒ identical hash; any field change ⇒ a different hash.
        let base = ShockConfig::default();
        assert_eq!(base.content_hash(), base.clone().content_hash());
        assert_eq!(base.content_hash().len(), 64);
        assert!(base.content_hash().chars().all(|ch| ch.is_ascii_hexdigit()));

        let diff_seed = ShockConfig {
            seed: base.seed + 1,
            ..base.clone()
        };
        let diff_freq = ShockConfig {
            frequency_per_million: base.frequency_per_million + 1,
            ..base.clone()
        };
        let diff_gap = ShockConfig::new(
            base.seed,
            base.frequency_per_million,
            d("0.2"),
            d("0.005"),
            8,
            d("0.05"),
        );
        assert_ne!(base.content_hash(), diff_seed.content_hash());
        assert_ne!(base.content_hash(), diff_freq.content_hash());
        assert_ne!(base.content_hash(), diff_gap.content_hash());
    }

    #[test]
    fn new_quantizes_and_clamps_negatives() {
        // Excess precision is quantized to a round-trip-stable value.
        let c = ShockConfig::new(1, 10, d("0.1000000000009999"), d("0.005"), 8, d("0.05"));
        assert!(c.gap_adverse_return.scale() <= SHOCK_SCALE);
        // Serialize→deserialize→serialize is byte-idempotent (content hash reproducible on reload).
        let json = serde_json::to_string(&c).unwrap();
        let round: ShockConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c.content_hash(), round.content_hash());
        // A negative "adverse" magnitude is clamped to 0.
        let clamped = ShockConfig::new(1, 10, d("-0.1"), d("0.005"), 8, d("0.05"));
        assert_eq!(clamped.gap_adverse_return, Decimal::ZERO);
    }
}
