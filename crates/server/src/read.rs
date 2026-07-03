//! QE-257 read APIs (spec §6.2), mounted under the **session-gated** `/api` subtree:
//! `GET /api/vintages` and `GET /api/market-data/coverage`.
//!
//! Both are registered inside [`crate::auth::protected_routes`], so they inherit the QE-256
//! `require_session` gate (no session ⇒ `401`) without any per-handler auth code. The handlers run
//! the blocking LMDB / filesystem work inside [`tokio::task::spawn_blocking`], keeping async confined
//! and non-blocking (mirrors the QE-256 verifier pattern).

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{http::StatusCode, Json, Router};
use serde::Serialize;
use serde_json::json;

use crate::ReadState;

/// The QE-257 read routes. Parameterised over [`crate::AppState`]; the handlers extract
/// `State<Arc<ReadState>>`, projected out of `AppState` via `FromRef`.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/vintages", get(list_vintages))
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

/// A `500` JSON error body with a message.
fn internal(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
        .into_response()
}
