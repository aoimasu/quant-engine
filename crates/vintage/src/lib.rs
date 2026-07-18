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
pub const VINTAGE_FORMAT_VERSION: u16 = 8;

/// The persisted **seal evidence** (QE-467): the gate's own tradability + deflation outputs, carried into
/// the sealed artefact so every downstream surface (inspector QE-456/457, leaderboard QE-466, flow
/// QE-460) **reads** — never recomputes — them. Part of the hashed content (content-addressed).
///
/// The DSR/PBO/SPA + turnover + `capacity_usd` are the ensemble train gate's own outputs and are
/// populated on the real seal path. The `Option` fields are schema slots defined here and populated by
/// the path that actually computes them: `uncensored_pbo`/`ic`/`fdr` are GP/IC-screen concerns (absent on
/// the normal train path, exactly like `GateSnapshot::uncensored_pbo`), and `cost_stress_net_min` is the
/// deployed ensemble's `min{1×,2×}` cost-stressed net (design §4.6a).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
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
    pub fn seal(content: VintageContent) -> Result<Self, VintageError> {
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
