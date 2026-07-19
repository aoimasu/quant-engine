//! QE-257 read APIs (spec §6.2), mounted under the **session-gated** `/api` subtree:
//! `GET /api/vintages` and `GET /api/market-data/coverage`.
//!
//! Both are registered inside [`crate::auth::protected_routes`], so they inherit the QE-256
//! `require_session` gate (no session ⇒ `401`) without any per-handler auth code. The handlers run
//! the blocking LMDB / filesystem work inside [`tokio::task::spawn_blocking`], keeping async confined
//! and non-blocking (mirrors the QE-256 verifier pattern).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{http::StatusCode, Json, Router};
use qe_ensemble::{pairwise_corr_penalty, CorrDeflation, DEFAULT_SIGNIFICANCE_Z};
use qe_risk::{CalibrationProfile, PortfolioSizer, SlippageCalibration};
use qe_signal::{CatalogueConfig, CatalogueIdentity, FeatureSchema};
use qe_vintage::{
    DataProvenance, HoldoutSplit, RegimeShare, SealEvidence, SteerDelta, Vintage, VintageError,
    VintageRepository,
};
use serde::Serialize;
use serde_json::json;

use crate::runs::store::RunStore;
use crate::runs::RunManager;
use crate::ReadState;

/// The QE-257 read routes. Parameterised over [`crate::AppState`]; the handlers extract
/// `State<Arc<ReadState>>` (and, for the QE-456 detail endpoint, `State<Arc<RunManager>>`),
/// projected out of `AppState` via `FromRef`.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/vintages", get(list_vintages))
        // QE-466: the read-only leaderboard/comparison. Registered as a static segment BEFORE `/vintages/{id}`
        // — axum/matchit routes the literal `leaderboard` ahead of the `{id}` param, so it never collides
        // with a vintage whose id is a hash. GET-only: no promote/select/seal/auto-run verb is mounted.
        .route("/vintages/leaderboard", get(leaderboard))
        .route("/vintages/{id}", get(get_vintage))
        .route("/market-data/coverage", get(market_data_coverage))
}

/// One entry of the `GET /api/vintages` list — the selectable shape QE-259's "New backtest" trigger
/// form consumes.
#[derive(Debug, Clone, Serialize)]
pub struct VintageListItem {
    /// The vintage id — the value `POST /api/runs` takes as its `vintage` param.
    pub id: String,
    /// Human display label (currently the vintage id; no distinct label field exists yet).
    pub label: String,
    /// A structured summary the trigger form can render alongside the label.
    pub summary: VintageSummary,
}

/// The per-vintage summary carried in a [`VintageListItem`].
#[derive(Debug, Clone, Serialize)]
pub struct VintageSummary {
    /// Number of strategy chromosomes the vintage bundles.
    pub chromosomes: usize,
    /// The content hash pinning the sealed artefact.
    pub content_hash: String,
    /// Worst-case capital loss under the QE-130 stress set, if attached.
    pub worst_case_loss: Option<f64>,
    /// The vintage artefact format version.
    pub format_version: u16,
}

impl From<&qe_vintage::Vintage> for VintageListItem {
    fn from(v: &qe_vintage::Vintage) -> Self {
        Self {
            id: v.content.vintage_id.clone(),
            label: v.content.vintage_id.clone(),
            summary: VintageSummary {
                chromosomes: v.content.chromosomes.len(),
                content_hash: v.content_hash.clone(),
                worst_case_loss: v.content.worst_case_loss,
                format_version: v.content.format_version,
            },
        }
    }
}

/// `GET /api/vintages` — list the sealed vintages under the configured artifacts dir (ascending by id,
/// each hash-verified on load), as `{ id, label, summary }`. A missing/empty dir yields `[]`.
async fn list_vintages(State(read): State<Arc<ReadState>>) -> Response {
    let repo = read.vintages.clone();
    // `VintageRepository::list` opens + verifies files (blocking fs) — off the async worker.
    match tokio::task::spawn_blocking(move || repo.list()).await {
        Ok(Ok(vintages)) => {
            let items: Vec<VintageListItem> = vintages.iter().map(VintageListItem::from).collect();
            Json(items).into_response()
        }
        Ok(Err(e)) => internal(format!("failed to list vintages: {e}")),
        Err(_) => internal("vintage listing task failed".to_owned()),
    }
}

/// The QE-456 vintage-detail body — **exactly** what QE-467 sealed into `VintageContent`, resliced
/// read-only (no gate recomputation, no re-run of the holdout series). Every field is a projection of
/// the hash-verified sealed artefact plus the vintage→run reverse-join.
#[derive(Debug, Clone, Serialize)]
pub struct VintageDetail {
    /// The vintage id (content hash / rollover label).
    pub id: String,
    /// Human display label (currently the vintage id).
    pub label: String,
    /// The content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The vintage artefact format version (`8` under QE-467).
    pub format_version: u16,
    /// The sealed data provenance (`real` | `synthetic` | `mixed`) — the inspector's provenance banner.
    pub data_provenance: DataProvenance,
    /// Ensemble composition — one entry per chromosome, with its referenced indicators + aligned weight.
    pub composition: Vec<ChromosomeComposition>,
    /// The persisted G1 gate / deflation evidence (QE-467) — read, never recomputed.
    pub seal_evidence: SealEvidence,
    /// The content handle of the persisted net-of-cost holdout return series (QE-467). A **ref**, not the
    /// inline series and not a re-run — the caller consults the series by this handle.
    pub holdout_series_handle: String,
    /// The length of the persisted holdout series (a count over sealed data; the series itself is not
    /// inlined here).
    pub holdout_series_len: usize,
    /// The frozen holdout split `{holdout_range, embargo, train_range}` (QE-467/QE-460).
    pub holdout_split: HoldoutSplit,
    /// The holdout regime composition (QE-125 / QE-467/QE-460).
    pub regime_composition: Vec<RegimeShare>,
    /// The overlap-keyed per-holdout consultation count (QE-467/QE-460).
    pub consultation_count: u64,
    /// The steer delta the search recorded (QE-467/QE-458); `None` for an unsteered vintage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer_delta: Option<SteerDelta>,
    /// The provenance sidecars already carried in the sealed content.
    pub sidecars: VintageSidecars,
    /// The vintage→run reverse-join — every run that produced this vintage, deterministically ordered
    /// (earliest `created_ms`, then lexicographic run id).
    pub producing_runs: Vec<ProducingRun>,
    /// The primary producer (first of `producing_runs` under the deterministic tie-break), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_run: Option<String>,
}

/// One chromosome's composition entry — the indicators its enabled clauses reference and its aligned
/// ensemble weight.
#[derive(Debug, Clone, Serialize)]
pub struct ChromosomeComposition {
    /// Positional index of the chromosome in the sealed ensemble.
    pub index: usize,
    /// The aligned per-chromosome ensemble weight (`VintageContent.weights[index]`).
    pub weight: f64,
    /// The indicators the chromosome's enabled clauses reference.
    pub indicators: Vec<IndicatorRef>,
}

/// One referenced indicator, resolved through the sealed catalogue identity (asserted exact on load).
#[derive(Debug, Clone, Serialize)]
pub struct IndicatorRef {
    /// The genome's raw feature index (`Clause::feature`).
    pub feature: u16,
    /// The resolved catalogue indicator id; `None` for an evolved-formula reference (out of catalogue
    /// range).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `catalogue` for a base-catalogue indicator, `evolved` for a sealed evolved-pool formula.
    pub source: &'static str,
}

/// The provenance sidecars already sealed in the vintage content (QE-431/433/130/116/402).
#[derive(Debug, Clone, Serialize)]
pub struct VintageSidecars {
    /// The content-addressed slippage/impact calibration (QE-431).
    pub slippage: SlippageCalibration,
    /// The advisory portfolio-Kelly sizer (QE-433).
    pub sizer: PortfolioSizer,
    /// Worst-case capital loss under the QE-130 stress set, if attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_case_loss: Option<f64>,
    /// The per-vintage calibration sidecar (QE-116).
    pub calibration: CalibrationProfile,
    /// The pinned catalogue identity the genomes were sealed against (QE-402).
    pub catalogue: CatalogueIdentity,
}

/// A run that produced this vintage (`meta.train.vintage == {id}`) — the reverse-join projection.
#[derive(Debug, Clone, Serialize)]
pub struct ProducingRun {
    /// The producing run's id.
    pub run_id: String,
    /// The run type (`train`).
    pub run_type: String,
    /// The run's current lifecycle status.
    pub status: crate::runs::model::RunStatus,
    /// Creation time (epoch-ms) — the primary tie-break key.
    pub created_ms: u64,
}

/// The blocking outcome of loading + reslicing a vintage detail.
enum DetailOutcome {
    /// The resliced detail to serve.
    Body(Box<VintageDetail>),
    /// No vintage with that id (missing artefact).
    NotFound,
    /// The artefact exists but failed to load/verify (hash/schema/IO/serialise) — a `500`, never a panic.
    Internal(String),
}

/// `GET /api/vintages/{id}` — the detail read for one sealed vintage: composition
/// (chromosomes→indicators + weights), the QE-467-persisted gate/deflation evidence, `data_provenance`,
/// the holdout-series **handle** (not the inline series), the holdout split + regime composition, the
/// provenance sidecars, and the vintage→run reverse-join. Hash-verified on load; recomputes nothing.
/// `404` for an unknown id; `500` (never a panic) for a corrupt/failing-verify artefact.
async fn get_vintage(
    State(read): State<Arc<ReadState>>,
    State(manager): State<Arc<RunManager>>,
    Path(id): Path<String>,
) -> Response {
    let repo = read.vintages.clone();
    let store = manager.store().clone();
    let id_for_msg = id.clone();
    // The vintage load (hash-verify) + the run-index scan are both blocking fs — off the async worker.
    match tokio::task::spawn_blocking(move || build_detail(&repo, &store, &id)).await {
        Ok(DetailOutcome::Body(detail)) => Json(*detail).into_response(),
        Ok(DetailOutcome::NotFound) => not_found_vintage(&id_for_msg),
        Ok(DetailOutcome::Internal(msg)) => internal(msg),
        Err(_) => internal("vintage detail task failed".to_owned()),
    }
}

/// Load, hash-verify and reslice a single vintage into a [`VintageDetail`], joining the producing run(s)
/// from the run store. Pure read-over-sealed-artefact: no gate recomputation, no holdout re-run.
fn build_detail(repo: &VintageRepository, store: &RunStore, id: &str) -> DetailOutcome {
    let vintage = match repo.load(id) {
        Ok(v) => v,
        // A missing artefact is a `404`; every other error (hash/schema mismatch, deserialise, other IO)
        // is a `500` with a message — never a panic.
        Err(VintageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return DetailOutcome::NotFound;
        }
        Err(e) => return DetailOutcome::Internal(format!("failed to load vintage `{id}`: {e}")),
    };

    let holdout_series_handle = match vintage.content.holdout_series.handle() {
        Ok(h) => h,
        Err(e) => return DetailOutcome::Internal(format!("failed to hash holdout series: {e}")),
    };

    let producing_runs = match store.find_runs_by_vintage(id) {
        Ok(metas) => metas
            .into_iter()
            .map(|m| ProducingRun {
                run_id: m.id,
                run_type: m.run_type,
                status: m.status,
                created_ms: m.created_ms,
            })
            .collect::<Vec<_>>(),
        Err(e) => return DetailOutcome::Internal(format!("failed to resolve producing runs: {e}")),
    };
    let primary_run = producing_runs.first().map(|r| r.run_id.clone());

    match build_detail_body(&vintage, holdout_series_handle, producing_runs, primary_run) {
        Ok(detail) => DetailOutcome::Body(Box::new(detail)),
        Err(msg) => DetailOutcome::Internal(msg),
    }
}

/// Project the (already hash-verified) sealed [`Vintage`] into the response DTO. Split out from
/// [`build_detail`] so the composition/sidecar reslice is unit-testable without a run store.
///
/// # Errors
/// A message (mapped to a `500`, never a panic and never a silent JSON `null`) if a chromosome has no
/// aligned weight — a broken seal invariant (`VintageContent::validate` guarantees `weights` is aligned
/// one-to-one with `chromosomes`, so this is unreachable for a validly-sealed artefact, but we surface
/// it rather than emit a silent `NaN`/`null`).
fn build_detail_body(
    vintage: &Vintage,
    holdout_series_handle: String,
    producing_runs: Vec<ProducingRun>,
    primary_run: Option<String>,
) -> Result<VintageDetail, String> {
    let content = &vintage.content;
    // The current build's catalogue schema. `VintageRepository::load` already asserted the sealed
    // `CatalogueIdentity` matches this build **exactly** (QE-402), so this schema is the authoritative
    // basis for resolving the sealed genomes' feature indices to indicator ids.
    let schema = FeatureSchema::from_catalogue(&CatalogueConfig::default());
    let ids = schema.ids();

    let composition = content
        .chromosomes
        .iter()
        .enumerate()
        .map(|(index, genome)| {
            let indicators = genome
                .referenced_features()
                .into_iter()
                .map(|feature| resolve_indicator(feature, ids))
                .collect();
            // The seal validates `weights.len() == chromosomes.len()`; a missing weight is a broken
            // invariant we surface as an error rather than a silent `NaN`/`null`.
            let weight = content.weights.get(index).copied().ok_or_else(|| {
                format!(
                    "vintage `{}` has no weight aligned to chromosome {index} \
                     ({} weights for {} chromosomes) — broken seal invariant",
                    content.vintage_id,
                    content.weights.len(),
                    content.chromosomes.len(),
                )
            })?;
            Ok(ChromosomeComposition {
                index,
                weight,
                indicators,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(VintageDetail {
        id: content.vintage_id.clone(),
        label: content.vintage_id.clone(),
        content_hash: vintage.content_hash.clone(),
        format_version: content.format_version,
        data_provenance: content.provenance.data_provenance,
        composition,
        seal_evidence: content.seal_evidence,
        holdout_series_handle,
        holdout_series_len: content.holdout_series.returns.len(),
        holdout_split: content.provenance.holdout_split.clone(),
        regime_composition: content.provenance.regime_composition.clone(),
        consultation_count: content.provenance.consultation_count,
        steer_delta: content.provenance.steer_delta.clone(),
        sidecars: VintageSidecars {
            slippage: content.slippage.clone(),
            sizer: content.sizer.clone(),
            worst_case_loss: content.worst_case_loss,
            calibration: content.calibration.clone(),
            catalogue: content.catalogue.clone(),
        },
        producing_runs,
        primary_run,
    })
}

/// Resolve a genome feature index to a referenced indicator, distinguishing catalogue from evolved. An
/// index within the catalogue schema resolves to its indicator id (`source = "catalogue"`); an
/// out-of-range index is a sealed evolved-pool formula reference (`source = "evolved"`, id `None` — the
/// pool hashes are surfaced under `sidecars.catalogue.formula_pool`).
fn resolve_indicator(feature: u16, ids: &[String]) -> IndicatorRef {
    match ids.get(feature as usize) {
        Some(id) => IndicatorRef {
            feature,
            id: Some(id.clone()),
            source: "catalogue",
        },
        None => IndicatorRef {
            feature,
            id: None,
            source: "evolved",
        },
    }
}

// ---- QE-466 vintage leaderboard / comparison (informational, NOT a selector) -----------------------
//
// A read-only ranking of already-sealed vintages on the PERSISTED, tradable, deflation-honest metrics
// QE-467 sealed — every number is READ from the sealed artefact, none recomputed. It is structurally
// incapable of selecting/promoting: no promote/select/seal/auto-run verb is mounted, and the ranking
// confers NO deflation credit beyond each vintage's own honest per-run G1 gate (design §9/§3/§11.1).

/// The overlap-keyed holdout-consultation budget the leaderboard ENFORCES (design §4/§9). `consultation_count`
/// is `1` for the single, honest consultation the backtest IS; `> BUDGET` means the *same* holdout was
/// re-consulted by a prior overlapping run — silent campaign-level multiple-testing. `1` is the conservative
/// default (design §4: "the backtest is the *single recorded consultation* of the holdout"). Product may lift
/// it; nothing else depends on the exact value.
pub const HOLDOUT_CONSULTATION_BUDGET: u64 = 1;

/// The chosen enforcement posture (design §9): **rank only on each vintage's own already-deflated evidence,
/// with NO fresh cross-vintage selection statistic on holdout verdicts.** The cross-vintage correlation is a
/// diversity DIAGNOSTIC (effective N), never a rank input — so the leaderboard cannot manufacture new
/// selection evidence over holdout verdicts (which posture (a), a max-statistic/SPA across the set, would).
const ENFORCEMENT_POSTURE: &str = "own-evidence-only";

/// The standing caveat every consumer of the ranking must read (design §9): cross-vintage ranking is
/// inspection, and re-running until the top slot improves is the rejected best-of-N pattern (§3).
const LEADERBOARD_CAVEAT: &str = "Cross-vintage ranking is INSPECTION, not selection. Each vintage already \
     passed its own honest per-run G1 gate; this ordering confers no additional blessing and promotes \
     nothing. Acting on the ranking by re-running until the top slot improves IS the rejected best-of-N \
     pattern (design §3) — the outer selector that re-introduces the uncounted multiple-testing QE-430..454 \
     killed. Every vintage is backtest-holdout only — not paper-confirmed (still owes G2/G3).";

/// How each vintage's DSR bar is treated given its consultation budget (design §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DsrStatus {
    /// Within the consultation budget — the DSR bar is shown normally.
    Ok,
    /// Over-consulted — the DSR bar is escalated/greyed and the vintage is demoted below every within-budget
    /// vintage, so the top slot cannot be "improved" by re-running the holdout.
    Escalated,
}

/// One ranked row of the QE-466 leaderboard — a projection of one sealed vintage's PERSISTED metrics. It
/// carries **only** net-of-cost / tradability / deflation-basis numbers read from `SealEvidence`; there is no
/// gross-Sharpe, equal-weight, lone-Sharpe, or in-sample field to rank on, and **no** promote/select action.
#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardEntry {
    /// 1-based display rank after sorting (within-budget vintages first, then by persisted net-of-cost).
    pub rank: usize,
    /// The vintage id.
    pub id: String,
    /// Human display label (currently the vintage id).
    pub label: String,
    /// The content hash pinning the sealed artefact.
    pub content_hash: String,
    /// The vintage artefact format version.
    pub format_version: u16,
    /// The sealed data provenance (`real` | `synthetic` | `mixed`).
    pub data_provenance: DataProvenance,
    /// **The ranking key** — the DEPLOYED capacity-capped, net-of-cost `min{1×,2×}` cost-stressed holdout
    /// return (QE-467/438/431). `None` on a path that did not run the cost-stress sweep (ranked last within
    /// its budget group). Never a gross / equal-weight / lone-Sharpe number.
    pub cost_stress_net_min: Option<f64>,
    /// Realised turnover of the DEPLOYED capacity-capped ensemble (QE-467).
    pub realised_turnover: f64,
    /// Modelled deployable capacity in USD at target AUM (QE-467).
    pub capacity_usd: f64,
    /// Deflated Sharpe Ratio (QE-131) — the demoted deflation basis (necessary, not sufficient). Rendered
    /// with `dsr_status`, never as a lone health tile.
    pub dsr: f64,
    /// Whether the DSR bar is shown normally or escalated/greyed (over-consulted).
    pub dsr_status: DsrStatus,
    /// The overlap-keyed per-holdout consultation count (QE-467/QE-460).
    pub consultation_count: u64,
    /// `true` when `consultation_count > HOLDOUT_CONSULTATION_BUDGET` — the holdout was re-consulted, so the
    /// vintage is demoted and its DSR bar escalated (ENFORCED, not merely displayed).
    pub over_consulted: bool,
    /// The length of the persisted net-of-cost holdout series (a count over sealed data; never re-run).
    pub holdout_series_len: usize,
    /// The steer/param diff the search recorded (QE-467/QE-458): indicator subset + budget + windows/folds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer_delta: Option<SteerDelta>,
    /// The standing per-vintage label: this is a backtest-holdout verdict, still owing G2/G3 — never
    /// paper- or live-confirmed.
    pub not_paper_confirmed: bool,
}

/// The QE-466 leaderboard body — the ranked sealed vintages plus the cross-vintage diversity diagnostic and
/// the standing anti-selection framing. **A read-only view over sealed artefacts**: it exposes no
/// promote/select/seal/auto-run action, ranks on each vintage's OWN persisted already-deflated evidence, and
/// computes NO fresh cross-vintage selection statistic on holdout verdicts (enforcement posture (b)).
#[derive(Debug, Clone, Serialize)]
pub struct Leaderboard {
    /// The ranked vintages (within-budget first, then descending persisted net-of-cost).
    pub entries: Vec<LeaderboardEntry>,
    /// The QE-430 R(N)/Fisher-z sample-size-deflated positive-mean pairwise correlation over the PERSISTED
    /// net-of-cost holdout series of the displayed set — a DIVERSITY DIAGNOSTIC (are these diverse, or the
    /// same bet re-drawn?), **never** a rank input. `0.0` when fewer than two series are comparable.
    pub cross_vintage_correlation: f64,
    /// The effective N the correlation rested on — the common (aligned) series length across the set. Surfaced
    /// so the operator sees how much data the diversity read stands on (the standard-error caveat made
    /// explicit, mirroring QE-430's `effective_n`).
    pub effective_n: usize,
    /// How the persisted series were aligned before the correlation (the honest v1 limitation).
    pub effective_n_note: String,
    /// The enforcement posture in force (`own-evidence-only` = posture (b)).
    pub enforcement_posture: String,
    /// The consultation budget enforced (`HOLDOUT_CONSULTATION_BUDGET`).
    pub consultation_budget: u64,
    /// Always `true`: every vintage on the leaderboard is backtest-holdout only, still owing G2/G3.
    pub not_paper_confirmed: bool,
    /// The standing caveat (`LEADERBOARD_CAVEAT`).
    pub caveat: String,
}

/// `GET /api/vintages/leaderboard` — rank the sealed vintages on their PERSISTED net-of-cost / capacity /
/// turnover evidence (read, never recomputed), surface the QE-430-deflated cross-vintage correlation +
/// effective N as a diversity diagnostic, and ENFORCE the consultation budget (over-consulted vintages are
/// demoted + their DSR bar escalated). Read-only: recomputes no gate, seals/promotes/selects nothing.
async fn leaderboard(State(read): State<Arc<ReadState>>) -> Response {
    let repo = read.vintages.clone();
    match tokio::task::spawn_blocking(move || repo.list().map(|vs| build_leaderboard(&vs))).await {
        Ok(Ok(board)) => Json(board).into_response(),
        Ok(Err(e)) => internal(format!("failed to list vintages: {e}")),
        Err(_) => internal("vintage leaderboard task failed".to_owned()),
    }
}

/// Build the leaderboard from the (already hash-verified) sealed vintages. Pure over the loaded artefacts —
/// no gate recomputation, no holdout re-run — so it is unit-testable without a server. Ranking rule
/// (posture (b)): within-budget vintages first; within a budget group, descending persisted
/// `cost_stress_net_min` (net-of-cost), then descending DSR, then id — a deterministic ordering over
/// independent already-deflated numbers, never a fresh cross-vintage statistic.
fn build_leaderboard(vintages: &[Vintage]) -> Leaderboard {
    // Cross-vintage correlation over the PERSISTED net-of-cost series (QE-430 R(N)/Fisher-z, reused). Align
    // to the common minimum length first, since `pearson` returns 0 for unequal-length series and the
    // persisted series carry no per-bar timestamps for an exact time alignment (documented v1 limitation).
    let series: Vec<&Vec<f64>> = vintages
        .iter()
        .map(|v| &v.content.holdout_series.returns)
        .collect();
    let min_len = series.iter().map(|s| s.len()).min().unwrap_or(0);
    let (cross_vintage_correlation, effective_n) = if series.len() < 2 || min_len == 0 {
        (0.0, 0)
    } else {
        let aligned: Vec<Vec<f64>> = series.iter().map(|s| s[..min_len].to_vec()).collect();
        let penalty = pairwise_corr_penalty(
            &aligned,
            CorrDeflation::SignificanceFloor {
                z: DEFAULT_SIGNIFICANCE_Z,
            },
        );
        (penalty.value, penalty.effective_n)
    };

    let mut entries: Vec<LeaderboardEntry> = vintages
        .iter()
        .map(|v| {
            let c = &v.content;
            let over_consulted = c.provenance.consultation_count > HOLDOUT_CONSULTATION_BUDGET;
            LeaderboardEntry {
                rank: 0, // assigned after the sort
                id: c.vintage_id.clone(),
                label: c.vintage_id.clone(),
                content_hash: v.content_hash.clone(),
                format_version: c.format_version,
                data_provenance: c.provenance.data_provenance,
                cost_stress_net_min: c.seal_evidence.cost_stress_net_min,
                realised_turnover: c.seal_evidence.realised_turnover,
                capacity_usd: c.seal_evidence.capacity_usd,
                dsr: c.seal_evidence.dsr,
                dsr_status: if over_consulted {
                    DsrStatus::Escalated
                } else {
                    DsrStatus::Ok
                },
                consultation_count: c.provenance.consultation_count,
                over_consulted,
                holdout_series_len: c.holdout_series.returns.len(),
                steer_delta: c.provenance.steer_delta.clone(),
                not_paper_confirmed: true,
            }
        })
        .collect();

    // Enforcement (posture (b)): over-consulted vintages sort BELOW every within-budget vintage regardless of
    // their (possibly holdout-shopped) net-of-cost number — so re-running until the top slot improves demotes
    // rather than promotes. Within a budget group, rank on the vintage's OWN persisted net-of-cost, then DSR,
    // then id (deterministic). `total_cmp` gives a total order over the `f64` keys (None → −∞ ranks last).
    entries.sort_by(|a, b| {
        let net = |e: &LeaderboardEntry| e.cost_stress_net_min.unwrap_or(f64::NEG_INFINITY);
        a.over_consulted
            .cmp(&b.over_consulted)
            .then(net(b).total_cmp(&net(a)))
            .then(b.dsr.total_cmp(&a.dsr))
            .then(a.id.cmp(&b.id))
    });
    for (i, e) in entries.iter_mut().enumerate() {
        e.rank = i + 1;
    }

    Leaderboard {
        entries,
        cross_vintage_correlation,
        effective_n,
        effective_n_note: "Persisted net-of-cost series aligned to the displayed-set minimum length (leading \
             bars) before the QE-430 R(N)/Fisher-z deflation; the persisted series carry no per-bar \
             timestamps for an exact time alignment (v1 limitation)."
            .to_owned(),
        enforcement_posture: ENFORCEMENT_POSTURE.to_owned(),
        consultation_budget: HOLDOUT_CONSULTATION_BUDGET,
        not_paper_confirmed: true,
        caveat: LEADERBOARD_CAVEAT.to_owned(),
    }
}

/// `GET /api/market-data/coverage` — the read-only coverage rows for every instrument stored in the
/// configured market store (`Vec<CoverageRow>`).
async fn market_data_coverage(State(read): State<Arc<ReadState>>) -> Response {
    let store = Arc::clone(&read.market_store);
    // The LMDB scan is blocking — run it off the async worker.
    match tokio::task::spawn_blocking(move || qe_storage::coverage_all(&store)).await {
        Ok(Ok(rows)) => Json(rows).into_response(),
        Ok(Err(e)) => internal(format!("failed to read market-data coverage: {e}")),
        Err(_) => internal("coverage task failed".to_owned()),
    }
}

/// `404` for an unknown vintage id (same JSON `{ "error": … }` shape the read module uses).
fn not_found_vintage(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("vintage `{id}` not found") })),
    )
        .into_response()
}

/// A `500` JSON error body with a message.
fn internal(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_indicator_distinguishes_catalogue_from_evolved() {
        // Feature indices within the current catalogue schema resolve to a real indicator id.
        let schema = FeatureSchema::from_catalogue(&CatalogueConfig::default());
        let ids = schema.ids();
        assert!(!ids.is_empty(), "the default catalogue is non-empty");

        let catalogue = resolve_indicator(0, ids);
        assert_eq!(catalogue.feature, 0);
        assert_eq!(catalogue.source, "catalogue");
        assert_eq!(catalogue.id.as_deref(), Some(ids[0].as_str()));

        // An index past the catalogue is an evolved-formula reference (no catalogue id) — never a panic.
        let evolved = resolve_indicator(ids.len() as u16, ids);
        assert_eq!(evolved.source, "evolved");
        assert!(evolved.id.is_none());
    }
}
