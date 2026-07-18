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

    DetailOutcome::Body(Box::new(build_detail_body(
        &vintage,
        holdout_series_handle,
        producing_runs,
        primary_run,
    )))
}

/// Project the (already hash-verified) sealed [`Vintage`] into the response DTO. Split out from
/// [`build_detail`] so the composition/sidecar reslice is unit-testable without a run store.
fn build_detail_body(
    vintage: &Vintage,
    holdout_series_handle: String,
    producing_runs: Vec<ProducingRun>,
    primary_run: Option<String>,
) -> VintageDetail {
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
            ChromosomeComposition {
                index,
                weight: content.weights.get(index).copied().unwrap_or(f64::NAN),
                indicators,
            }
        })
        .collect();

    VintageDetail {
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
    }
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
