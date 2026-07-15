//! Run lifecycle HTTP API (§6.2), mounted under `/api`:
//! `POST /api/runs`, `GET /api/runs`, `GET /api/runs/{id}`, `GET /api/runs/{id}/result`.
//!
//! QE-256 layers session auth over the whole `/api` nest without touching these handlers: the routes
//! are parameterised over [`crate::AppState`] and the handlers keep extracting `State<Arc<RunManager>>`
//! (projected via `FromRef<AppState>`).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::manager::{CreateError, RunManager};
use super::model::{CreateRunRequest, RunMeta, RunStatus};
use super::store::RunStore;

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
        // QE-407: the server has begun shutting down and no longer accepts runs.
        Err(err @ CreateError::ShuttingDown) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
        Err(CreateError::Io(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to create run: {e}") })),
        )
            .into_response(),
    }
}

/// Query string for `GET /api/runs`. `?type=backtest|train` filters to that run type (QE-408); absent
/// ⇒ all runs. An unrecognised value simply matches nothing (empty list), never an error.
#[derive(Debug, Deserialize)]
struct ListRunsQuery {
    /// Optional run-type filter, matched against `IndexEntry.run_type` / `RunMeta.type`.
    #[serde(rename = "type")]
    run_type: Option<String>,
}

/// `GET /api/runs` — list runs newest-first (index order reversed), each enriched with its
/// authoritative `meta.json` status/progress.
///
/// QE-408: an optional `?type=` query filters to a single run type. The filter is applied at the
/// **index** level (before reading each `meta.json`), so a type-specific caller (e.g. the Backtests
/// list) does not make the server over-read the other type's meta. With no `?type=` the behaviour is
/// byte-identical to before (all runs).
///
/// QE-411: the index read and the per-run `meta.json` reads are blocking `std::fs`, so the whole batch
/// runs inside a single [`tokio::task::spawn_blocking`] closure (one closure for the whole list, not one
/// per run) — keeping the async worker free while preserving the newest-first order and the
/// skip-on-missing-meta semantics.
async fn list_runs(
    State(manager): State<Arc<RunManager>>,
    Query(query): Query<ListRunsQuery>,
) -> Response {
    let store = manager.store().clone();
    let type_filter = query.run_type;
    match tokio::task::spawn_blocking(move || list_runs_blocking(&store, type_filter.as_deref()))
        .await
    {
        Ok(Ok(runs)) => Json(runs).into_response(),
        Ok(Err(msg)) => internal(msg),
        Err(_) => internal("run listing task failed".to_owned()),
    }
}

/// The blocking body of [`list_runs`], run off the async executor. Returns the newest-first runs, or a
/// pre-formatted error message identical to the previous inline error bodies. When `type_filter` is
/// `Some`, only index entries whose `run_type` matches are read + returned (QE-408); `None` is the
/// original all-runs behaviour.
fn list_runs_blocking(store: &RunStore, type_filter: Option<&str>) -> Result<Vec<RunMeta>, String> {
    let index = store
        .read_index()
        .map_err(|e| format!("failed to read index: {e}"))?;
    let mut runs: Vec<RunMeta> = Vec::with_capacity(index.len());
    // Newest first.
    for entry in index.iter().rev() {
        // QE-408: skip the other run type at the index level, before touching its `meta.json`.
        if type_filter.is_some_and(|want| entry.run_type != want) {
            continue;
        }
        match store.read_meta(&entry.id) {
            Ok(Some(meta)) => runs.push(meta),
            Ok(None) => {} // indexed but meta missing (mid-create race / manual deletion) — skip.
            Err(e) => return Err(format!("failed to read run `{}`: {e}", entry.id)),
        }
    }
    Ok(runs)
}

/// `GET /api/runs/{id}` — one run's `meta.json` (status + progress), or `404`.
///
/// QE-411: the `meta.json` read is blocking `std::fs`, so it runs inside [`tokio::task::spawn_blocking`].
async fn get_run(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    let store = manager.store().clone();
    let task_id = id.clone();
    match tokio::task::spawn_blocking(move || store.read_meta(&task_id)).await {
        Ok(Ok(Some(meta))) => Json(meta).into_response(),
        Ok(Ok(None)) => not_found(&id),
        Ok(Err(e)) => internal(format!("failed to read run `{id}`: {e}")),
        Err(_) => internal("run task failed".to_owned()),
    }
}

/// The outcome of the blocking `get_result` work, mapped to a `Response` on the async side so every
/// status code and JSON body stays byte-identical to the previous inline implementation.
enum ResultOutcome {
    /// The `result.json` bytes to serve (`200`).
    Body(Vec<u8>),
    /// The run id is unknown (`404`).
    NotFound,
    /// The run exists but is not `succeeded` yet (`409`, carries the status for the body).
    NotReady(RunStatus),
    /// The run is `succeeded` but the artefact could not be read (`409`).
    Missing,
    /// Reading `meta.json` failed (`500`, carries the formatted message).
    MetaError(String),
}

/// `GET /api/runs/{id}/result` — the run's `result.json` once `succeeded`. `404` if the run is
/// unknown, `409` if it exists but has no result yet.
///
/// QE-411: the `meta.json` read and the `result.json` read are blocking `std::fs`, so the whole sequence
/// runs inside one [`tokio::task::spawn_blocking`] closure; the `Response` is built from its outcome.
async fn get_result(State(manager): State<Arc<RunManager>>, Path(id): Path<String>) -> Response {
    let store = manager.store().clone();
    let task_id = id.clone();
    match tokio::task::spawn_blocking(move || read_result_outcome(&store, &task_id)).await {
        Ok(ResultOutcome::Body(bytes)) => {
            ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
        }
        Ok(ResultOutcome::NotFound) => not_found(&id),
        Ok(ResultOutcome::NotReady(status)) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "result not available", "status": status })),
        )
            .into_response(),
        Ok(ResultOutcome::Missing) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "result artefact missing" })),
        )
            .into_response(),
        Ok(ResultOutcome::MetaError(msg)) => internal(msg),
        Err(_) => internal("result task failed".to_owned()),
    }
}

/// The blocking body of [`get_result`], run off the async executor: read `meta.json`, gate on
/// `succeeded`, then read the `result.json` bytes. Mirrors the previous inline control flow exactly.
fn read_result_outcome(store: &RunStore, id: &str) -> ResultOutcome {
    let meta = match store.read_meta(id) {
        Ok(Some(meta)) => meta,
        Ok(None) => return ResultOutcome::NotFound,
        Err(e) => return ResultOutcome::MetaError(format!("failed to read run `{id}`: {e}")),
    };
    if meta.status != RunStatus::Succeeded {
        return ResultOutcome::NotReady(meta.status);
    }
    match store.read_result(id) {
        Ok(bytes) => ResultOutcome::Body(bytes),
        Err(_) => ResultOutcome::Missing,
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

#[cfg(test)]
mod tests {
    /// QE-411 AC guard: no blocking `std::fs` call remains on an async handler body. Scans this file's
    /// handler region (everything before this `#[cfg(test)]` module) and asserts the `std::fs::` token
    /// is absent — the run-store fs primitives now live in `store.rs` and are reached only through
    /// `spawn_blocking`. The needle is built at runtime so this assertion line is not a self-match.
    #[test]
    fn handlers_do_no_blocking_std_fs() {
        let src = include_str!("api.rs");
        // The handler code is everything before the test module attribute.
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        // Drop line comments so doc/comment mentions of the token never count.
        let stripped: String = code
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let needle = format!("std{sep}fs{sep}", sep = "::");
        assert!(
            !stripped.contains(&needle),
            "blocking `std::fs` must not appear on an async run handler body (QE-411)"
        );
    }
}
