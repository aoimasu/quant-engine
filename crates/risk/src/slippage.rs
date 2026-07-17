//! Content-addressed slippage/impact calibration (QE-431) — the **single source of truth** the
//! selection-critical cost model reads on both sides of the search⟂portfolio firewall.
//!
//! `friction.rs` (`qe-wfo`) and `capacity.rs` (`qe-ensemble`) used to author the slippage `half_spread`
//! and the size-impact coefficient as **hardcoded literals in two crates and two unit systems** that
//! could silently drift. This module hoists them into one [`SlippageCalibration`] that both sides
//! **derive** from (each keeps its own unit conversion — the sanctioned duplicated-CONFIG pattern, never
//! a `qe-wfo → qe-ensemble` code edge), so a coefficient-parity test can prove they never diverge.
//!
//! The calibration is **content-addressed** ([`SlippageCalibration::content_hash`], the `Lineage::id`
//! pattern) and rides the vintage lineage alongside [`CalibrationProfile`](crate::CalibrationProfile).
//! [`fit_slippage_calibration`] is maxdama §7.7's estimator: bin trades by size, fit `impact = f(volume)`,
//! and read `half_spread` off the observed spread distribution. The perp trade feed **carries aggressor
//! side**, so the Lee-Ready classifier is skipped — the aggressor sign is taken directly.
//!
//! All arithmetic is exact `Decimal` and every stored coefficient is quantized+normalized, so a fit on a
//! pinned input snapshot reproduces **byte-identical** coefficients and a byte-stable content hash.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use qe_domain::Side;

/// Decimal places calibrated coefficients are quantized to before hashing/serializing. Wide enough for a
/// per-$ impact coefficient (`~2e-9`); quantizing+normalizing keeps the value serialize-idempotent so the
/// content hash is byte-reproducible (the hazard [`quantize_calibration`](crate::quantize_calibration)
/// guards for breaker thresholds, QE-416).
pub const SLIPPAGE_SCALE: u32 = 18;

/// The canonical default half-spread (fraction of price) — a 1bp spread-cross, mirroring QE-109/QE-128.
pub const DEFAULT_HALF_SPREAD: Decimal = Decimal::from_parts(1, 0, 0, false, 4); // 0.0001
/// The canonical default size-impact per $ of traded notional (QE-128's `2e-9`).
pub const DEFAULT_IMPACT_PER_NOTIONAL: Decimal = Decimal::from_parts(2, 0, 0, false, 9); // 0.000000002
/// The canonical default reference mark ($/contract) that pins friction's per-contract coefficient. At
/// this mark `impact_per_notional · reference_mark = 2e-9 · 50000 = 1e-4`, exactly QE-109's per-contract
/// `impact` default — the two legacy literals were only mutually consistent at this mark.
pub const DEFAULT_REFERENCE_MARK: Decimal = Decimal::from_parts(50_000, 0, 0, false, 0);

/// Quantize a coefficient to [`SLIPPAGE_SCALE`] and normalize to its minimal scale, so it round-trips
/// byte-identically through serde (excess-precision division results would otherwise change the content
/// hash on reload).
#[must_use]
fn quantize(d: Decimal) -> Decimal {
    d.round_dp(SLIPPAGE_SCALE).normalize()
}

/// The one content-addressed slippage/impact calibration (QE-431) shared by friction & capacity.
///
/// `impact_per_notional` is the **canonical** size-impact unit — per $ of traded notional (capacity's
/// unit, asset-portable). `reference_mark` is the mark price that converts it to friction's per-contract
/// coefficient. `half_spread` (a fraction of price) is identical on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlippageCalibration {
    /// Half the bid/ask spread, as a fraction of price (the spread-cross term).
    pub half_spread: Decimal,
    /// Size-impact coefficient per $ of traded notional (the canonical, asset-portable unit).
    pub impact_per_notional: Decimal,
    /// Reference mark ($/contract) pinning friction's per-contract coefficient
    /// (`impact_per_notional · reference_mark`).
    pub reference_mark: Decimal,
}

impl Default for SlippageCalibration {
    fn default() -> Self {
        // The canonical pre-wiring seed: reproduces QE-109's friction `impact = 1e-4`/contract and
        // QE-128's capacity `impact_coeff = 2e-9`/$ from one source. Live-fitted wiring is follow-up.
        SlippageCalibration {
            half_spread: DEFAULT_HALF_SPREAD,
            impact_per_notional: DEFAULT_IMPACT_PER_NOTIONAL,
            reference_mark: DEFAULT_REFERENCE_MARK,
        }
    }
}

impl SlippageCalibration {
    /// Construct from raw coefficients, quantizing each so the value is serialize-idempotent.
    #[must_use]
    pub fn new(
        half_spread: Decimal,
        impact_per_notional: Decimal,
        reference_mark: Decimal,
    ) -> Self {
        SlippageCalibration {
            half_spread: quantize(half_spread),
            impact_per_notional: quantize(impact_per_notional),
            reference_mark: quantize(reference_mark),
        }
    }

    /// Friction's **per-contract** size-impact coefficient: `impact_per_notional · reference_mark`.
    #[must_use]
    pub fn friction_impact_per_contract(&self) -> Decimal {
        self.impact_per_notional * self.reference_mark
    }

    /// The canonical per-fill slippage cost for a trade of `notional` ($), in the per-notional form both
    /// sides reduce to: `notional · (half_spread + impact_per_notional · notional)`.
    #[must_use]
    pub fn notional_cost(&self, notional: Decimal) -> Decimal {
        notional * (self.half_spread + self.impact_per_notional * notional)
    }

    /// `half_spread` as `f64` (capacity is an f64 model). Panics only on a non-representable `Decimal`,
    /// which the quantized coefficients never are.
    #[must_use]
    pub fn half_spread_f64(&self) -> f64 {
        self.half_spread
            .to_f64()
            .expect("quantized half_spread is representable as f64")
    }

    /// `impact_per_notional` as `f64` (capacity's `impact_coeff`).
    #[must_use]
    pub fn impact_per_notional_f64(&self) -> f64 {
        self.impact_per_notional
            .to_f64()
            .expect("quantized impact_per_notional is representable as f64")
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (same pattern as
    /// [`Lineage::id`](qe_determinism::Lineage::id) / `Vintage::content_hash`). Stable because the
    /// `Decimal` fields serialize as canonical strings (`serde-with-str`) and are quantized+normalized.
    ///
    /// # Panics
    /// Never in practice — a struct of three `Decimal`s always serializes.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("SlippageCalibration always serializes");
        let digest = Sha256::digest(&bytes);
        let mut s = String::with_capacity(digest.len() * 2);
        use std::fmt::Write as _;
        for b in digest {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// A venue trade stamped with its **aggressor side** (the perp feed carries it — no Lee-Ready needed),
/// its size and fill price, and the pre-trade mid used to measure the realized impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizedTrade {
    /// Aggressor side (Buy lifts the offer, Sell hits the bid).
    pub side: Side,
    /// Filled quantity (> 0, in contracts).
    pub qty: Decimal,
    /// Fill price.
    pub price: Decimal,
    /// Mid price immediately before the trade (the impact baseline).
    pub pre_mid: Decimal,
}

/// A top-of-book quote sample, used to read the observed `half_spread` distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuoteSample {
    /// Best bid.
    pub bid: Decimal,
    /// Best ask.
    pub ask: Decimal,
}

/// The default bin count for [`fit_slippage_calibration`] (maxdama §7.7 "bin trades by size").
pub const DEFAULT_IMPACT_BINS: usize = 10;

/// The quantile-median element of a sorted slice (round `0.5·(n−1)`), mirroring
/// [`calibrate_threshold`](crate::calibrate_threshold) — a single element, no averaging, so the result is
/// exactly reproducible.
fn median(sorted: &[Decimal]) -> Option<Decimal> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((0.5 * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    Some(sorted[idx])
}

/// Fit a [`SlippageCalibration`] from the venue's own trade + quote history (maxdama §7.7).
///
/// - `half_spread` = median of `(ask − bid) / (2·mid)` over `quotes` (`mid = (ask+bid)/2`, `mid > 0`).
/// - `impact_per_notional` = **binned zero-intercept least-squares** slope of *signed* fractional impact
///   vs notional: `signed_impact = dir·(price − pre_mid)/pre_mid`, `notional = qty·price`, `dir = +1`
///   (Buy) / `−1` (Sell) — the aggressor sign taken directly (no Lee-Ready). Trades are sorted by
///   notional, split into `bins` equal-count buckets, and the slope through the per-bucket means is
///   `Σ(x̄·ȳ) / Σ(x̄²)`.
/// - `reference_mark` = median trade price.
///
/// Degenerate inputs (no usable quotes/trades, a zero slope denominator) fall back to the corresponding
/// [`SlippageCalibration::default`] coefficient. All arithmetic is exact `Decimal`, so the fit is
/// byte-reproducible on a pinned input snapshot.
#[must_use]
pub fn fit_slippage_calibration(
    trades: &[SizedTrade],
    quotes: &[QuoteSample],
    bins: usize,
) -> SlippageCalibration {
    let default = SlippageCalibration::default();

    // --- half_spread from the observed spread distribution ---
    let two = Decimal::from(2);
    let mut half_spreads: Vec<Decimal> = quotes
        .iter()
        .filter_map(|q| {
            let mid = (q.bid + q.ask) / two;
            if mid > Decimal::ZERO {
                Some((q.ask - q.bid) / (two * mid))
            } else {
                None
            }
        })
        .collect();
    half_spreads.sort();
    let half_spread = median(&half_spreads).unwrap_or(default.half_spread);

    // --- reference_mark = median trade price ---
    let mut prices: Vec<Decimal> = trades
        .iter()
        .filter(|t| t.price > Decimal::ZERO)
        .map(|t| t.price)
        .collect();
    prices.sort();
    let reference_mark = median(&prices).unwrap_or(default.reference_mark);

    // --- impact_per_notional = binned zero-intercept LS slope of signed impact vs notional ---
    // (notional, signed_impact) pairs, sorted by notional so equal-count bins are size-ordered.
    let mut points: Vec<(Decimal, Decimal)> = trades
        .iter()
        .filter(|t| t.qty > Decimal::ZERO && t.pre_mid > Decimal::ZERO && t.price > Decimal::ZERO)
        .map(|t| {
            let notional = t.qty * t.price;
            let dir = match t.side {
                Side::Buy => Decimal::ONE,
                Side::Sell => Decimal::NEGATIVE_ONE,
            };
            let signed_impact = dir * (t.price - t.pre_mid) / t.pre_mid;
            (notional, signed_impact)
        })
        .collect();
    points.sort_by_key(|p| p.0);

    let impact_per_notional = binned_slope(&points, bins).unwrap_or(default.impact_per_notional);

    SlippageCalibration::new(half_spread, impact_per_notional, reference_mark)
}

/// Zero-intercept least-squares slope through the means of `bins` equal-count buckets of size-sorted
/// `points` (`(x = notional, y = signed_impact)`): `Σ(x̄·ȳ) / Σ(x̄²)`. `None` if there are no points or the
/// denominator is zero.
fn binned_slope(points: &[(Decimal, Decimal)], bins: usize) -> Option<Decimal> {
    let n = points.len();
    if n == 0 {
        return None;
    }
    let k = bins.clamp(1, n);
    let mut numer = Decimal::ZERO;
    let mut denom = Decimal::ZERO;
    for j in 0..k {
        let lo = j * n / k;
        let hi = (j + 1) * n / k;
        let bucket = &points[lo..hi];
        if bucket.is_empty() {
            continue;
        }
        let count = Decimal::from(bucket.len());
        let x_sum: Decimal = bucket.iter().map(|p| p.0).sum();
        let y_sum: Decimal = bucket.iter().map(|p| p.1).sum();
        let x_mean = x_sum / count;
        let y_mean = y_sum / count;
        numer += x_mean * y_mean;
        denom += x_mean * x_mean;
    }
    if denom.is_zero() {
        return None;
    }
    Some(numer / denom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn default_reconstructs_the_two_legacy_literals() {
        let cal = SlippageCalibration::default();
        // capacity's per-$ default …
        assert_eq!(cal.impact_per_notional, d("0.000000002")); // 2e-9
        assert_eq!(cal.half_spread, d("0.0001")); // 1bp
                                                  // … and friction's per-contract default is the derived product (2e-9 · 50000 = 1e-4).
        assert_eq!(cal.friction_impact_per_contract(), d("0.0001"));
    }

    #[test]
    fn content_hash_is_stable_and_serialize_idempotent() {
        let cal = SlippageCalibration::default();
        let h1 = cal.content_hash();
        // 64 lowercase-hex chars, and re-computing is identical (byte-reproducible).
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h1, SlippageCalibration::default().content_hash());

        // serialize → parse → serialize is byte-stable (the content-hash invariant).
        let s1 = serde_json::to_string(&cal).unwrap();
        let back: SlippageCalibration = serde_json::from_str(&s1).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), s1);
        assert_eq!(back.content_hash(), h1);
    }

    #[test]
    fn content_hash_is_sensitive_to_every_field() {
        let base = SlippageCalibration::default().content_hash();
        assert_ne!(
            base,
            SlippageCalibration::new(
                d("0.0002"),
                DEFAULT_IMPACT_PER_NOTIONAL,
                DEFAULT_REFERENCE_MARK
            )
            .content_hash()
        );
        assert_ne!(
            base,
            SlippageCalibration::new(
                DEFAULT_HALF_SPREAD,
                d("0.000000003"),
                DEFAULT_REFERENCE_MARK
            )
            .content_hash()
        );
        assert_ne!(
            base,
            SlippageCalibration::new(DEFAULT_HALF_SPREAD, DEFAULT_IMPACT_PER_NOTIONAL, d("40000"))
                .content_hash()
        );
    }

    fn trade(side: Side, qty: &str, price: &str, pre_mid: &str) -> SizedTrade {
        SizedTrade {
            side,
            qty: d(qty),
            price: d(price),
            pre_mid: d(pre_mid),
        }
    }

    #[test]
    fn fit_is_byte_reproducible_and_reads_the_venue_signal() {
        // Quotes: a 2bp full spread at mid 100 ⇒ half_spread 1bp = 0.0001.
        let quotes = vec![
            QuoteSample {
                bid: d("99.99"),
                ask: d("100.01"),
            },
            QuoteSample {
                bid: d("99.99"),
                ask: d("100.01"),
            },
            QuoteSample {
                bid: d("199.98"),
                ask: d("200.02"),
            }, // same 2bp at mid 200
        ];
        // Trades: aggressor side carried; larger trades move price further (positive impact slope).
        let trades = vec![
            trade(Side::Buy, "1", "100.001", "100"),
            trade(Side::Buy, "10", "100.05", "100"),
            trade(Side::Sell, "1", "99.999", "100"),
            trade(Side::Sell, "10", "99.95", "100"),
        ];

        let a = fit_slippage_calibration(&trades, &quotes, DEFAULT_IMPACT_BINS);
        let b = fit_slippage_calibration(&trades, &quotes, DEFAULT_IMPACT_BINS);
        // Byte-reproducible on the same pinned input.
        assert_eq!(a, b);
        assert_eq!(a.content_hash(), b.content_hash());

        // half_spread read off the observed distribution (median 1bp).
        assert_eq!(a.half_spread, d("0.0001"));
        // reference_mark = median trade price (sorted prices → median element 100.001).
        assert!(a.reference_mark > d("99") && a.reference_mark < d("201"));
        // A positive, non-degenerate impact slope was fit from the size/impact relationship.
        assert!(a.impact_per_notional > Decimal::ZERO);
    }

    #[test]
    fn fit_falls_back_to_defaults_on_empty_input() {
        let cal = fit_slippage_calibration(&[], &[], DEFAULT_IMPACT_BINS);
        assert_eq!(cal, SlippageCalibration::default());
    }

    #[test]
    fn skips_lee_ready_uses_carried_aggressor_sign() {
        // Two trades with the SAME price move magnitude but opposite carried aggressor sides both yield a
        // POSITIVE (adverse) impact — proving the sign is taken from `side`, not inferred by Lee-Ready.
        let buy = trade(Side::Buy, "5", "100.02", "100");
        let sell = trade(Side::Sell, "5", "99.98", "100");
        let up = fit_slippage_calibration(&[buy], &[], 1);
        let down = fit_slippage_calibration(&[sell], &[], 1);
        assert!(up.impact_per_notional > Decimal::ZERO);
        assert!(down.impact_per_notional > Decimal::ZERO);
    }
}
