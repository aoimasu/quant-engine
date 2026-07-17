//! Portfolio-level advisory Kelly sizer (QE-433) — the content-addressed sidecar that rides the vintage
//! and carries the fractional (≤½) empirical-Kelly **leverage multiplier** the live netter applies.
//!
//! After the ensemble mask + capacity weights are fixed, the seal path solves the growth-optimal leverage
//! `f*` on the realised **combined net-of-cost** series (`qe_wfo::fractional_kelly`, reusing `log_growth`)
//! and applies a fractional multiplier `κ ∈ [0.3, 0.5]`. The resulting factor `κ·f*` is stored here,
//! per-vintage, alongside [`CalibrationProfile`](crate::CalibrationProfile) and
//! [`SlippageCalibration`](crate::SlippageCalibration).
//!
//! It is **advisory**: the live netter scales the netted book by [`PortfolioSizer::multiplier`] and clamps
//! the result **below** the pretrade leverage cap (QE-215), which remains the hard backstop. Because it is
//! solved on the realised joint path it estimates **no covariance** — positively-correlated members
//! inflate the combined variance directly, so the multiplier down-weights them by construction.
//!
//! The coefficient is quantized+normalized (like `SlippageCalibration`) so a fit on a pinned input
//! reproduces a **byte-identical** multiplier and content hash.

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Decimal places the multiplier is quantized to before hashing/serializing — sub-basis-point resolution,
/// matching [`CALIBRATION_SCALE`](crate::CALIBRATION_SCALE). Quantizing+normalizing keeps the value
/// serialize-idempotent so the content hash is byte-reproducible.
pub const SIZER_SCALE: u32 = 12;

/// Quantize a coefficient to [`SIZER_SCALE`] and normalize to its minimal scale, so it round-trips
/// byte-identically through serde (an excess-precision `f64`→`Decimal` conversion would otherwise change
/// the content hash on reload).
#[must_use]
fn quantize(d: Decimal) -> Decimal {
    d.round_dp(SIZER_SCALE).normalize()
}

/// The per-vintage advisory portfolio-Kelly sizer (QE-433): a single fractional-Kelly **leverage
/// multiplier** applied to the netted book.
///
/// `multiplier` (`≥ 0`) is `κ·f*` — the fractional (≤½) empirical Kelly solved on the realised combined
/// net-of-cost series. `1.0` is the **neutral** deploy-the-naive-size value (the pre-QE-433 behaviour);
/// `< 1` cuts leverage (the typical fat-left-tail / positive-correlation outcome); `> 1` raises it but is
/// clamped below the pretrade cap by the consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSizer {
    /// The fractional-Kelly leverage multiplier `κ·f*` (`≥ 0`), quantized+normalized.
    pub multiplier: Decimal,
}

impl Default for PortfolioSizer {
    /// The neutral sizer: `multiplier = 1.0`, i.e. deploy the naive summed size unchanged. This is the
    /// value a vintage sealed **without** a Kelly pass carries, preserving pre-QE-433 sizing.
    fn default() -> Self {
        PortfolioSizer {
            multiplier: Decimal::ONE,
        }
    }
}

impl PortfolioSizer {
    /// Construct from a raw multiplier, clamped to `≥ 0` (a negative leverage is nonsensical — flip the
    /// direction, never the leverage) and quantized so the value is serialize-idempotent.
    #[must_use]
    pub fn new(multiplier: Decimal) -> Self {
        PortfolioSizer {
            multiplier: quantize(multiplier.max(Decimal::ZERO)),
        }
    }

    /// Construct from the fractional-Kelly `f64` the solver returns (`qe_wfo::fractional_kelly`). A
    /// non-finite input maps to `0` (fail-safe: never size **up** on a broken solve); otherwise it is
    /// clamped to `≥ 0` and quantized.
    #[must_use]
    pub fn from_kelly(fractional_kelly: f64) -> Self {
        if !fractional_kelly.is_finite() {
            return PortfolioSizer {
                multiplier: Decimal::ZERO,
            };
        }
        let raw = Decimal::from_f64(fractional_kelly.max(0.0)).unwrap_or(Decimal::ZERO);
        PortfolioSizer::new(raw)
    }

    /// The advisory leverage multiplier `κ·f*` (`≥ 0`) the live netter scales the netted book by.
    #[must_use]
    pub fn multiplier(&self) -> Decimal {
        self.multiplier
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (same pattern as
    /// [`SlippageCalibration::content_hash`](crate::SlippageCalibration::content_hash)). Stable because the
    /// `Decimal` field serializes as a canonical string (`serde-with-str`) and is quantized+normalized.
    ///
    /// # Panics
    /// Never in practice — a struct of one `Decimal` always serializes.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("PortfolioSizer always serializes");
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
    fn default_is_neutral_unit_multiplier() {
        // A vintage without a Kelly pass deploys the naive summed size unchanged.
        assert_eq!(PortfolioSizer::default().multiplier(), Decimal::ONE);
    }

    #[test]
    fn new_clamps_negative_and_quantizes() {
        // Negative leverage is nonsensical → clamped to 0.
        assert_eq!(PortfolioSizer::new(d("-0.5")).multiplier(), Decimal::ZERO);
        // A sane fractional multiplier passes through.
        assert_eq!(PortfolioSizer::new(d("0.35")).multiplier(), d("0.35"));
    }

    #[test]
    fn from_kelly_quantizes_and_fails_safe() {
        // A finite fractional Kelly is stored, clamped ≥ 0 and quantized to a round-trip-stable value.
        let s = PortfolioSizer::from_kelly(0.4237);
        assert!(s.multiplier() > Decimal::ZERO && s.multiplier() < Decimal::ONE);
        assert!(s.multiplier().scale() <= SIZER_SCALE);
        // Non-finite / negative → 0 (never size up on a broken solve).
        assert_eq!(
            PortfolioSizer::from_kelly(f64::NAN).multiplier(),
            Decimal::ZERO
        );
        assert_eq!(
            PortfolioSizer::from_kelly(f64::INFINITY).multiplier(),
            Decimal::ZERO
        );
        assert_eq!(PortfolioSizer::from_kelly(-1.0).multiplier(), Decimal::ZERO);
    }

    #[test]
    fn content_hash_is_stable_serialize_idempotent_and_field_sensitive() {
        let s = PortfolioSizer::new(d("0.42"));
        let h1 = s.content_hash();
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        // Re-computing is identical (byte-reproducible).
        assert_eq!(h1, PortfolioSizer::new(d("0.42")).content_hash());
        // serialize → parse → serialize is byte-stable.
        let j = serde_json::to_string(&s).unwrap();
        let back: PortfolioSizer = serde_json::from_str(&j).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), j);
        assert_eq!(back.content_hash(), h1);
        // A different multiplier changes the hash.
        assert_ne!(h1, PortfolioSizer::new(d("0.43")).content_hash());
        assert_ne!(h1, PortfolioSizer::default().content_hash());
    }
}
