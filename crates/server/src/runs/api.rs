//! Run lifecycle HTTP API (§6.2), mounted under `/api`:
//! `POST /api/runs`, `GET /api/runs`, `GET /api/runs/{id}`, `GET /api/runs/{id}/result`.
//!
//! QE-256 layers session auth over the whole `/api` nest without touching these handlers: the routes
//! are parameterised over [`crate::AppState`] and the handlers keep extracting `State<Arc<RunManager>>`
//! (projected via `FromRef<AppState>`).

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::manager::{CreateError, RunManager};
use super::model::{CreateRunRequest, IndexEntry, Progress, RunMeta, RunStatus, TrainProgress};
use super::store::RunStore;

/// Default page size for `GET /api/runs` when `?limit=` is absent (spec QE-410 "caps result size").
const DEFAULT_LIMIT: usize = 50;
/// Hard ceiling on `?limit=` so a caller cannot ask for the whole history in one page.
const MAX_LIMIT: usize = 200;

/// QE-425: explicit request-body cap on `POST /api/runs`. A run-spec JSON (window + resolution +
/// universe + costs + strategy config) is a few KB in practice; 256 KiB leaves ~20–50× headroom for a
/// pathologically large universe while rejecting any multi-MB body far below axum's 2 MiB default. An
/// over-cap body is short-circuited to `413 Payload Too Large` before the handler runs.
const RUN_SPEC_BODY_LIMIT: usize = 256 * 1024;

/// The run lifecycle routes. Parameterised over [`crate::AppState`]; the handlers still extract
/// `State<Arc<RunManager>>`, projected out of `AppState` via `FromRef`.
///
/// QE-425: the [`RUN_SPEC_BODY_LIMIT`] body cap is layered on this sub-router. The only body-carrying
/// route is `POST /api/runs`; the sibling GETs are bodyless, so the cap is a no-op for them and an
/// over-limit run spec returns `413` before `create_run` is reached.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/runs", post(create_run).get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/result", get(get_result))
        .layer(DefaultBodyLimit::max(RUN_SPEC_BODY_LIMIT))
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

/// Query string for `GET /api/runs` (QE-408 `?type=`, extended by QE-410). Every field is optional and
/// they **compose**:
/// - `?type=backtest|train` — run-type filter, applied at the **index** level (no meta read); an
///   unrecognised value simply matches nothing (empty page), never an error.
/// - `?status=queued|running|succeeded|failed` — status filter, evaluated against `meta.json`.
/// - `?limit=` — page size (defaults to [`DEFAULT_LIMIT`], capped at [`MAX_LIMIT`]).
/// - `?cursor=<id>` — return the page of runs strictly **older** than this run id (id-anchored, stable
///   under concurrent creates — see [`list_runs_blocking`]).
#[derive(Debug, Default, Deserialize)]
struct ListRunsQuery {
    /// Optional run-type filter, matched against `IndexEntry.run_type` / `RunMeta.type`.
    #[serde(rename = "type")]
    run_type: Option<String>,
    /// Optional lifecycle-status filter, matched against `RunMeta.status`.
    status: Option<RunStatus>,
    /// Page size; `None` ⇒ [`DEFAULT_LIMIT`], clamped to [`MAX_LIMIT`].
    limit: Option<usize>,
    /// Opaque pagination cursor: the run id after which (older than which) to resume.
    cursor: Option<String>,
}

/// The **slim** list projection (QE-410) — the per-row shape `GET /api/runs` returns, deliberately
/// smaller than the full [`RunMeta`] served by `GET /api/runs/{id}`. Carries only the fields the run
/// lists render live: identity (`id`/`type`/`label`), lifecycle (`status`/`progress`), the rich
/// training progress (`train`, small + live), and `created_ms`. The heavy immutable `params` (universe
/// arrays, costs, strategy config) is **deferred** to the detail endpoint.
///
/// `label` is sourced from `index.json` (already read), so it costs no extra meta read: it is the
/// vintage id (backtest) or `"train {start}→{end}"` (train) — the lists' identifying column.
#[derive(Debug, Clone, Serialize)]
struct RunListItem {
    /// Run id.
    id: String,
    /// Run type (`backtest` or `train`).
    #[serde(rename = "type")]
    run_type: String,
    /// Human discovery label from the index (vintage id / window).
    label: String,
    /// Current lifecycle status.
    status: RunStatus,
    /// Latest coarse progress (`pct`/`stage`/`msg`).
    progress: Progress,
    /// Rich training progress — present only on `train` runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    train: Option<TrainProgress>,
    /// Creation time (epoch-ms).
    created_ms: u64,
}

impl RunListItem {
    /// Project an index entry + its authoritative `meta.json` into the slim list row.
    fn project(entry: &IndexEntry, meta: RunMeta) -> Self {
        Self {
            id: meta.id,
            run_type: entry.run_type.clone(),
            label: entry.label.clone(),
            status: meta.status,
            progress: meta.progress,
            train: meta.train,
            created_ms: meta.created_ms,
        }
    }
}

/// The paginated `GET /api/runs` envelope (QE-410). `runs` is the newest-first page (capped); when more
/// older rows remain, `next_cursor` is the id to pass as `?cursor=` for the following page, else `null`.
#[derive(Debug, Clone, Serialize)]
struct RunPage {
    /// The page of slim run rows, newest-first.
    runs: Vec<RunListItem>,
    /// Cursor for the next (older) page, or `null` when this is the last page.
    next_cursor: Option<String>,
}

/// `GET /api/runs` — one **page** of runs newest-first (index order reversed), each projected to the
/// slim [`RunListItem`] shape and enriched with its authoritative `meta.json` status/progress.
///
/// QE-408: an optional `?type=` query filters to a single run type, applied at the **index** level
/// (before reading each `meta.json`), so a type-specific caller (e.g. the Backtests list) does not make
/// the server over-read the other type's meta.
///
/// QE-410: `?limit=`/`?cursor=` paginate (id-anchored cursor — stable under concurrent creates) and
/// `?status=` filters by lifecycle status; the response is a [`RunPage`] envelope with a `next_cursor`.
/// The slim projection defers heavy `params` to `GET /api/runs/{id}`.
///
/// QE-411: the index read and the per-run `meta.json` reads are blocking `std::fs`, so the whole batch
/// runs inside a single [`tokio::task::spawn_blocking`] closure (one closure for the whole page, not one
/// per run) — keeping the async worker free while preserving the newest-first order and the
/// skip-on-missing-meta semantics.
async fn list_runs(
    State(manager): State<Arc<RunManager>>,
    Query(query): Query<ListRunsQuery>,
) -> Response {
    let store = manager.store().clone();
    match tokio::task::spawn_blocking(move || list_runs_blocking(&store, &query)).await {
        Ok(Ok(page)) => Json(page).into_response(),
        Ok(Err(msg)) => internal(msg),
        Err(_) => internal("run listing task failed".to_owned()),
    }
}

/// The blocking body of [`list_runs`], run off the async executor.
///
/// Walks the index newest-first (`iter().rev()`). Pagination is an **id-anchored cursor**: `?cursor=id`
/// resumes at the entry immediately older than that id, so a run created between two page fetches (which
/// always prepends to the newest-first view) can never shift, skip, or duplicate a cursor-paginated
/// older page — the AC's "paginates stably under concurrent creates". An unknown cursor yields an empty
/// continuation (never a restart-from-top, which would duplicate rows).
///
/// The slice is taken at the **index** level: with only `?type=` (index-level skip) we read exactly the
/// page's `meta.json` files. `?status=` is evaluated from `meta.json` (status is not in the index), so a
/// status filter may scan past filtered rows to fill the page.
fn list_runs_blocking(store: &RunStore, query: &ListRunsQuery) -> Result<RunPage, String> {
    let index = store
        .read_index()
        .map_err(|e| format!("failed to read index: {e}"))?;
    // Newest-first view of the append-only index.
    let newest_first: Vec<&IndexEntry> = index.iter().rev().collect();

    // Resolve the id-anchored cursor to a start offset in the newest-first view.
    let start = match query.cursor.as_deref() {
        Some(cursor) => match newest_first.iter().position(|e| e.id == cursor) {
            Some(pos) => pos + 1,
            // Unknown cursor (e.g. a since-removed run) ⇒ no further rows, rather than duplicating.
            None => newest_first.len(),
        },
        None => 0,
    };

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let type_filter = query.run_type.as_deref();
    let status_filter = query.status;

    let mut runs: Vec<RunListItem> = Vec::with_capacity(limit.min(newest_first.len()));
    // `idx` advances past every candidate we consider (matched, skipped, or missing) so `next_cursor`
    // can report whether any further entry remains after the page.
    let mut idx = start;
    while idx < newest_first.len() && runs.len() < limit {
        let entry = newest_first[idx];
        idx += 1;
        // QE-408: skip the other run type at the index level, before touching its `meta.json`.
        if type_filter.is_some_and(|want| entry.run_type != want) {
            continue;
        }
        let meta = match store.read_meta(&entry.id) {
            Ok(Some(meta)) => meta,
            // Indexed but meta missing (mid-create race / manual deletion) — skip.
            Ok(None) => continue,
            Err(e) => return Err(format!("failed to read run `{}`: {e}", entry.id)),
        };
        // QE-410: status filter — evaluated from the authoritative meta.
        if status_filter.is_some_and(|want| meta.status != want) {
            continue;
        }
        runs.push(RunListItem::project(entry, meta));
    }

    // More older rows remain iff we stopped on a full page with entries left to scan; anchor the next
    // page on the last row we returned.
    let next_cursor = if idx < newest_first.len() {
        runs.last().map(|r| r.id.clone())
    } else {
        None
    };
    Ok(RunPage { runs, next_cursor })
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
