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
use rust_decimal::{Decimal, MathematicalOps};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use qe_domain::Side;

/// Decimal places calibrated coefficients are quantized to before hashing/serializing. Wide enough for a
/// dimensionless participation coefficient and a fractional exponent; quantizing+normalizing keeps the
/// value serialize-idempotent so the content hash is byte-reproducible (the hazard
/// [`quantize_calibration`](crate::quantize_calibration) guards for breaker thresholds, QE-416).
pub const SLIPPAGE_SCALE: u32 = 18;

/// The canonical default half-spread (fraction of price) — a 1bp spread-cross, mirroring QE-109/QE-128.
pub const DEFAULT_HALF_SPREAD: Decimal = Decimal::from_parts(1, 0, 0, false, 4); // 0.0001
/// The canonical default **participation** impact coefficient (QE-440): the impact fraction of notional
/// when trading `u = 1` (100 % of a rolling ADV). Dimensionless and asset-portable (no per-contract vs
/// per-$ split), shared verbatim by friction and capacity. Default `0.01` is an economically-grounded
/// √-law seed (~1 % impact at 100 % of one hour's ADV, maxdama §7.7); live power-law fitting is follow-up.
pub const DEFAULT_IMPACT_COEFF: Decimal = Decimal::from_parts(1, 0, 0, false, 2); // 0.01
/// The canonical default impact **exponent** β (QE-440): the concavity of impact in participation,
/// `β ∈ [0.2, 0.5]`. Default `0.5` is the square-root law (maxdama §7.7); `β < 1` makes impact concave.
pub const DEFAULT_IMPACT_EXPONENT: Decimal = Decimal::from_parts(5, 0, 0, false, 1); // 0.5
/// The canonical default **decision-to-fill alpha-loss** (implementation-shortfall) coefficient (QE-444,
/// maxdama §7.3): the adverse close→open directional drift charged **in the trade direction** as a fraction
/// of notional. Default **`0`** — the term is present, wired, and tested but **inert**: the realised drift
/// can only be *measured* from live/shadow data (QE-435), which does not exist yet, so we refuse to invent a
/// directional cost. Zero keeps the calibration serialization (and every golden/`content_hash`) byte-stable
/// via the field's `skip_serializing_if`. Live-fitted wiring is follow-up (see [`AlphaLossAccumulator`]).
pub const DEFAULT_ALPHA_LOSS: Decimal = Decimal::ZERO; // 0 — measurement-deferred, golden-safe

/// Quantize a coefficient to [`SLIPPAGE_SCALE`] and normalize to its minimal scale, so it round-trips
/// byte-identically through serde (excess-precision division results would otherwise change the content
/// hash on reload).
#[must_use]
fn quantize(d: Decimal) -> Decimal {
    d.round_dp(SLIPPAGE_SCALE).normalize()
}

/// The one content-addressed slippage/impact calibration (QE-431 + QE-440) shared by friction & capacity.
///
/// The size-impact is **concave in participation** (maxdama §7.7): the impact fraction of notional is
/// `impact_coeff · u^β`, where `u = traded / ADV` is the dimensionless participation (order size as a
/// fraction of a rolling ADV). `impact_coeff` (the fraction at `u = 1`) and `impact_exponent` (β) are the
/// **same, asset-portable, participation-keyed** coefficients on both sides of the search⟂portfolio
/// firewall — no per-contract vs per-$ conversion (QE-440 resolves the QE-431 reviewer's unit flag).
/// `half_spread` (a fraction of price) is identical on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlippageCalibration {
    /// Half the bid/ask spread, as a fraction of price (the spread-cross term).
    pub half_spread: Decimal,
    /// Participation impact coefficient — the impact fraction of notional at `u = 1` (100 % of ADV).
    /// Dimensionless, asset-portable, shared verbatim by friction & capacity.
    pub impact_coeff: Decimal,
    /// Impact exponent β — the concavity of impact in participation (`u^β`, `β ∈ [0.2, 0.5]`, `< 1`).
    pub impact_exponent: Decimal,
    /// Decision-to-fill **alpha-loss** (implementation-shortfall) coefficient (QE-444, maxdama §7.3): the
    /// adverse close→open directional drift, as a fraction of notional, charged **in the trade direction**
    /// (a buy pays the up-drift, a sell pays the down-drift — an *odd* function of side, unlike the
    /// side-blind `half_spread`; see [`SlippageCalibration::directional_drift`]). It is a **separate**,
    /// friction-only term — deliberately **not** part of [`SlippageCalibration::cost_fraction`] /
    /// [`SlippageCalibration::notional_cost`], which `capacity.rs` also consumes (capacity models sizing
    /// headroom, not per-fill decision-to-fill drift, so folding it in would break the QE-431
    /// friction↔capacity parity).
    ///
    /// Default **`0`** ([`DEFAULT_ALPHA_LOSS`]) — measurement-deferred (QE-435: realised drift needs
    /// live/shadow data that does not exist yet). `#[serde(default, skip_serializing_if)]` omits the field
    /// at `0`, so a default calibration serialises byte-identically to the pre-QE-444 record and its
    /// `content_hash` (hence every downstream golden/vintage) is **unmoved**; only a fitted non-zero value
    /// makes the key — and the hash — move.
    #[serde(default, skip_serializing_if = "Decimal::is_zero")]
    pub alpha_loss: Decimal,
}

impl Default for SlippageCalibration {
    fn default() -> Self {
        // The pre-fit seed: a √-in-participation impact (β = 0.5) with a ~1 % coefficient at full-ADV
        // participation, shared by friction & capacity from one source. Live-fitted wiring is follow-up.
        SlippageCalibration {
            half_spread: DEFAULT_HALF_SPREAD,
            impact_coeff: DEFAULT_IMPACT_COEFF,
            impact_exponent: DEFAULT_IMPACT_EXPONENT,
            alpha_loss: DEFAULT_ALPHA_LOSS,
        }
    }
}

impl SlippageCalibration {
    /// Construct from raw coefficients, quantizing each so the value is serialize-idempotent. The
    /// decision-to-fill `alpha_loss` defaults to [`DEFAULT_ALPHA_LOSS`] (`0`, measurement-deferred); set it
    /// with [`SlippageCalibration::with_alpha_loss`] once fitted from live/shadow data.
    #[must_use]
    pub fn new(half_spread: Decimal, impact_coeff: Decimal, impact_exponent: Decimal) -> Self {
        SlippageCalibration {
            half_spread: quantize(half_spread),
            impact_coeff: quantize(impact_coeff),
            impact_exponent: quantize(impact_exponent),
            alpha_loss: DEFAULT_ALPHA_LOSS,
        }
    }

    /// A copy with the decision-to-fill `alpha_loss` (implementation-shortfall) coefficient replaced,
    /// quantized so it stays serialize-idempotent (QE-444). This is the sink the live/shadow measurement
    /// (`AlphaLossAccumulator::mean`) feeds once realised close→open drift can be measured (QE-435).
    #[must_use]
    pub fn with_alpha_loss(mut self, alpha_loss: Decimal) -> Self {
        self.alpha_loss = quantize(alpha_loss);
        self
    }

    /// The **signed** per-notional decision-to-fill drift for a trade of `side` (QE-444, maxdama §7.3):
    /// `+alpha_loss` for a **buy** (the fill drifted **up** from the decision price) and `−alpha_loss` for a
    /// **sell** (the fill drifted **down**). An **odd** function of side — the directional signature that
    /// distinguishes alpha-loss from the side-blind (even) `half_spread`. Exact `Decimal`.
    #[must_use]
    pub fn directional_drift(&self, side: Side) -> Decimal {
        match side {
            Side::Buy => self.alpha_loss,
            Side::Sell => -self.alpha_loss,
        }
    }

    /// The **adverse** decision-to-fill alpha-loss cost on a fill of `notional_abs` (QE-444): the drift is
    /// signal-aligned (adverse to *whichever* way the trade points), so the cost magnitude is
    /// `notional_abs · alpha_loss` in the trade's own direction — it always **reduces** net return. The
    /// direction (sign) is carried by [`directional_drift`](Self::directional_drift); this is its magnitude.
    #[must_use]
    pub fn alpha_loss_cost(&self, notional_abs: Decimal) -> Decimal {
        notional_abs * self.alpha_loss
    }

    /// The impact fraction of notional at participation `u` (QE-440): `impact_coeff · u^β`, concave in
    /// `u` (`β < 1`). A non-positive `u` (no size, or missing ADV) yields `0` (spread-cross only).
    ///
    /// Deterministic across platforms: `u^β` is `rust_decimal`'s [`MathematicalOps::powd`], evaluated in
    /// pure Decimal arithmetic (no hardware `f64`), so it is byte-identical on arm64 (dev) and x86_64
    /// (CI) — safe for the sealed/hashed money ledger.
    #[must_use]
    pub fn participation_impact(&self, participation: Decimal) -> Decimal {
        if participation <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.impact_coeff * participation.powd(self.impact_exponent)
    }

    /// The full cost fraction of notional at participation `u`: `half_spread + impact_coeff · u^β`.
    #[must_use]
    pub fn cost_fraction(&self, participation: Decimal) -> Decimal {
        self.half_spread + self.participation_impact(participation)
    }

    /// The canonical per-fill slippage cost for a trade of `notional` ($) against `adv_notional` ($ of
    /// rolling ADV), in the participation form both sides reduce to:
    /// `notional · (half_spread + impact_coeff · (notional/adv_notional)^β)`. A non-positive
    /// `adv_notional` charges the spread-cross only (participation is undefined without an ADV).
    #[must_use]
    pub fn notional_cost(&self, notional: Decimal, adv_notional: Decimal) -> Decimal {
        let participation = if adv_notional > Decimal::ZERO {
            notional / adv_notional
        } else {
            Decimal::ZERO
        };
        notional * self.cost_fraction(participation)
    }

    /// `half_spread` as `f64` (capacity is an f64 model). Panics only on a non-representable `Decimal`,
    /// which the quantized coefficients never are.
    #[must_use]
    pub fn half_spread_f64(&self) -> f64 {
        self.half_spread
            .to_f64()
            .expect("quantized half_spread is representable as f64")
    }

    /// `impact_coeff` as `f64` (capacity's participation coefficient).
    #[must_use]
    pub fn impact_coeff_f64(&self) -> f64 {
        self.impact_coeff
            .to_f64()
            .expect("quantized impact_coeff is representable as f64")
    }

    /// `impact_exponent` (β) as `f64` (capacity's participation exponent).
    #[must_use]
    pub fn impact_exponent_f64(&self) -> f64 {
        self.impact_exponent
            .to_f64()
            .expect("quantized impact_exponent is representable as f64")
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

/// Fit a [`SlippageCalibration`] from the venue's own trade + quote history (maxdama §7.7 + QE-440).
///
/// - `half_spread` = median of `(ask − bid) / (2·mid)` over `quotes` (`mid = (ask+bid)/2`, `mid > 0`).
/// - `impact_coeff` = **binned zero-intercept least-squares** slope of *signed* fractional impact vs the
///   **participation regressor** `u^β`: `signed_impact = dir·(price − pre_mid)/pre_mid`,
///   `u = (qty·price) / adv_notional`, `β = ` [`DEFAULT_IMPACT_EXPONENT`], `dir = +1` (Buy) / `−1` (Sell)
///   — the aggressor sign taken directly (no Lee-Ready). Trades are sorted by size, split into `bins`
///   equal-count buckets, and the slope through the per-bucket means is `Σ(x̄·ȳ) / Σ(x̄²)`.
/// - `impact_exponent` = [`DEFAULT_IMPACT_EXPONENT`] (β is held at the √-law prior — robustly fitting an
///   exponent needs far more data than binning here provides; the panel's prior is `β ∈ [0.2, 0.5]`).
///
/// `adv_notional` ($ of rolling ADV) makes participation dimensionless. Degenerate inputs (no usable
/// quotes/trades, non-positive `adv_notional`, a zero slope denominator) fall back to the corresponding
/// [`SlippageCalibration::default`] coefficient. All arithmetic is exact `Decimal` (`u^β` via
/// deterministic pure-Decimal [`MathematicalOps::powd`]), so the fit is byte-reproducible on a pinned
/// input snapshot.
#[must_use]
pub fn fit_slippage_calibration(
    trades: &[SizedTrade],
    quotes: &[QuoteSample],
    adv_notional: Decimal,
    bins: usize,
) -> SlippageCalibration {
    let default = SlippageCalibration::default();
    let beta = default.impact_exponent;

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

    // --- impact_coeff = binned zero-intercept LS slope of signed impact vs the participation regressor ---
    // (u^β, signed_impact) pairs, sorted by participation so equal-count bins are size-ordered.
    let impact_coeff = if adv_notional > Decimal::ZERO {
        let mut points: Vec<(Decimal, Decimal)> = trades
            .iter()
            .filter(|t| {
                t.qty > Decimal::ZERO && t.pre_mid > Decimal::ZERO && t.price > Decimal::ZERO
            })
            .map(|t| {
                let participation = (t.qty * t.price) / adv_notional;
                let regressor = participation.powd(beta); // deterministic pure-Decimal power
                let dir = match t.side {
                    Side::Buy => Decimal::ONE,
                    Side::Sell => Decimal::NEGATIVE_ONE,
                };
                let signed_impact = dir * (t.price - t.pre_mid) / t.pre_mid;
                (regressor, signed_impact)
            })
            .collect();
        points.sort_by_key(|p| p.0);
        binned_slope(&points, bins).unwrap_or(default.impact_coeff)
    } else {
        default.impact_coeff
    };

    SlippageCalibration::new(half_spread, impact_coeff, beta)
}

/// Zero-intercept least-squares slope through the means of `bins` equal-count buckets of size-sorted
/// `points` (`(x = participation^β, y = signed_impact)`): `Σ(x̄·ȳ) / Σ(x̄²)`. `None` if there are no
/// points or the denominator is zero.
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

/// The realised decision-to-fill **implementation shortfall** for one fill (QE-444, maxdama §7.3), as a
/// **signed fraction of the decision price, charged in the trade direction** — the measurement primitive the
/// live/shadow path (QE-435) accumulates to *fit* the [`SlippageCalibration::alpha_loss`] coefficient.
///
/// - `side` — the fill's aggressor side (the perp feed carries it, as in [`SizedTrade`]).
/// - `decision_price` — the bar-**close** mark the signal decided on (`> 0`).
/// - `fill_price` — the achieved **next-bar-open** fill.
///
/// Returns, per unit of the trade's direction:
/// - Buy: `(fill − decision) / decision` — **positive when adverse** (price drifted up before the buy).
/// - Sell: `(decision − fill) / decision` — **positive when adverse** (price drifted down before the sell).
///
/// A non-positive `decision_price` yields `0` (undefined baseline). Exact `Decimal`, deterministic.
#[must_use]
pub fn realized_alpha_loss(side: Side, decision_price: Decimal, fill_price: Decimal) -> Decimal {
    if decision_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let dir = match side {
        Side::Buy => Decimal::ONE,
        Side::Sell => Decimal::NEGATIVE_ONE,
    };
    dir * (fill_price - decision_price) / decision_price
}

/// Accumulates realised decision-to-fill shortfalls ([`realized_alpha_loss`]) over live/shadow fills and
/// yields their **mean** — the deferred calibration of [`SlippageCalibration::alpha_loss`] (QE-444/QE-435).
///
/// The historical backtest has no realised close→open drift to observe, so this stays empty (and the
/// coefficient stays `0`) until the live/shadow execution path exists. When it does, feed each fill's
/// `(side, decision_price, fill_price)` via [`observe`](Self::observe); [`mean`](Self::mean) — clamped at
/// `0` so a *favourable* drift is never credited as a negative cost into the selection fitness — is the
/// fitted coefficient handed to [`SlippageCalibration::with_alpha_loss`]. Exact `Decimal`, order-independent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlphaLossAccumulator {
    sum: Decimal,
    count: u64,
}

impl AlphaLossAccumulator {
    /// A fresh, empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one realised fill's decision-to-fill shortfall (`decision_price` = bar-close mark,
    /// `fill_price` = next-bar-open fill).
    pub fn observe(&mut self, side: Side, decision_price: Decimal, fill_price: Decimal) {
        self.sum += realized_alpha_loss(side, decision_price, fill_price);
        self.count += 1;
    }

    /// Number of observed fills.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The mean realised shortfall, **clamped at `0`** (a favourable net drift is not credited as a negative
    /// cost), or `None` if nothing has been observed. This is the fitted `alpha_loss` coefficient.
    #[must_use]
    pub fn mean(&self) -> Option<Decimal> {
        if self.count == 0 {
            return None;
        }
        let mean = self.sum / Decimal::from(self.count);
        Some(mean.max(Decimal::ZERO))
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
    fn default_is_the_sqrt_law_seed() {
        let cal = SlippageCalibration::default();
        assert_eq!(cal.half_spread, d("0.0001")); // 1bp spread-cross
        assert_eq!(cal.impact_coeff, d("0.01")); // 1% impact fraction at 100% participation
        assert_eq!(cal.impact_exponent, d("0.5")); // √-in-participation law
    }

    #[test]
    fn impact_is_concave_in_participation() {
        // QE-440: doubling participation at fixed coefficient multiplies the impact fraction by 2^β < 2
        // (sub-linear), unlike the old linear-in-qty term. At β = 0.5 the ratio is exactly √2.
        let cal = SlippageCalibration::default();
        let u = d("0.01");
        let a = cal.participation_impact(u);
        let b = cal.participation_impact(u * d("2"));
        assert!(a > Decimal::ZERO && b > a);
        let ratio = (b / a).round_dp(6);
        assert_eq!(ratio, d("2").sqrt().unwrap().round_dp(6)); // √2 ≈ 1.414214, strictly < 2
        assert!(ratio < d("2"));
    }

    #[test]
    fn participation_impact_reduces_sensibly_and_is_deterministic() {
        let cal = SlippageCalibration::default();
        // No participation (or missing ADV) ⇒ no impact term (spread-cross only).
        assert_eq!(cal.participation_impact(Decimal::ZERO), Decimal::ZERO);
        assert_eq!(cal.participation_impact(d("-1")), Decimal::ZERO);
        assert_eq!(
            cal.notional_cost(d("1000"), Decimal::ZERO),
            d("1000") * cal.half_spread
        );
        // u = 1 (100% of ADV) ⇒ impact fraction == impact_coeff exactly.
        assert_eq!(cal.participation_impact(Decimal::ONE), cal.impact_coeff);
        // Determinism: the pure-Decimal power pins an exact expected literal (0.01·√0.01 = 0.001).
        assert_eq!(cal.participation_impact(d("0.01")), d("0.001"));
        assert_eq!(
            cal.participation_impact(d("0.01")),
            SlippageCalibration::default().participation_impact(d("0.01"))
        );
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
            SlippageCalibration::new(d("0.0002"), DEFAULT_IMPACT_COEFF, DEFAULT_IMPACT_EXPONENT)
                .content_hash()
        );
        assert_ne!(
            base,
            SlippageCalibration::new(DEFAULT_HALF_SPREAD, d("0.02"), DEFAULT_IMPACT_EXPONENT)
                .content_hash()
        );
        assert_ne!(
            base,
            SlippageCalibration::new(DEFAULT_HALF_SPREAD, DEFAULT_IMPACT_COEFF, d("0.3"))
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

        let adv = d("10000"); // $ of rolling ADV, making participation dimensionless
        let a = fit_slippage_calibration(&trades, &quotes, adv, DEFAULT_IMPACT_BINS);
        let b = fit_slippage_calibration(&trades, &quotes, adv, DEFAULT_IMPACT_BINS);
        // Byte-reproducible on the same pinned input.
        assert_eq!(a, b);
        assert_eq!(a.content_hash(), b.content_hash());

        // half_spread read off the observed distribution (median 1bp).
        assert_eq!(a.half_spread, d("0.0001"));
        // β is held at the √-law prior.
        assert_eq!(a.impact_exponent, DEFAULT_IMPACT_EXPONENT);
        // A positive, non-degenerate participation coefficient was fit from the size/impact relationship.
        assert!(a.impact_coeff > Decimal::ZERO);
    }

    #[test]
    fn fit_falls_back_to_defaults_on_empty_input() {
        let cal = fit_slippage_calibration(&[], &[], d("10000"), DEFAULT_IMPACT_BINS);
        assert_eq!(cal, SlippageCalibration::default());
    }

    #[test]
    fn fit_falls_back_to_default_coeff_without_adv() {
        // No ADV ⇒ participation is undefined ⇒ the coefficient falls back to the seed (β/half_spread
        // still fit from the venue where present).
        let trades = vec![trade(Side::Buy, "10", "100.05", "100")];
        let cal = fit_slippage_calibration(&trades, &[], Decimal::ZERO, DEFAULT_IMPACT_BINS);
        assert_eq!(
            cal.impact_coeff,
            SlippageCalibration::default().impact_coeff
        );
    }

    #[test]
    fn skips_lee_ready_uses_carried_aggressor_sign() {
        // Two trades with the SAME price move magnitude but opposite carried aggressor sides both yield a
        // POSITIVE (adverse) impact — proving the sign is taken from `side`, not inferred by Lee-Ready.
        let buy = trade(Side::Buy, "5", "100.02", "100");
        let sell = trade(Side::Sell, "5", "99.98", "100");
        let up = fit_slippage_calibration(&[buy], &[], d("10000"), 1);
        let down = fit_slippage_calibration(&[sell], &[], d("10000"), 1);
        assert!(up.impact_coeff > Decimal::ZERO);
        assert!(down.impact_coeff > Decimal::ZERO);
    }

    // --- QE-444 decision-to-fill alpha-loss (implementation shortfall) -------------------------------

    #[test]
    fn default_alpha_loss_is_zero_and_serialises_byte_identically_hash_unmoved() {
        // Measurement-deferred, golden-safe: the default coefficient is 0, and `skip_serializing_if` OMITS
        // the field at 0, so a default calibration serialises to the SAME bytes as the pre-QE-444 3-field
        // record — its content_hash (hence every downstream golden/vintage) is unmoved.
        let cal = SlippageCalibration::default();
        assert_eq!(cal.alpha_loss, Decimal::ZERO);
        assert_eq!(cal.alpha_loss, DEFAULT_ALPHA_LOSS);
        let json = serde_json::to_string(&cal).unwrap();
        assert!(
            !json.contains("alpha_loss"),
            "default alpha_loss must be omitted from JSON so the content_hash is byte-stable: {json}"
        );
        // Old JSON that predates the field deserialises (serde default) and round-trips byte-identically.
        let back: SlippageCalibration = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cal);
        assert_eq!(back.alpha_loss, Decimal::ZERO);
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn directional_drift_is_odd_in_side_unlike_the_side_blind_half_spread() {
        // QE-444: alpha-loss is DIRECTIONAL — a buy pays the up-drift (+γ), a sell the down-drift (−γ), an
        // ODD function of side; whereas half_spread is side-blind (EVEN). This is why alpha-loss cannot be
        // folded into half_spread.
        let cal = SlippageCalibration::default().with_alpha_loss(d("0.001"));
        assert_eq!(cal.directional_drift(Side::Buy), d("0.001"));
        assert_eq!(cal.directional_drift(Side::Sell), d("-0.001"));
        assert_eq!(
            cal.directional_drift(Side::Buy),
            -cal.directional_drift(Side::Sell),
            "drift must be odd in side (directional)"
        );
        // half_spread is even in side (the existing symmetric cost takes no side at all).
        assert_ne!(
            cal.directional_drift(Side::Buy),
            cal.directional_drift(Side::Sell),
            "a non-zero alpha-loss must actually distinguish the two sides"
        );
        // The cost magnitude is symmetric (signal-aligned adverse drift), always ≥ 0.
        let notional = d("1000");
        assert_eq!(cal.alpha_loss_cost(notional), d("1")); // 1000 · 0.001
        assert!(cal.alpha_loss_cost(notional) >= Decimal::ZERO);
    }

    #[test]
    fn alpha_loss_is_separate_from_the_symmetric_shared_cost_capacity_parity_preserved() {
        // The shared symmetric cost that capacity ALSO consumes must NOT include alpha-loss — otherwise a
        // directional friction term would leak into capacity and break the QE-431 parity. A non-zero
        // alpha-loss leaves cost_fraction / notional_cost untouched.
        let base = SlippageCalibration::default();
        let with = base.clone().with_alpha_loss(d("0.005"));
        let u = d("0.01");
        assert_eq!(with.cost_fraction(u), base.cost_fraction(u));
        assert_eq!(
            with.notional_cost(d("1000"), d("100000")),
            base.notional_cost(d("1000"), d("100000"))
        );
    }

    #[test]
    fn non_zero_alpha_loss_moves_the_content_hash_field_only_diff() {
        // Only a fitted, non-zero coefficient makes the field appear and the hash move (a clean field-only
        // diff) — the golden-move path, taken via real code when live data exists.
        let base = SlippageCalibration::default();
        let fitted = base.clone().with_alpha_loss(d("0.0007"));
        assert_ne!(base.content_hash(), fitted.content_hash());
        let json = serde_json::to_string(&fitted).unwrap();
        assert!(json.contains("alpha_loss"));
        // Quantized + serialize-idempotent (the content-hash invariant holds for the new field too).
        let back: SlippageCalibration = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fitted);
        assert_eq!(back.content_hash(), fitted.content_hash());
    }

    #[test]
    fn realized_alpha_loss_signs_adverse_drift_by_trade_direction() {
        // Buy filled ABOVE the decision price ⇒ adverse (positive). Sell filled BELOW ⇒ adverse (positive).
        // The opposite drifts are favourable (negative).
        let decision = d("100");
        assert_eq!(
            realized_alpha_loss(Side::Buy, decision, d("100.1")),
            d("0.001")
        );
        assert_eq!(
            realized_alpha_loss(Side::Sell, decision, d("99.9")),
            d("0.001")
        );
        assert!(realized_alpha_loss(Side::Buy, decision, d("99.9")) < Decimal::ZERO);
        assert!(realized_alpha_loss(Side::Sell, decision, d("100.1")) < Decimal::ZERO);
        // Undefined baseline ⇒ 0.
        assert_eq!(
            realized_alpha_loss(Side::Buy, Decimal::ZERO, d("100")),
            Decimal::ZERO
        );
    }

    #[test]
    fn accumulator_means_realised_shortfalls_and_feeds_the_coefficient() {
        // Two adverse buys (0.001 and 0.003) mean to 0.002 → the fitted alpha_loss.
        let mut acc = AlphaLossAccumulator::new();
        assert_eq!(acc.mean(), None); // empty ⇒ nothing to fit
        acc.observe(Side::Buy, d("100"), d("100.1")); // +0.001
        acc.observe(Side::Buy, d("100"), d("100.3")); // +0.003
        assert_eq!(acc.count(), 2);
        assert_eq!(acc.mean(), Some(d("0.002")));

        // Feeds the coefficient sink verbatim.
        let cal = SlippageCalibration::default().with_alpha_loss(acc.mean().unwrap());
        assert_eq!(cal.alpha_loss, d("0.002"));

        // A net-favourable sample is clamped to 0 (never credited as negative cost).
        let mut fav = AlphaLossAccumulator::new();
        fav.observe(Side::Buy, d("100"), d("99.5")); // favourable
        assert_eq!(fav.mean(), Some(Decimal::ZERO));
    }
}
