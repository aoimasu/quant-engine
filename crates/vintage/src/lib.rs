//! qe-vintage (QE-129) — the vintage artefact format.
//!
//! A **vintage** is the unit handed to runtime: the chromosomes (strategy genomes — `qe_wfo::Genome`,
//! QE-110/123), the ensemble (materialised as per-chromosome weights — the capacity-capped output of
//! QE-126/127/128), and the per-vintage calibration profile (`qe_risk::CalibrationProfile`, QE-116),
//! tagged with a resolvable [`Lineage`] (QE-006) and pinned by a **content hash**. The format is the
//! output of Area ⑦; it is read-only-loadable by runtime (QE-219), which is out of scope here.
//!
//! Being *downstream* of the search⟂portfolio firewall (QE-001/QE-132 govern information flow during
//! search/portfolio construction, not a final artefact recording their outputs), the vintage may bundle
//! both sides' data. It stores the ensemble as plain `weights`, not `qe_ensemble`'s search types, so the
//! artefact is pure data — runtime loads it without pulling in any search/portfolio logic.

use std::io::{Read, Write};
use std::path::PathBuf;

use qe_determinism::Lineage;
use qe_risk::{CalibrationProfile, PortfolioSizer, ShockConfig, SlippageCalibration};
use qe_signal::{CatalogueIdentity, Genome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod schema;

/// The vintage artefact format version. Part of the hashed content, so a format change changes the hash.
///
/// - `2` (QE-130): added [`VintageContent::worst_case_loss`].
/// - `3` (QE-402): added [`VintageContent::catalogue`] (the pinned catalogue identity), asserted
///   exactly at the load boundary — see [`schema`].
/// - `4` (QE-431): added [`VintageContent::slippage`] (the content-addressed slippage/impact
///   calibration shared by friction & capacity), riding the lineage alongside `calibration`.
/// - `5` (QE-433): added [`VintageContent::sizer`] (the content-addressed advisory portfolio-Kelly
///   leverage multiplier), riding the lineage alongside `slippage`.
/// - `6` (QE-440): reshaped [`VintageContent::slippage`] to the concave √-in-participation impact model —
///   the `SlippageCalibration` hashed fields changed (`impact_per_notional` + `reference_mark` →
///   participation `impact_coeff` + `impact_exponent` β).
/// - `7` (QE-441): added [`VintageContent::shocks`] (the frozen, content-addressed bar-level scenario-shock
///   set that shaped the tail-aware `size_bps` in the single-strategy sizing fitness), riding the lineage
///   alongside `slippage` / `sizer`.
/// - `8` (QE-467): the research-flow persistence foundation — added [`VintageContent::seal_evidence`]
///   (the gate's own tradability + deflation outputs: DSR/PBO/SPA, realised turnover, `capacity_usd`,
///   cost-stress `min{1×,2×}` net, and the deferred IC/FDR/uncensored-PBO slots),
///   [`VintageContent::holdout_series`] (the canonical net-of-cost holdout return series on the DEPLOYED
///   capacity-capped weights, addressable by [`HoldoutReturnSeries::handle`]), and
///   [`VintageContent::provenance`] (hashed `data_provenance` + the extended lineage the research flow
///   needs: holdout split, holdout regime composition, per-holdout consultation count, and steer delta).
///   The whole schema is defined here; downstream tickets (QE-458/QE-460) **populate** the deferred
///   fields under this single bump — nobody bumps the version again.
///
/// **QE-469 rides version 8 (no bump).** The additive [`SealEvidence::cpcv`] slot (the CPCV OOS
/// distribution) is `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a `None`-bearing
/// vintage is byte-identical to the pre-QE-469 artefact — exactly the QE-454 additive-`Option` precedent,
/// no `VINTAGE_FORMAT_VERSION` change, no golden drift.
pub const VINTAGE_FORMAT_VERSION: u16 = 8;

/// The fixed **hash-stable rounding scale** (`10^12`) every hashed `f64` is canonicalised to before the
/// content hash is computed (QE-482 / QE-416). The content hash is the digest of `serde_json`'s output,
/// whose default float parser is not correctly-rounded: a 17-significant-digit `f64` can re-parse to a
/// neighbouring `f64` that serialises one ULP differently, breaking the content-hash verify on reload.
/// Rounding to 12 decimal places keeps every hashed `f64` inside the parser's exact range (far finer than
/// any allocation / risk / statistic figure needs) so seal → load is byte-stable.
///
/// This is the **same** rule the train-job producer applies (`crates/cli/src/jobs/train.rs` `hash_stable`,
/// `HASH_STABLE_SCALE = 1e12`); QE-482 lifts it here so [`Vintage::seal`] enforces the invariant at the
/// type boundary and producer + type agree by construction. The producer helper stays as belt-and-braces.
pub const HASH_STABLE_SCALE: f64 = 1e12;

/// Round `value` to the fixed [`HASH_STABLE_SCALE`] precision so it serialises to a bounded-precision,
/// round-trip-stable decimal (QE-482). Non-finite inputs pass through unchanged — [`VintageContent::validate`]
/// rejects them at seal, so the rounding never has to reason about `NaN`/`∞`. Idempotent: rounding an
/// already-rounded value is a no-op, so canonicalising producer output that already applied this rule
/// leaves the hashed bytes unchanged.
#[must_use]
pub fn hash_stable(value: f64) -> f64 {
    if value.is_finite() {
        (value * HASH_STABLE_SCALE).round() / HASH_STABLE_SCALE
    } else {
        value
    }
}

/// The persisted **seal evidence** (QE-467): the gate's own tradability + deflation outputs, carried into
/// the sealed artefact so every downstream surface (inspector QE-456/457, leaderboard QE-466, flow
/// QE-460) **reads** — never recomputes — them. Part of the hashed content (content-addressed).
///
/// The DSR/PBO/SPA + turnover + `capacity_usd` are the ensemble train gate's own outputs and are
/// populated on the real seal path. The `Option` fields are schema slots defined here and populated by
/// the path that actually computes them: `uncensored_pbo`/`ic`/`fdr` are GP/IC-screen concerns (absent on
/// the normal train path, exactly like `GateSnapshot::uncensored_pbo`), and `cost_stress_net_min` is the
/// deployed ensemble's `min{1×,2×}` cost-stressed net (design §4.6a).
///
/// **QE-469 (additive, no version bump).** The `cpcv` slot carries the CPCV out-of-sample **distribution**
/// summary (median/IQR/percentile of held-out Sharpe/DSR + the fraction clearing the DSR floor) — the OOS
/// evidence that replaces the single G1 terminal-holdout point estimate. Like the QE-454 `Option` slots it
/// is `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a vintage that does not run CPCV
/// (`None`) serialises **byte-identically** to the pre-QE-469 artefact — no `VINTAGE_FORMAT_VERSION` bump,
/// no golden drift. When populated it enters the hashed content (content-addressed, changes the vintage id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SealEvidence {
    /// Deflated Sharpe Ratio (QE-131) the DSR criterion evaluated.
    pub dsr: f64,
    /// Probability of Backtest Overfitting (CSCV, QE-131).
    pub pbo: f64,
    /// White's Reality Check / SPA data-snooping p-value (QE-131).
    pub spa_pvalue: f64,
    /// Effective number of trials the DSR deflated against.
    pub n_trials: u64,
    /// Realised turnover of the DEPLOYED capacity-capped ensemble over the train window — the exact
    /// round-trip-notional-per-period figure the sealed capacity model prices with (QE-431/440).
    pub realised_turnover: f64,
    /// Modelled deployable capacity in USD of the DEPLOYED book at the target AUM (QE-431/440).
    pub capacity_usd: f64,
    /// Cost-stressed net: `min` over friction multipliers `m ∈ {1×,2×}` of the deployed ensemble's
    /// net-of-cost holdout return (design §4.6a, QE-431/450). `None` on a path that does not run the
    /// cost-stress sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_stress_net_min: Option<f64>,
    /// The uncensored PBO the GP/evolve monitor surfaces (QE-454). Absent on the normal (non-evolve) train
    /// path — populated by the evolve/GP path, matching `GateSnapshot::uncensored_pbo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncensored_pbo: Option<f64>,
    /// Information Coefficient (QE-434 rank-IC) of the admitted factor screen. `None` on paths that do not
    /// run the IC screen (the ensemble train path) — populated by the IC-screen/evolve path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ic: Option<f64>,
    /// Benjamini–Hochberg false-discovery level the IC screen admitted at (QE-434). `None` where no IC
    /// screen ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fdr: Option<f64>,
    /// The CPCV out-of-sample **distribution** summary (QE-469): the median/IQR/percentile of the held-out
    /// Sharpe/DSR distribution and the fraction of held-out paths clearing the DSR floor — the promotion-
    /// facing OOS evidence that replaces the single G1 terminal-holdout point estimate. **QE-477:** built
    /// over the FROZEN HOLDOUT series (the deployed ensemble's net-of-cost returns on the untouched
    /// holdout), **not** the in-sample selection window — so "out-of-sample" is literal, every held-out
    /// configuration's Sharpe/DSR is measured outside the window the ensemble was chosen on. `None` on a
    /// path that does not run CPCV (byte-identical to pre-QE-469 — see the struct doc). Content-addressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpcv: Option<CpcvSummary>,
    /// The sealed **G1 promotion verdict** (QE-476): the content-hashed mirror of the run-doc
    /// `G1Decision` — the `promoted` flag plus each criterion's frozen name/value/threshold. Under the
    /// **write-but-mark** policy a G1-failed candidate is still sealed and written, marked
    /// `promoted = false`, so downstream selectors read the verdict and refuse a non-promoted vintage.
    /// Additive `Option` (`skip_serializing_if` when `None`), exactly the QE-454/QE-469 precedent, so a
    /// verdict-less vintage serialises **byte-identically** to the pre-QE-476 artefact — no
    /// `VINTAGE_FORMAT_VERSION` bump. When populated it enters the hashed content (content-addressed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionVerdict>,
}

impl SealEvidence {
    /// The **fail-closed** promotion read a downstream selector uses (QE-476): `true` only when a sealed
    /// verdict is present **and** records `promoted = true`. A verdict-less vintage (`None`, e.g. a
    /// pre-QE-476 or otherwise unmarked artifact) reads as **not promoted** — a selector refuses it rather
    /// than default-accepting an artifact whose gate outcome the hash cannot vouch for.
    #[must_use]
    pub fn is_promoted(&self) -> bool {
        self.promotion.as_ref().is_some_and(|v| v.promoted)
    }
}

/// The CPCV out-of-sample distribution summary (QE-469 — López de Prado Ch. 12.4), persisted in the sealed
/// [`SealEvidence`] so every downstream surface **reads** — never recomputes — the OOS distribution. Built
/// by the seal path from `qe_validation::CpcvDistribution` over the deployed ensemble's net-of-cost
/// **holdout** series (QE-477 — the frozen, untouched holdout, not the in-sample selection window).
///
/// Carries the per-held-out-path Sharpe and DSR vectors (content-addressed by inclusion in the hashed
/// content) plus the reduced summary the promotion gate and the report surface consume. Every field is a
/// finite `f64` (checked in [`VintageContent::validate`]); the seal writer rounds each to a hash-stable
/// precision (like `weights`) so the whole block round-trips byte-identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CpcvSummary {
    /// Number of contiguous blocks `S` the series was split into.
    pub blocks: u32,
    /// Number of held-out paths (balanced `C(S, S/2)` partitions) the distribution was built from.
    pub n_paths: u32,
    /// Median held-out Sharpe.
    pub median_sharpe: f64,
    /// 25th-percentile held-out Sharpe (IQR lower bound).
    pub sharpe_iqr_lo: f64,
    /// 75th-percentile held-out Sharpe (IQR upper bound).
    pub sharpe_iqr_hi: f64,
    /// 5th-percentile held-out Sharpe.
    pub sharpe_p05: f64,
    /// 95th-percentile held-out Sharpe.
    pub sharpe_p95: f64,
    /// Median held-out DSR.
    pub median_dsr: f64,
    /// The lower (5th) percentile of held-out DSR — the figure the promotion gate decides on.
    pub dsr_p05: f64,
    /// Fraction of held-out paths whose DSR ≥ the floor (0.95).
    pub frac_dsr_ge_floor: f64,
    /// Per-held-out-path Sharpe ratios, in partition order.
    pub path_sharpes: Vec<f64>,
    /// Per-held-out-path Deflated Sharpe Ratios, in partition order.
    pub path_dsrs: Vec<f64>,
}

impl CpcvSummary {
    /// The distribution's **complete** set of scalar summary figures (name, value) — every scalar the
    /// report surface reads and the exact set [`VintageContent::validate`] finite-checks (the per-path
    /// Sharpe/DSR *vectors* are checked separately). `sharpe_p95` is included so no percentile is silently
    /// dropped.
    fn summary_fields(&self) -> [(&'static str, f64); 8] {
        [
            ("cpcv.median_sharpe", self.median_sharpe),
            ("cpcv.sharpe_iqr_lo", self.sharpe_iqr_lo),
            ("cpcv.sharpe_iqr_hi", self.sharpe_iqr_hi),
            ("cpcv.sharpe_p05", self.sharpe_p05),
            ("cpcv.sharpe_p95", self.sharpe_p95),
            ("cpcv.median_dsr", self.median_dsr),
            ("cpcv.dsr_p05", self.dsr_p05),
            ("cpcv.frac_dsr_ge_floor", self.frac_dsr_ge_floor),
        ]
    }
}

/// One sealed G1 criterion's frozen evidence (QE-476): the criterion name, whether it passed, the
/// observed value, and the **threshold it was judged against** — mirrors `qe_gate::CriterionResult` but
/// lives in `qe-vintage` so the artefact format keeps no `qe-gate` dependency. Freezing the threshold into
/// the hash is the point: a later drift of a threshold constant cannot silently re-classify an
/// already-sealed artifact, because the value-vs-threshold comparison it was judged on is content-addressed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SealedCriterion {
    /// Short identifier for the criterion (matches the run-doc `G1Decision` criterion name).
    pub name: String,
    /// Whether this criterion passed.
    pub passed: bool,
    /// The observed value.
    pub value: f64,
    /// The threshold the value was compared against (frozen into the hash).
    pub threshold: f64,
}

/// The sealed **G1 promotion verdict** (QE-476): the content-hashed mirror of the run-doc
/// `qe_gate::G1Decision`, so a downstream selector reading only the content-addressed vintage — never the
/// separate, non-content-addressed run doc — can tell a gate-passed artifact from a failed one and recover
/// each criterion's frozen threshold. **Write-but-mark policy:** a G1-failed candidate is still sealed and
/// written (preserving negative-result lineage for research), but carries `promoted = false`; a selector
/// must read [`promoted`](Self::promoted) and refuse a non-promoted vintage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PromotionVerdict {
    /// Whether the vintage cleared **every** G1 criterion (`true` iff gate-passed / promotable).
    pub promoted: bool,
    /// The per-criterion evidence with frozen thresholds, in evaluation order.
    pub criteria: Vec<SealedCriterion>,
}

/// The canonical **net-of-cost holdout return series on the DEPLOYED capacity-capped weights** (QE-438),
/// persisted per vintage and content-addressed. It is the exact series the leaderboard's cross-vintage
/// correlation (QE-430 R(N)/Fisher-z) and the inspector consume — **never** gross / equal-weight /
/// lone-Sharpe. Addressable by [`handle`](Self::handle) so the detail endpoint (QE-456) returns a ref, not
/// a re-run. The seal writer rounds each return to a hash-stable precision (like `weights`) so it
/// round-trips byte-identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HoldoutReturnSeries {
    /// Per-bar net-of-cost returns of the deployed ensemble over the frozen holdout.
    pub returns: Vec<f64>,
}

impl HoldoutReturnSeries {
    /// The content handle: lowercase-hex SHA-256 over the series' canonical JSON — the stable ref a detail
    /// endpoint returns instead of re-running the backtest.
    ///
    /// # Errors
    /// [`VintageError::Serialize`] if the series cannot be serialised.
    pub fn handle(&self) -> Result<String, VintageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| VintageError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }
}

/// The data provenance of the bars a vintage was trained / validated on (QE-467): whether the pinned
/// input snapshot is real market data, a synthetic generator's output, or a labelled mix. Hashed into the
/// vintage id, so a synthetic-derived vintage is no longer indistinguishable from a real one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DataProvenance {
    /// Real market data (the default for a train over a real/loaded store).
    #[default]
    Real,
    /// Deterministic synthetic data (the `qe ingest --synthetic` offline generator).
    Synthetic,
    /// A labelled mix of real and synthetic coverage — never a silent blend.
    Mixed,
}

/// An inclusive-exclusive labelled range (bar timestamps or index labels). Kept as opaque strings so the
/// schema is format-agnostic — the flow (QE-460) writes the concrete labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TimeRange {
    /// Inclusive start label.
    pub start: String,
    /// Exclusive end label.
    pub end: String,
}

/// The frozen holdout split (design §4) the gate consulted, recorded so the verdict's bars are
/// reproducible from the sealed artefact. Schema defined by QE-467; **populated by QE-460**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HoldoutSplit {
    /// The frozen holdout window (`None` until QE-460 writes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout_range: Option<TimeRange>,
    /// The train window disjoint from the holdout (`None` until QE-460 writes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train_range: Option<TimeRange>,
    /// Embargo bars purged between the train window and the holdout (QE-113/117).
    pub embargo_bars: u64,
}

/// One regime's share of the holdout window (QE-125): the regime label and how many holdout bars carried
/// it. The holdout regime composition (design §4) — populated by QE-460.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RegimeShare {
    /// The regime label (QE-125) the bars were classified into.
    pub regime: String,
    /// Number of holdout bars in this regime.
    pub bars: u64,
}

/// The steer delta the search recorded (design §6, QE-458): the indicator-subset the campaign steered to
/// plus the budget knobs. Schema defined by QE-467; **populated by QE-458**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SteerDelta {
    /// Hash of the steered indicator subset (which catalogue/evolved indicators were in play).
    pub indicator_subset_hash: String,
    /// Search generations the steered budget ran.
    pub generations: u64,
    /// Population / variation steps per direction.
    pub population: u64,
    /// WFO windows the steered run scored over.
    pub windows: u64,
    /// Cross-validation folds the steered run scored over.
    pub folds: u64,
}

impl SteerDelta {
    /// The SHA-256 (64 lowercase hex) **order-independent set hash** of the steered indicator subset —
    /// the catalogue-indicator ids in play plus any included evolved-formula hashes, sorted + deduped so
    /// the hash is a stable set identity regardless of listing order. The single source of truth both the
    /// server (`validate`/record) and the CLI seal path address the steered feature space by (QE-458).
    #[must_use]
    pub fn subset_hash(catalogue_ids: &[String], evolved_formula_hashes: &[String]) -> String {
        let mut items: Vec<String> = catalogue_ids
            .iter()
            .map(|s| format!("cat:{s}"))
            .chain(evolved_formula_hashes.iter().map(|s| format!("evo:{s}")))
            .collect();
        items.sort();
        items.dedup();
        let mut hasher = Sha256::new();
        for item in items {
            hasher.update(item.as_bytes());
            hasher.update([0u8]); // length-safe separator
        }
        hex(&hasher.finalize())
    }

    /// Build the recorded steer delta from the applied steer knobs (QE-458 populates QE-467's schema):
    /// the [`subset_hash`](Self::subset_hash) over the feature space in play plus the budget / window /
    /// fold counts the steered search actually ran.
    #[must_use]
    pub fn from_parts(
        catalogue_ids: &[String],
        evolved_formula_hashes: &[String],
        generations: u64,
        population: u64,
        windows: u64,
        folds: u64,
    ) -> Self {
        SteerDelta {
            indicator_subset_hash: Self::subset_hash(catalogue_ids, evolved_formula_hashes),
            generations,
            population,
            windows,
            folds,
        }
    }
}

/// The **extended lineage / provenance block** (QE-467) riding the sealed vintage alongside the resolvable
/// [`Lineage`] — the "sibling lineage block on `VintageContent`" the ticket permits (so the widely-shared
/// `qe_determinism::Lineage` stays untouched). Part of the hashed content, so `data_provenance` and every
/// populated field changes the vintage id.
///
/// QE-467 defines the **whole** schema and populates `data_provenance`. The remaining fields are the
/// research flow's, populated **downstream under this single bump**: QE-460 writes `holdout_split` /
/// `regime_composition` / `consultation_count`; QE-458 writes `steer_delta`. Nobody bumps the version
/// again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResearchProvenance {
    /// Whether the input data is real, synthetic, or a labelled mix (QE-467).
    pub data_provenance: DataProvenance,
    /// The frozen holdout split `{holdout_range, embargo, train_range}` (QE-460).
    pub holdout_split: HoldoutSplit,
    /// The holdout regime composition — the regimes the holdout spanned (QE-125 / QE-460).
    pub regime_composition: Vec<RegimeShare>,
    /// Per-holdout consultation count — the overlap-keyed budget QE-460 increments (design §4/§11.3).
    pub consultation_count: u64,
    /// The steer delta the search recorded (QE-458); `None` for an unsteered vintage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_delta: Option<SteerDelta>,
}

/// The hashed content of a vintage — everything the content hash covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VintageContent {
    /// Artefact format version ([`VINTAGE_FORMAT_VERSION`]).
    pub format_version: u16,
    /// Human / rollover identifier for this vintage (e.g. a date-stamped label).
    pub vintage_id: String,
    /// The strategy genomes (chromosomes) the ensemble selected (QE-110/123).
    pub chromosomes: Vec<Genome>,
    /// Per-chromosome ensemble weight, aligned to `chromosomes` (capacity-capped, QE-126/127/128).
    pub weights: Vec<f64>,
    /// The per-vintage calibration sidecar (QE-116).
    pub calibration: CalibrationProfile,
    /// The content-addressed slippage/impact calibration (QE-431) — the single source of truth that both
    /// the wfo friction cost model and the ensemble capacity model derive from. Riding it here in the
    /// hashed content (alongside `calibration`) ties the exact cost coefficients that priced selection into
    /// the vintage's reproducible lineage. Part of the hashed content, so it changes the vintage id.
    pub slippage: SlippageCalibration,
    /// The per-vintage advisory portfolio-Kelly sizer (QE-433) — the fractional (≤½) empirical-Kelly
    /// leverage multiplier solved on the realised **combined net-of-cost** series after the mask +
    /// capacity weights are fixed. The live netter scales the netted book by it and clamps the result
    /// **below** the pretrade leverage cap (the hard cap stays the backstop). Riding it here in the hashed
    /// content ties the chosen size into the vintage's reproducible lineage, like `slippage`. Part of the
    /// hashed content, so it changes the vintage id.
    pub sizer: PortfolioSizer,
    /// The frozen, content-addressed **bar-level scenario-shock set** (QE-441) that shaped the tail-aware
    /// `size_bps` in the single-strategy sizing fitness. The MAP-Elites / DE selection fitness ran the
    /// backtester with these bounded synthetic gap / funding-spike / ADL shocks injected at the price/bar
    /// level (drawn from the seeded portable RNG), so a larger size produced a larger drawdown and
    /// `log_growth` self-selected a lower leverage. Its severity/frequency are un-deflated researcher DOF,
    /// so the set is **frozen / pre-registered** (a fixed seed, not the run seed) and sealed here in the
    /// hashed content — pinning the exact shocks that priced sizing into the vintage's reproducible
    /// lineage, like `slippage` / `sizer`. Part of the hashed content, so it changes the vintage id.
    pub shocks: ShockConfig,
    /// Worst-case capital loss (a positive fraction) under the QE-130 stress set — the figure the
    /// vintage carries to gate G3 (QE-308). `None` until the stress engine
    /// (`qe_ensemble::stress::worst_case_loss`) has been run and its bare figure attached. Stored as a
    /// plain `f64`, not the `StressReport` type, so the vintage keeps no `qe-ensemble` dependency.
    pub worst_case_loss: Option<f64>,
    /// The pinned identity of the indicator catalogue the `chromosomes` were sealed against (QE-402):
    /// the `CATALOGUE_VERSION`, per-indicator state count, and an ordered indicator-id hash. Asserted
    /// **exactly** at the load boundary ([`schema::assert_schema`]) so a catalogue reorder or a
    /// same-width version bump is caught instead of silently re-addressing a clause to a different
    /// indicator. Part of the hashed content, so pinning it changes the vintage id.
    pub catalogue: CatalogueIdentity,
    /// The lineage that produced this vintage (QE-006).
    pub lineage: Lineage,
    /// The persisted **seal evidence** (QE-467): the gate's own tradability + deflation outputs
    /// (DSR/PBO/SPA, realised turnover, `capacity_usd`, cost-stress `min{1×,2×}` net, IC/FDR/uncensored-PBO
    /// slots), carried into the artefact so downstream reads (never recomputes) them. Part of the hashed
    /// content, so it changes the vintage id.
    pub seal_evidence: SealEvidence,
    /// The canonical **net-of-cost holdout return series on the DEPLOYED capacity-capped weights** (QE-438,
    /// QE-467), content-addressed and addressable by [`HoldoutReturnSeries::handle`]. Part of the hashed
    /// content, so it changes the vintage id.
    pub holdout_series: HoldoutReturnSeries,
    /// The **extended lineage / provenance block** (QE-467): hashed `data_provenance` plus the holdout
    /// split, holdout regime composition, per-holdout consultation count, and steer delta the research
    /// flow needs. Schema owned here; deferred fields populated downstream (QE-458/QE-460). Part of the
    /// hashed content, so it changes the vintage id.
    pub provenance: ResearchProvenance,
}

impl VintageContent {
    /// The canonical per-strategy ids the live breaker layer keys its calibration lookup by (QE-416):
    /// the positional index of each chromosome as a string (`["0", "1", …]`). This is the **single
    /// source of truth** for the strategy↔calibration mapping — the seal writes the
    /// [`CalibrationProfile`] `per_strategy` map under exactly these keys, and
    /// `BreakerLayer::from_calibration` looks them up with the same ids, so every sealed strategy is
    /// found (no unintended pre-gating of a calibrated member). A method, not a field, so it does not
    /// enter the content hash.
    #[must_use]
    pub fn strategy_ids(&self) -> Vec<String> {
        (0..self.chromosomes.len()).map(|i| i.to_string()).collect()
    }

    /// Canonicalise **every hashed `f64` field** to the fixed [`HASH_STABLE_SCALE`] precision (QE-482) —
    /// `weights`, `worst_case_loss`, every `holdout_series` return, every [`SealEvidence`] scalar +
    /// `Option`, and the [`CpcvSummary`] summary fields + per-path vectors. Called by [`Vintage::seal`]
    /// **before** validation and hashing, so the hash-stable rounding invariant lives at the type boundary
    /// that owns the hash — not only in the train-job producer. A future seal writer that constructs a
    /// `VintageContent` without routing every f64 through the producer's rounding still seals a byte-stable
    /// artefact, because `seal` rounds here by construction. Idempotent on already-rounded producer output
    /// (rounding a rounded value is a no-op), so it introduces no drift for the existing seal path.
    fn canonicalize(&mut self) {
        for w in &mut self.weights {
            *w = hash_stable(*w);
        }
        if let Some(loss) = self.worst_case_loss.as_mut() {
            *loss = hash_stable(*loss);
        }
        for r in &mut self.holdout_series.returns {
            *r = hash_stable(*r);
        }
        let ev = &mut self.seal_evidence;
        ev.dsr = hash_stable(ev.dsr);
        ev.pbo = hash_stable(ev.pbo);
        ev.spa_pvalue = hash_stable(ev.spa_pvalue);
        ev.realised_turnover = hash_stable(ev.realised_turnover);
        ev.capacity_usd = hash_stable(ev.capacity_usd);
        for opt in [
            &mut ev.cost_stress_net_min,
            &mut ev.uncensored_pbo,
            &mut ev.ic,
            &mut ev.fdr,
        ] {
            if let Some(v) = opt.as_mut() {
                *v = hash_stable(*v);
            }
        }
        if let Some(cpcv) = ev.cpcv.as_mut() {
            cpcv.median_sharpe = hash_stable(cpcv.median_sharpe);
            cpcv.sharpe_iqr_lo = hash_stable(cpcv.sharpe_iqr_lo);
            cpcv.sharpe_iqr_hi = hash_stable(cpcv.sharpe_iqr_hi);
            cpcv.sharpe_p05 = hash_stable(cpcv.sharpe_p05);
            cpcv.sharpe_p95 = hash_stable(cpcv.sharpe_p95);
            cpcv.median_dsr = hash_stable(cpcv.median_dsr);
            cpcv.dsr_p05 = hash_stable(cpcv.dsr_p05);
            cpcv.frac_dsr_ge_floor = hash_stable(cpcv.frac_dsr_ge_floor);
            for x in &mut cpcv.path_sharpes {
                *x = hash_stable(*x);
            }
            for x in &mut cpcv.path_dsrs {
                *x = hash_stable(*x);
            }
        }
        // QE-476: canonicalise the sealed verdict's value/threshold (QE-482 hash-stable precision).
        if let Some(verdict) = ev.promotion.as_mut() {
            for c in &mut verdict.criteria {
                c.value = hash_stable(c.value);
                c.threshold = hash_stable(c.threshold);
            }
        }
    }

    /// Validate the artefact's structural invariants — `weights` aligned one-to-one with `chromosomes`
    /// and every weight finite, and `worst_case_loss` (if present) a finite non-negative fraction.
    /// Called by [`Vintage::seal`], so a silent upstream bug (a non-finite weight that would serialise
    /// to JSON `null` and fail re-load, a weight/chromosome length mismatch, or a nonsensical loss
    /// figure) surfaces as a clear error at seal time rather than a corrupt artefact.
    ///
    /// # Errors
    /// [`VintageError::WeightChromosomeMismatch`], [`VintageError::NonFiniteWeight`], or
    /// [`VintageError::InvalidWorstCaseLoss`].
    pub fn validate(&self) -> Result<(), VintageError> {
        if self.weights.len() != self.chromosomes.len() {
            return Err(VintageError::WeightChromosomeMismatch {
                weights: self.weights.len(),
                chromosomes: self.chromosomes.len(),
            });
        }
        for (index, &value) in self.weights.iter().enumerate() {
            if !value.is_finite() {
                return Err(VintageError::NonFiniteWeight { index, value });
            }
        }
        if let Some(loss) = self.worst_case_loss {
            if !loss.is_finite() || loss < 0.0 {
                return Err(VintageError::InvalidWorstCaseLoss { value: loss });
            }
        }
        // QE-467: a non-finite holdout return would serialise to JSON `null` and fail re-load — caught at
        // seal time, like the weights, so a corrupt series never reaches the leaderboard/inspector.
        for (index, &value) in self.holdout_series.returns.iter().enumerate() {
            if !value.is_finite() {
                return Err(VintageError::NonFiniteHoldoutReturn { index, value });
            }
        }
        // QE-467: the persisted seal-evidence figures must be finite (same round-trip reason). The
        // `Option` slots are checked only when populated.
        let ev = &self.seal_evidence;
        let mut evidence_fields = vec![
            ("dsr", ev.dsr),
            ("pbo", ev.pbo),
            ("spa_pvalue", ev.spa_pvalue),
            ("realised_turnover", ev.realised_turnover),
            ("capacity_usd", ev.capacity_usd),
        ];
        for (name, opt) in [
            ("cost_stress_net_min", ev.cost_stress_net_min),
            ("uncensored_pbo", ev.uncensored_pbo),
            ("ic", ev.ic),
            ("fdr", ev.fdr),
        ] {
            if let Some(v) = opt {
                evidence_fields.push((name, v));
            }
        }
        for (field, value) in evidence_fields {
            if !value.is_finite() {
                return Err(VintageError::NonFiniteEvidence { field, value });
            }
        }
        // QE-469: the CPCV distribution summary (when populated) must be finite in every figure and every
        // per-path Sharpe/DSR — same round-trip reason (a non-finite value serialises to JSON `null`).
        if let Some(cpcv) = &ev.cpcv {
            let mut cpcv_fields: Vec<(&'static str, f64)> = cpcv.summary_fields().to_vec();
            for &v in &cpcv.path_sharpes {
                cpcv_fields.push(("cpcv.path_sharpes", v));
            }
            for &v in &cpcv.path_dsrs {
                cpcv_fields.push(("cpcv.path_dsrs", v));
            }
            for (field, value) in cpcv_fields {
                if !value.is_finite() {
                    return Err(VintageError::NonFiniteEvidence { field, value });
                }
            }
        }
        // QE-476: the sealed G1 verdict's per-criterion value/threshold must be finite (same round-trip
        // reason — a non-finite value would serialise to JSON `null` and fail re-load).
        if let Some(verdict) = &ev.promotion {
            for c in &verdict.criteria {
                for (field, value) in [
                    ("promotion.value", c.value),
                    ("promotion.threshold", c.threshold),
                ] {
                    if !value.is_finite() {
                        return Err(VintageError::NonFiniteEvidence { field, value });
                    }
                }
            }
        }
        Ok(())
    }

    /// Lowercase-hex SHA-256 over the record's canonical JSON — the **content hash** (same pattern as
    /// [`Lineage::id`]). Stable because every embedded type serialises deterministically (fixed field
    /// order; `BTreeMap`-ordered calibration maps; no `HashMap`/`HashSet` anywhere in the embedded types).
    ///
    /// **Hashing contract:** the hash is the digest of `serde_json`'s output. Its stability therefore
    /// depends on (a) no map type with nondeterministic iteration order ever entering the hashed content,
    /// and (b) `serde_json`'s number/whitespace formatting. Any future field addition must preserve (a);
    /// a `serde_json` major bump that changed (b) would change every vintage hash (and so must bump
    /// [`VINTAGE_FORMAT_VERSION`]).
    ///
    /// # Errors
    /// [`VintageError::Serialize`] if the content cannot be serialised.
    pub fn content_hash(&self) -> Result<String, VintageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| VintageError::Serialize(e.to_string()))?;
        Ok(hex(&Sha256::digest(&bytes)))
    }
}

/// A sealed vintage artefact: its [`VintageContent`] plus the content hash that pins it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vintage {
    /// The hashed content.
    pub content: VintageContent,
    /// The content hash computed at [`seal`](Vintage::seal) time.
    pub content_hash: String,
}

impl Vintage {
    /// Seal `content` by [validating](VintageContent::validate) its invariants, then computing and
    /// pinning its content hash.
    ///
    /// # Errors
    /// [`VintageContent::validate`] errors (non-finite or misaligned weights), or a serialisation
    /// failure from [`VintageContent::content_hash`].
    pub fn seal(mut content: VintageContent) -> Result<Self, VintageError> {
        // QE-482: canonicalise every hashed f64 to the hash-stable precision at the type boundary, BEFORE
        // validating and hashing, so the content-addressed reproducibility guarantee no longer depends on
        // the producer having rounded every field. Idempotent on already-rounded input (no drift).
        content.canonicalize();
        content.validate()?;
        let content_hash = content.content_hash()?;
        Ok(Vintage {
            content,
            content_hash,
        })
    }

    /// Verify the stored hash matches a freshly recomputed one — detects any post-seal tampering.
    ///
    /// # Errors
    /// [`VintageError::HashMismatch`] if the stored hash does not match, or a serialisation failure.
    pub fn verify(&self) -> Result<(), VintageError> {
        let recomputed = self.content.content_hash()?;
        if recomputed != self.content_hash {
            return Err(VintageError::HashMismatch {
                stored: self.content_hash.clone(),
                recomputed,
            });
        }
        Ok(())
    }

    /// Serialise the sealed artefact as JSON to `w`.
    ///
    /// # Errors
    /// [`VintageError::Serialize`] / [`VintageError::Io`] on failure.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), VintageError> {
        let bytes = serde_json::to_vec(self).map_err(|e| VintageError::Serialize(e.to_string()))?;
        w.write_all(&bytes)?;
        Ok(())
    }

    /// Load a sealed artefact from a JSON reader, **verifying the content hash** before returning — a
    /// load never yields an unverified vintage.
    ///
    /// # Errors
    /// [`VintageError::Deserialize`] / [`VintageError::Io`] on read failure, [`VintageError::HashMismatch`]
    /// if the content hash does not verify.
    pub fn load<R: Read>(r: R) -> Result<Self, VintageError> {
        let vintage: Vintage =
            serde_json::from_reader(r).map_err(|e| VintageError::Deserialize(e.to_string()))?;
        vintage.verify()?;
        Ok(vintage)
    }
}

/// A directory-backed store of vintages (the ensemble/vintage repository, QE-129/D3): one
/// `<root>/<vintage_id>.json` per vintage. Runtime (QE-219) opens it read-only.
#[derive(Debug, Clone)]
pub struct VintageRepository {
    root: PathBuf,
}

impl VintageRepository {
    /// A repository rooted at `root` (created on first [`write`](VintageRepository::write)).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        VintageRepository { root: root.into() }
    }

    /// The on-disk path for `vintage_id`.
    #[must_use]
    pub fn path_for(&self, vintage_id: &str) -> PathBuf {
        self.root.join(format!("{vintage_id}.json"))
    }

    /// Write `vintage` to `<root>/<vintage_id>.json`, creating `root` if needed. Returns the path.
    ///
    /// # Errors
    /// [`VintageError::Io`] / [`VintageError::Serialize`] on failure.
    pub fn write(&self, vintage: &Vintage) -> Result<PathBuf, VintageError> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_for(&vintage.content.vintage_id);
        let mut file = std::fs::File::create(&path)?;
        vintage.write(&mut file)?;
        Ok(path)
    }

    /// Load and verify the vintage `vintage_id` from disk, then assert its persisted schema identity
    /// matches this build **exactly** ([`schema::assert_schema`], QE-402) — the fail-closed
    /// catalogue↔vintage / genome-rep boundary shared by the CLI backtest and the live runtime. A
    /// vintage sealed against a different (reordered / version-bumped) catalogue is rejected here rather
    /// than silently re-addressing its clauses.
    ///
    /// # Errors
    /// [`VintageError::Io`] if the file is missing/unreadable, plus the [`Vintage::load`] errors, plus
    /// [`VintageError::SchemaMismatch`] / [`VintageError::GenomeRepMismatch`] on an identity mismatch.
    pub fn load(&self, vintage_id: &str) -> Result<Vintage, VintageError> {
        let file = std::fs::File::open(self.path_for(vintage_id))?;
        let vintage = Vintage::load(file)?;
        schema::assert_schema(&vintage.content)?;
        Ok(vintage)
    }

    /// List every sealed vintage under `root`, **ascending by `vintage_id`** (deterministic order).
    ///
    /// Each `*.json` file is loaded through [`Vintage::load`] (so the content hash is verified). Files
    /// that don't parse/verify as a vintage are **skipped** — the artifacts dir may hold unrelated
    /// files — so a stray file never fails the whole listing. A missing `root` yields an empty list
    /// (nothing has been sealed yet), not an error.
    ///
    /// # Errors
    /// [`VintageError::Io`] on a filesystem error reading the directory (other than "not found").
    pub fn list(&self) -> Result<Vec<Vintage>, VintageError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(VintageError::Io(e)),
        };
        let mut vintages = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Skip anything that doesn't open + verify as a vintage (unrelated artefact / corrupt file).
            if let Ok(file) = std::fs::File::open(&path) {
                if let Ok(vintage) = Vintage::load(file) {
                    vintages.push(vintage);
                }
            }
        }
        vintages.sort_by(|a, b| a.content.vintage_id.cmp(&b.content.vintage_id));
        Ok(vintages)
    }
}

/// Errors raised while sealing / writing / loading a vintage.
#[derive(Debug, Error)]
pub enum VintageError {
    /// The artefact could not be serialised.
    #[error("failed to serialise vintage: {0}")]
    Serialize(String),
    /// The artefact could not be deserialised.
    #[error("failed to deserialise vintage: {0}")]
    Deserialize(String),
    /// The content hash did not verify (tampered or corrupted artefact).
    #[error("vintage content hash mismatch: stored {stored}, recomputed {recomputed}")]
    HashMismatch {
        /// The hash stored in the artefact.
        stored: String,
        /// The hash recomputed from the content.
        recomputed: String,
    },
    /// `weights` is not aligned one-to-one with `chromosomes`.
    #[error("vintage has {weights} weights for {chromosomes} chromosomes (must be aligned)")]
    WeightChromosomeMismatch {
        /// Number of weights supplied.
        weights: usize,
        /// Number of chromosomes supplied.
        chromosomes: usize,
    },
    /// A weight is not finite (would serialise to JSON `null` and fail re-load).
    #[error("vintage weight {index} is not finite: {value}")]
    NonFiniteWeight {
        /// Index of the offending weight.
        index: usize,
        /// The non-finite value.
        value: f64,
    },
    /// `worst_case_loss` is not a finite, non-negative fraction (QE-130).
    #[error("vintage worst_case_loss must be a finite non-negative fraction, got {value}")]
    InvalidWorstCaseLoss {
        /// The offending value.
        value: f64,
    },
    /// A holdout-series return is not finite (QE-467) — would serialise to JSON `null` and fail re-load.
    #[error("vintage holdout return {index} is not finite: {value}")]
    NonFiniteHoldoutReturn {
        /// Index of the offending return.
        index: usize,
        /// The non-finite value.
        value: f64,
    },
    /// A persisted seal-evidence figure is not finite (QE-467).
    #[error("vintage seal evidence `{field}` is not finite: {value}")]
    NonFiniteEvidence {
        /// The offending evidence field name.
        field: &'static str,
        /// The non-finite value.
        value: f64,
    },
    /// The persisted catalogue identity does not match this build's catalogue **exactly** (QE-402): a
    /// catalogue reorder or a same-width `CATALOGUE_VERSION` bump. Loading is refused — the sealed
    /// genomes would silently address different indicators.
    #[error(
        "catalogue schema mismatch: vintage was sealed against catalogue {found:?}, but this build is \
         {expected:?} — a reorder or version bump makes every clause index unsafe"
    )]
    SchemaMismatch {
        /// This build's current catalogue identity.
        expected: CatalogueIdentity,
        /// The identity the vintage was sealed against.
        found: CatalogueIdentity,
    },
    /// A persisted chromosome's representation version does not match this build's `REP_VERSION`
    /// (QE-402, vintage↔genome-rep boundary).
    #[error(
        "genome representation mismatch: chromosome #{index} is rep version {found}, this build is \
         {expected}"
    )]
    GenomeRepMismatch {
        /// The offending chromosome index.
        index: usize,
        /// This build's genome representation version.
        expected: u16,
        /// The version stored in the chromosome.
        found: u16,
    },
    /// Underlying I/O error.
    #[error("vintage I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Lowercase-hex encoding of a byte slice.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_risk::{CalibrationProfile, Fraction};
    use qe_signal::{
        Clause, ExitParams, Genome, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
    };
    use rust_decimal::Decimal;

    fn genome(hold: u16) -> Genome {
        let off = Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        };
        let mut clauses = [off; CLAUSES_PER_SET];
        clauses[0] = Clause {
            enabled: true,
            feature: 0,
            lo: 1,
            hi: 2,
        };
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses,
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [off; CLAUSES_PER_SET],
                min_satisfied: 1,
            },
            exit: ExitParams {
                max_holding_bars: hold,
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    fn calibration() -> CalibrationProfile {
        CalibrationProfile::new(Fraction::new(Decimal::new(2, 1)).unwrap()) // 0.2 ensemble fast-drop
    }

    fn lineage() -> Lineage {
        Lineage::new(
            "cfg-hash-abc",
            "snapshot-2024-06",
            "commit-deadbeef",
            vec![7, 42],
        )
    }

    fn content() -> VintageContent {
        VintageContent {
            format_version: VINTAGE_FORMAT_VERSION,
            vintage_id: "2024-06-vintage".to_string(),
            chromosomes: vec![genome(10), genome(25)],
            weights: vec![0.6, 0.4],
            calibration: calibration(),
            slippage: SlippageCalibration::default(),
            sizer: PortfolioSizer::default(),
            shocks: ShockConfig::default(),
            worst_case_loss: Some(0.28), // QE-130 stress figure
            catalogue: CatalogueIdentity::current(), // QE-402 pinned identity
            lineage: lineage(),
            seal_evidence: SealEvidence {
                dsr: 0.8,
                pbo: 0.1,
                spa_pvalue: 0.02,
                n_trials: 64,
                realised_turnover: 0.5,
                capacity_usd: 1_500_000.0,
                cost_stress_net_min: Some(0.12),
                ..SealEvidence::default()
            },
            holdout_series: HoldoutReturnSeries {
                returns: vec![0.01, -0.02, 0.03],
            },
            provenance: ResearchProvenance::default(),
        }
    }

    #[test]
    fn round_trips_with_stable_verifiable_hash() {
        let sealed = Vintage::seal(content()).unwrap();

        // Write → load reproduces the vintage exactly, and the load verifies the hash.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded, sealed);
        assert_eq!(loaded.content_hash, sealed.content_hash);

        // The hash is stable: sealing the same content again yields the same hash.
        let resealed = Vintage::seal(content()).unwrap();
        assert_eq!(resealed.content_hash, sealed.content_hash);
        // … and it is non-empty hex (a real SHA-256).
        assert_eq!(sealed.content_hash.len(), 64);
        assert!(sealed.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tampering_with_content_fails_verification() {
        let mut sealed = Vintage::seal(content()).unwrap();
        // Mutate the content without re-sealing — the stored hash no longer matches.
        sealed.content.weights[0] = 0.99;
        let err = sealed.verify().unwrap_err();
        assert!(matches!(err, VintageError::HashMismatch { .. }));

        // And a load of the tampered bytes is rejected.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        assert!(matches!(
            Vintage::load(buf.as_slice()),
            Err(VintageError::HashMismatch { .. })
        ));
    }

    #[test]
    fn vintage_carries_worst_case_loss_and_rejects_an_invalid_one() {
        // The QE-130 worst-case-loss figure round-trips with the vintage (and is in the hash).
        let sealed = Vintage::seal(content()).unwrap();
        assert_eq!(sealed.content.worst_case_loss, Some(0.28));
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.worst_case_loss, Some(0.28));

        // A different figure changes the hash (it is part of the hashed content).
        let mut other = content();
        other.worst_case_loss = Some(0.40);
        assert_ne!(
            Vintage::seal(other).unwrap().content_hash,
            sealed.content_hash
        );

        // A negative or non-finite loss is rejected at seal time.
        let mut negative = content();
        negative.worst_case_loss = Some(-0.1);
        assert!(matches!(
            Vintage::seal(negative),
            Err(VintageError::InvalidWorstCaseLoss { .. })
        ));
    }

    #[test]
    fn sizer_is_part_of_the_hash() {
        // QE-433: the advisory portfolio-Kelly sizer rides the hashed content, so a different multiplier
        // yields a different vintage id.
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.sizer = PortfolioSizer::new(rust_decimal::Decimal::new(35, 2)); // 0.35 vs default 1.0
        let sized = Vintage::seal(other).unwrap();
        assert_ne!(sized.content_hash, base.content_hash);

        // And it round-trips through disk verify.
        let mut buf: Vec<u8> = Vec::new();
        sized.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.sizer, sized.content.sizer);
    }

    #[test]
    fn shocks_are_part_of_the_hash() {
        // QE-441: the frozen bar-level scenario-shock set rides the hashed content, so a different shock
        // set (e.g. a heavier gap) yields a different vintage id — the shocks that shaped `size_bps` are
        // pinned into the reproducible lineage (content-addressed / frozen-per-vintage).
        assert_eq!(
            VINTAGE_FORMAT_VERSION, 8,
            "QE-467 bumped the format version to 8 (seal evidence + holdout series + provenance)"
        );
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.shocks = ShockConfig::new(
            other.shocks.seed,
            other.shocks.frequency_per_million,
            rust_decimal::Decimal::new(20, 2), // 0.20 gap vs default 0.10
            other.shocks.funding_per_period,
            other.shocks.funding_periods,
            other.shocks.adl_haircut,
        );
        let shocked = Vintage::seal(other).unwrap();
        assert_ne!(shocked.content_hash, base.content_hash);

        // And it round-trips through disk verify.
        let mut buf: Vec<u8> = Vec::new();
        shocked.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.shocks, shocked.content.shocks);
    }

    #[test]
    fn seal_evidence_is_part_of_the_hash_and_round_trips() {
        // QE-467: the persisted seal evidence rides the hashed content, so a different DSR (or any figure)
        // yields a different vintage id — downstream reads it, so it must be pinned into the lineage.
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.seal_evidence.dsr = 1.9; // vs 0.8
        assert_ne!(
            Vintage::seal(other).unwrap().content_hash,
            base.content_hash
        );

        // A different capacity_usd also moves the id.
        let mut cap = content();
        cap.seal_evidence.capacity_usd = 42.0;
        assert_ne!(Vintage::seal(cap).unwrap().content_hash, base.content_hash);

        // And the whole block round-trips through disk verify.
        let mut buf: Vec<u8> = Vec::new();
        base.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.seal_evidence, base.content.seal_evidence);
        assert_eq!(loaded.content.seal_evidence.cost_stress_net_min, Some(0.12));

        // A non-finite evidence figure is rejected at seal time.
        let mut bad = content();
        bad.seal_evidence.capacity_usd = f64::INFINITY;
        assert!(matches!(
            Vintage::seal(bad),
            Err(VintageError::NonFiniteEvidence {
                field: "capacity_usd",
                ..
            })
        ));
    }

    #[test]
    fn cpcv_summary_is_part_of_the_hash_and_round_trips() {
        // QE-469: the CPCV OOS distribution summary rides the hashed content, so populating it moves the
        // vintage id — downstream reads it, so it must be pinned into the lineage (content-addressed).
        let base = Vintage::seal(content()).unwrap();
        let summary = CpcvSummary {
            blocks: 6,
            n_paths: 20,
            median_sharpe: 0.12,
            sharpe_iqr_lo: 0.08,
            sharpe_iqr_hi: 0.16,
            sharpe_p05: 0.03,
            sharpe_p95: 0.20,
            median_dsr: 0.97,
            dsr_p05: 0.91,
            frac_dsr_ge_floor: 0.85,
            path_sharpes: vec![0.10, 0.12, 0.14],
            path_dsrs: vec![0.96, 0.97, 0.98],
        };
        let mut withc = content();
        withc.seal_evidence.cpcv = Some(summary.clone());
        let sealed = Vintage::seal(withc).unwrap();
        assert_ne!(
            sealed.content_hash, base.content_hash,
            "populating cpcv must change the vintage id"
        );

        // The whole block round-trips through disk verify.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.seal_evidence.cpcv, Some(summary));

        // A non-finite CPCV figure (here in a per-path Sharpe) is rejected at seal time.
        let mut bad = content();
        let mut bad_summary = loaded.content.seal_evidence.cpcv.clone().unwrap();
        bad_summary.path_sharpes[1] = f64::NAN;
        bad.seal_evidence.cpcv = Some(bad_summary);
        assert!(matches!(
            Vintage::seal(bad),
            Err(VintageError::NonFiniteEvidence {
                field: "cpcv.path_sharpes",
                ..
            })
        ));
    }

    #[test]
    fn default_cpcv_is_absent_and_keeps_the_pre_qe469_bytes() {
        // The golden-safety guarantee: an unset `cpcv` (the default) is OMITTED from the serialised content
        // via skip_serializing_if, so a `SealEvidence`-default vintage is byte-identical to pre-QE-469 —
        // no VINTAGE_FORMAT_VERSION bump, no golden move.
        assert_eq!(
            VINTAGE_FORMAT_VERSION, 8,
            "QE-469 rides version 8 (no bump)"
        );
        let sealed = Vintage::seal(content()).unwrap();
        assert!(sealed.content.seal_evidence.cpcv.is_none());
        let json = serde_json::to_string(&sealed.content).unwrap();
        assert!(
            !json.contains("cpcv"),
            "an absent cpcv must not appear in the serialised content: {json}"
        );
    }

    fn verdict(promoted: bool) -> PromotionVerdict {
        PromotionVerdict {
            promoted,
            criteria: vec![
                SealedCriterion {
                    name: "dsr_exceeds_threshold".to_string(),
                    passed: promoted,
                    value: if promoted { 0.98 } else { 0.80 },
                    threshold: 0.95, // the frozen DSR threshold
                },
                SealedCriterion {
                    name: "pbo_below_overfit_threshold".to_string(),
                    passed: true,
                    value: 0.10,
                    threshold: 0.5,
                },
            ],
        }
    }

    #[test]
    fn sealed_verdict_recovers_promoted_and_thresholds_without_the_run_doc() {
        // QE-476: a downstream reader recovers the promotion verdict + each criterion's frozen threshold
        // from the sealed (content-addressed) artifact alone — no separate run doc needed.
        let mut c = content();
        c.seal_evidence.promotion = Some(verdict(true));
        let sealed = Vintage::seal(c).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();

        assert!(loaded.content.seal_evidence.is_promoted());
        let v = loaded.content.seal_evidence.promotion.as_ref().unwrap();
        assert!(v.promoted);
        let dsr_crit = v
            .criteria
            .iter()
            .find(|c| c.name == "dsr_exceeds_threshold")
            .expect("the DSR criterion is sealed");
        assert_eq!(
            dsr_crit.threshold, 0.95,
            "the threshold is frozen in the hash"
        );
        assert!(dsr_crit.passed);
    }

    #[test]
    fn a_drifted_threshold_constant_does_not_change_a_sealed_verdict() {
        // QE-476: the recorded verdict is READ FROM THE HASH, not re-derived — so even if a threshold
        // CONSTANT drifts in a later build, an already-sealed artifact's frozen threshold/value is
        // unchanged (its content hash still verifies against the figures it was judged on).
        let mut c = content();
        c.seal_evidence.promotion = Some(verdict(true));
        let sealed = Vintage::seal(c).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        // Reload verifies the content hash: the sealed threshold cannot be silently re-derived to a drifted
        // constant without breaking the hash. The frozen value is exactly what was sealed.
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        let dsr_crit = loaded
            .content
            .seal_evidence
            .promotion
            .as_ref()
            .unwrap()
            .criteria
            .iter()
            .find(|c| c.name == "dsr_exceeds_threshold")
            .unwrap();
        assert_eq!(dsr_crit.threshold, 0.95);
        assert_eq!(dsr_crit.value, 0.98);
        loaded.verify().unwrap();
    }

    #[test]
    fn a_failed_candidate_is_written_but_marked_non_promotable() {
        // QE-476 write-but-mark: a G1-FAILED candidate is NOT un-writable — it seals, writes, and reloads,
        // but carries promoted = false so a selector's fail-closed read refuses it.
        let mut c = content();
        c.seal_evidence.promotion = Some(verdict(false));
        let sealed = Vintage::seal(c).unwrap();
        assert!(!sealed.content.seal_evidence.is_promoted());

        let dir = std::env::temp_dir().join(format!("qe-vintage-failed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = VintageRepository::new(&dir);
        repo.write(&sealed).unwrap(); // written, not refused
        let loaded = repo.load(&sealed.content.vintage_id).unwrap();
        assert!(
            !loaded.content.seal_evidence.is_promoted(),
            "a non-promoted vintage must read as not-promoted (a selector refuses it)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verdict_is_part_of_the_hash_and_absent_is_byte_identical() {
        // Flipping the promoted flag (or any criterion figure) moves the vintage id — the verdict is
        // content-addressed. And an absent verdict (default None) is OMITTED from the serialised content
        // (skip_serializing_if), so a verdict-less vintage is byte-identical to pre-QE-476 (no bump).
        let base = Vintage::seal(content()).unwrap();
        assert!(base.content.seal_evidence.promotion.is_none());
        assert!(
            !base.content.seal_evidence.is_promoted(),
            "None ⇒ fail-closed"
        );
        let json = serde_json::to_string(&base.content).unwrap();
        assert!(
            !json.contains("promotion"),
            "an absent verdict must not appear in the serialised content: {json}"
        );

        let mut promoted = content();
        promoted.seal_evidence.promotion = Some(verdict(true));
        let mut failed = content();
        failed.seal_evidence.promotion = Some(verdict(false));
        assert_ne!(
            Vintage::seal(promoted).unwrap().content_hash,
            Vintage::seal(failed).unwrap().content_hash,
            "the promotion verdict is part of the hashed content"
        );
    }

    #[test]
    fn holdout_series_is_part_of_the_hash_and_addressable() {
        // QE-467: the canonical net-of-cost holdout series (on deployed weights) rides the hashed content,
        // so a different series yields a different vintage id.
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.holdout_series.returns = vec![0.05, 0.05, 0.05];
        let changed = Vintage::seal(other).unwrap();
        assert_ne!(changed.content_hash, base.content_hash);

        // The handle is a stable 64-hex ref (what the detail endpoint returns instead of a re-run), and it
        // is sensitive to the series contents.
        let handle = base.content.holdout_series.handle().unwrap();
        assert_eq!(handle.len(), 64);
        assert!(handle.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(handle, base.content.holdout_series.handle().unwrap());
        assert_ne!(handle, changed.content.holdout_series.handle().unwrap());

        // It round-trips through disk verify.
        let mut buf: Vec<u8> = Vec::new();
        base.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.holdout_series, base.content.holdout_series);

        // A non-finite holdout return is rejected at seal time.
        let mut bad = content();
        bad.holdout_series.returns = vec![0.01, f64::NAN];
        assert!(matches!(
            Vintage::seal(bad),
            Err(VintageError::NonFiniteHoldoutReturn { index: 1, .. })
        ));
    }

    #[test]
    fn provenance_is_part_of_the_hash_and_downstream_fields_round_trip() {
        // QE-467: flipping data_provenance real→synthetic changes the vintage id — a synthetic-derived
        // vintage is no longer indistinguishable from a real one.
        let base = Vintage::seal(content()).unwrap();
        let mut synth = content();
        synth.provenance.data_provenance = DataProvenance::Synthetic;
        assert_ne!(
            Vintage::seal(synth).unwrap().content_hash,
            base.content_hash
        );

        // The deferred fields (schema owned here, populated downstream) can be written and round-trip —
        // proving QE-458/QE-460 can populate them under THIS bump without another version change.
        let mut populated = content();
        populated.provenance.holdout_split = HoldoutSplit {
            holdout_range: Some(TimeRange {
                start: "2021-06-01".to_string(),
                end: "2021-07-01".to_string(),
            }),
            train_range: Some(TimeRange {
                start: "2020-01-01".to_string(),
                end: "2021-05-01".to_string(),
            }),
            embargo_bars: 24,
        };
        populated.provenance.regime_composition = vec![
            RegimeShare {
                regime: "trend".to_string(),
                bars: 300,
            },
            RegimeShare {
                regime: "chop".to_string(),
                bars: 120,
            },
        ];
        populated.provenance.consultation_count = 3;
        populated.provenance.steer_delta = Some(SteerDelta {
            indicator_subset_hash: "a".repeat(64),
            generations: 40,
            population: 12,
            windows: 6,
            folds: 4,
        });
        let sealed = Vintage::seal(populated).unwrap();
        assert_ne!(sealed.content_hash, base.content_hash);

        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        let loaded = Vintage::load(buf.as_slice()).unwrap();
        assert_eq!(loaded.content.provenance, sealed.content.provenance);
        assert_eq!(loaded.content.provenance.consultation_count, 3);
    }

    #[test]
    fn seal_is_invariant_to_sub_precision_noise() {
        // QE-482 AC1 (idempotence): seal canonicalises every hashed f64 to the hash-stable precision, so
        // sub-1e-12 noise (a producer that forgot to round, or a differently-rounded f64) does NOT change
        // the vintage id — the rounding invariant lives at the type boundary, not only in the producer.
        let clean = Vintage::seal(content()).unwrap();
        let mut noisy = content();
        noisy.weights[0] += 1e-15;
        noisy.weights[1] -= 3e-15;
        noisy.seal_evidence.dsr += 2e-15;
        noisy.holdout_series.returns[0] += 1e-14;
        if let Some(l) = noisy.worst_case_loss.as_mut() {
            *l += 1e-15;
        }
        let sealed = Vintage::seal(noisy).unwrap();
        assert_eq!(
            sealed.content_hash, clean.content_hash,
            "sub-precision noise must not move the vintage id"
        );
        // The stored content is the canonicalised content and round-trips + verifies from disk.
        let mut buf: Vec<u8> = Vec::new();
        sealed.write(&mut buf).unwrap();
        Vintage::load(buf.as_slice()).unwrap();
    }

    #[test]
    fn seal_canonicalises_a_non_hash_stable_field_to_its_rounded_twin() {
        // QE-482 AC2 (canonicalise posture, proven): a VintageContent carrying a non-hash-stable f64 in the
        // CPCV block seals to the SAME hash as its explicitly pre-rounded twin — seal is the single point of
        // truth for hash-stable precision.
        let raw = 0.123_456_789_012_999_f64; // a tail below 1e-12
        let rounded = (raw * HASH_STABLE_SCALE).round() / HASH_STABLE_SCALE;
        assert_ne!(
            raw, rounded,
            "the fixture must actually carry sub-precision noise"
        );

        let summary = |sharpe: f64| CpcvSummary {
            blocks: 6,
            n_paths: 20,
            median_sharpe: sharpe,
            path_sharpes: vec![sharpe],
            path_dsrs: vec![0.97],
            ..CpcvSummary::default()
        };
        let mut noisy = content();
        noisy.seal_evidence.cpcv = Some(summary(raw));
        let mut twin = content();
        twin.seal_evidence.cpcv = Some(summary(rounded));

        assert_eq!(
            Vintage::seal(noisy).unwrap().content_hash,
            Vintage::seal(twin).unwrap().content_hash,
            "a non-hash-stable f64 must canonicalise to its rounded twin's hash"
        );
    }

    #[test]
    fn seal_rejects_non_finite_and_misaligned_weights() {
        // A non-finite weight would serialise to JSON `null` and fail re-load — caught at seal time.
        let mut bad = content();
        bad.weights[1] = f64::NAN;
        assert!(matches!(
            Vintage::seal(bad),
            Err(VintageError::NonFiniteWeight { index: 1, .. })
        ));

        // Weights must be aligned one-to-one with chromosomes.
        let mut misaligned = content();
        misaligned.weights.pop(); // 1 weight for 2 chromosomes
        assert!(matches!(
            Vintage::seal(misaligned),
            Err(VintageError::WeightChromosomeMismatch {
                weights: 1,
                chromosomes: 2,
            })
        ));
    }

    #[test]
    fn format_version_is_part_of_the_hash() {
        let base = Vintage::seal(content()).unwrap();
        let mut other = content();
        other.format_version = VINTAGE_FORMAT_VERSION + 1;
        let bumped = Vintage::seal(other).unwrap();
        assert_ne!(bumped.content_hash, base.content_hash);
    }

    #[test]
    fn repository_lists_sealed_vintages_sorted_skipping_strays() {
        let dir = std::env::temp_dir().join(format!("qe-vintage-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = VintageRepository::new(&dir);

        // A missing dir lists as empty (nothing sealed yet).
        assert!(repo.list().unwrap().is_empty());

        // Seal two vintages with distinct ids and write them (out of alphabetical order).
        let mut c2 = content();
        c2.vintage_id = "zzz-late".to_string();
        let mut c1 = content();
        c1.vintage_id = "aaa-early".to_string();
        repo.write(&Vintage::seal(c2).unwrap()).unwrap();
        repo.write(&Vintage::seal(c1).unwrap()).unwrap();

        // A stray non-vintage `.json` and a non-json file are both ignored.
        std::fs::write(dir.join("not-a-vintage.json"), b"{\"nope\":true}").unwrap();
        std::fs::write(dir.join("README.txt"), b"ignore me").unwrap();

        let listed = repo.list().unwrap();
        let ids: Vec<&str> = listed
            .iter()
            .map(|v| v.content.vintage_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["aaa-early", "zzz-late"],
            "ascending by id, strays skipped"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repository_round_trips_from_disk() {
        let dir = std::env::temp_dir().join(format!("qe-vintage-test-{}", std::process::id()));
        let repo = VintageRepository::new(&dir);
        let sealed = Vintage::seal(content()).unwrap();

        let path = repo.write(&sealed).unwrap();
        assert!(path.exists());
        let loaded = repo.load(&sealed.content.vintage_id).unwrap();
        assert_eq!(loaded, sealed);

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
