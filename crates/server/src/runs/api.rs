//! Run lifecycle HTTP API (§6.2), mounted under `/api`:
//! `POST /api/runs`, `GET /api/runs`, `GET /api/runs/{id}`, `GET /api/runs/{id}/result`.
//!
//! QE-256 layers session auth over the whole `/api` nest without touching these handlers: the routes
//! are parameterised over [`crate::AppState`] and the handlers keep extracting `State<Arc<RunManager>>`
//! (projected via `FromRef<AppState>`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use super::manager::{CreateError, RunManager};
use super::model::{CreateRunRequest, RunMeta};

/// The run lifecycle routes. Parameterised over [`crate::AppState`]; the handlers still extract
/// `State<Arc<RunManager>>`, projected out of `AppState` via `FromRef`.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/runs", post(create_run).get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/result", get(get_result))
}

/// `POST /api/runs` — validate, create + spawn a run. `201` with `{ "id": … }`, or `400` on a bad
/// request.
async fn create_run(
    State(manager): State<Arc<RunManager>>,
    Json(req): Json<CreateRunRequest>,
) -> Response {
    match manager.create(req).await {
        Ok(id) => (StatusCode::CREATED, Json(json!({ "id": id }))).into_response(),
        Err(CreateError::Validation(msg)) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
        }
        Err(CreateError::Io(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to create run: {e}") })),
        )
            .into_response(),
    }
}

/// `GET /api/runs` — list runs newest-first (index order reversed), each enriched with its
/// authoritative `meta.json` status/progress.
async fn list_runs(State(manager): State<Arc<RunManager>>) -> Response {
    let store = manager.store();
    let index = match store.read_index() {
        Ok(index) => index,
        Err(e) => return internal(format!("failed to read index: {e}")),
    };
    let mut runs: Vec<RunMeta> = Vec::with_capacity(index.len());
    // Newest first.
    for entry in index.iter().rev() {
        match store.read_meta(&entry.id) {
            Ok(Some(meta)) => runs.push(meta),
            Ok(None) => {} // indexed but meta missing (mid-create race / manual deletion) — skip.
            Err(e) => return internal(format!("failed to read run `{}`: {e}", entry.id)),
        }
    }
    Json(runs).into_response()
}

/// `GET /api/runs/{id}` — one run's `meta.json` (status + progress), or `404`.
async fn get_run(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    match manager.store().read_meta(&id) {
        Ok(Some(meta)) => Json(meta).into_response(),
        Ok(None) => not_found(&id),
        Err(e) => internal(format!("failed to read run `{id}`: {e}")),
    }
}

/// `GET /api/runs/{id}/result` — the run's `result.json` once `succeeded`. `404` if the run is
/// unknown, `409` if it exists but has no result yet.
async fn get_result(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    let store = manager.store();
    let meta = match store.read_meta(&id) {
        Ok(Some(meta)) => meta,
        Ok(None) => return not_found(&id),
        Err(e) => return internal(format!("failed to read run `{id}`: {e}")),
    };
    if meta.status != super::model::RunStatus::Succeeded {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "result not available", "status": meta.status })),
        )
            .into_response();
    }
    match std::fs::read(store.result_path(&id)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(_) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "result artefact missing" })),
        )
            .into_response(),
    }
}

/// `404` body for an unknown run id.
fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("run `{id}` not found") })),
    )
        .into_response()
}

/// `500` body with a message.
fn internal(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
        .into_response()
}
