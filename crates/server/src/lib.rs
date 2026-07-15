//! qe-server (QE-254) — the admin-UI backend scaffold.
//!
//! A **second composition root** (ADR D4a): an axum + tokio HTTP server that reuses the training-side
//! and shared engine crates. Async lives **only** here; the QE-132 firewall and QE-001 decoupling
//! guards forbid any `qe-runtime`/`qe-venue` edge, so the server never links the live trading path.
//!
//! This scaffold delivers exactly three things (later tickets fill in the rest):
//! - a health endpoint (`GET /api/health` → `200 {"status":"ok"}`),
//! - static-SPA serving at `/` with a client-side-routing fallback to `index.html`,
//! - the reserved `/api` namespace that QE-255 (runs), QE-256 (auth) and QE-257 (read APIs) extend.
//!
//! The router is built by [`build_router`] as a plain [`axum::Router`], so it can be driven
//! in-process with `tower::ServiceExt::oneshot` in tests — no network bind required.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRef, Request};
use axum::response::Response;
use axum::{http::StatusCode, routing::get, Json, Router};
use qe_storage::MarketStore;
use qe_vintage::VintageRepository;
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

pub mod auth;
pub mod config;
pub mod read;
pub mod runs;

pub use auth::{
    mint_session_cookie, AuthConfig, AuthContext, GoogleClaims, IdTokenVerifier, VerifyError,
    SESSION_COOKIE_NAME,
};
pub use config::{
    check_storage_dirs_match, load_app_config, resolve_config_path, server_storage_dirs,
    StorageDirMismatch, StorageDirs,
};
pub use runs::{CliJobSpawner, JobSpawner, RunManager};

/// Shared application state carried by the `/api` router.
///
/// A single state type lets QE-256 layer session auth over the whole `/api` nest while the QE-255 run
/// handlers keep extracting `State<Arc<RunManager>>` unchanged — the [`FromRef`] impls below project
/// [`AppState`] onto each sub-state.
#[derive(Clone)]
pub struct AppState {
    /// The QE-255 run-lifecycle manager.
    pub manager: Arc<RunManager>,
    /// The QE-256 OAuth + session context.
    pub auth: Arc<AuthContext>,
    /// The QE-257 read-API state (sealed-vintage repo + opened market store).
    pub read: Arc<ReadState>,
}

impl AppState {
    /// Build the application state from its three halves.
    pub fn new(manager: Arc<RunManager>, auth: Arc<AuthContext>, read: Arc<ReadState>) -> Self {
        Self {
            manager,
            auth,
            read,
        }
    }
}

impl FromRef<AppState> for Arc<RunManager> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.manager)
    }
}

impl FromRef<AppState> for Arc<AuthContext> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.auth)
    }
}

impl FromRef<AppState> for Arc<ReadState> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.read)
    }
}

/// State backing the QE-257 read APIs: the sealed-vintage repository (a cheap path wrapper) and the
/// **once-opened** market store.
///
/// The market store is opened a single time at startup and shared by `Arc`, never per request:
/// [`MarketStore::open`] documents that opening the same path more than once concurrently in a process
/// is undefined behaviour, so a per-request open under concurrent load would be unsound.
pub struct ReadState {
    /// The sealed-vintage repository rooted at the configured artifacts dir.
    pub vintages: VintageRepository,
    /// The opened market store the coverage endpoint scans.
    pub market_store: Arc<MarketStore>,
}

impl ReadState {
    /// Build read state from a vintage repository + an opened market store.
    pub fn new(vintages: VintageRepository, market_store: Arc<MarketStore>) -> Self {
        Self {
            vintages,
            market_store,
        }
    }
}

/// Default bind address when `QE_SERVER_ADDR` is unset. Loopback-only so a fresh run never exposes
/// the (unauthenticated, this ticket) server on a public interface.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Default static-assets directory when `QE_SERVER_STATIC_DIR` is unset. A **relative** path (never a
/// hard-coded absolute one): the placeholder `index.html` committed here is served until QE-258 builds
/// the real SPA into this dir.
///
/// This default is **CWD-relative**, so it only resolves to the committed placeholder when the binary
/// is launched from the workspace root. A real deploy should set `QE_SERVER_STATIC_DIR` to the built
/// SPA's absolute path (QE-258 builds the SPA to the configured directory); if the path can't be
/// resolved, static serving degrades to `404` rather than panicking (see [`build_router`]).
pub const DEFAULT_STATIC_DIR: &str = "crates/server/static";

/// Default state directory when `QE_SERVER_DATA_DIR` is unset. The run store lives at
/// `<data_dir>/runs`. A **relative** default (`data`, CWD-relative — never hard-coded absolute),
/// consistent with the repo's `data/` layout (`qe-config` `storage.market_dir = data/lmdb/market`,
/// `artifacts_dir = data/artifacts`). Spec §6.4 names this `QE_DATA_DIR`; we keep the crate-local
/// `QE_SERVER_` prefix for consistency with the QE-254 env vars.
pub const DEFAULT_DATA_DIR: &str = "data";

/// Default bound on concurrently-running subprocesses when `QE_SERVER_MAX_CONCURRENCY` is unset.
pub const DEFAULT_MAX_CONCURRENCY: usize = 2;

/// QE-407: how long graceful shutdown waits for in-flight run supervisors to finish before aborting
/// and terminally marking them `failed`. Bounded so a wedged child can never hold the process open.
pub const DEFAULT_SHUTDOWN_DRAIN: Duration = Duration::from_secs(20);

/// Environment variable naming the bind address (12-factor, `QE_`-prefixed like `qe-config`).
pub const ENV_ADDR: &str = "QE_SERVER_ADDR";

/// Environment variable naming the static-assets directory.
pub const ENV_STATIC_DIR: &str = "QE_SERVER_STATIC_DIR";

/// Environment variable naming the state directory (holds the `runs/` store).
pub const ENV_DATA_DIR: &str = "QE_SERVER_DATA_DIR";

/// Environment variable naming the max number of concurrently-running run subprocesses.
pub const ENV_MAX_CONCURRENCY: &str = "QE_SERVER_MAX_CONCURRENCY";

/// Server transport configuration (bind address + static-assets dir).
///
/// These are server-only knobs, so they live here rather than in `qe-config`'s training-domain schema;
/// the `QE_` prefix + env-override style is deliberately consistent with the `qe-config` conventions.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address the server binds to.
    pub addr: SocketAddr,
    /// Directory of built SPA static assets served at `/`.
    pub static_dir: PathBuf,
    /// State directory; the run store lives at `<data_dir>/runs`.
    pub data_dir: PathBuf,
    /// Path to the `qe-cli` (`qe`) binary the server spawns for backtest runs.
    pub cli_bin: PathBuf,
    /// Max number of concurrently-running run subprocesses (excess runs stay `queued`).
    pub max_concurrency: usize,
    /// QE-419: path to the `qe-config` file the server loads for the shared `[storage]` dirs and pins
    /// onto every spawned `qe-cli` (via `QE_CONFIG`) so both sides read ONE source of truth. Resolved
    /// from `QE_CONFIG` or the `config.toml` default — identical to the CLI's own resolution.
    pub config_path: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: DEFAULT_ADDR
                .parse()
                .expect("DEFAULT_ADDR is a valid socket address"),
            static_dir: PathBuf::from(DEFAULT_STATIC_DIR),
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            cli_bin: runs::resolve_cli_bin(),
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            config_path: config::resolve_config_path(),
        }
    }
}

/// Error resolving [`ServerConfig`] from the environment.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `QE_SERVER_ADDR` was set but is not a parseable `host:port`.
    #[error("invalid QE_SERVER_ADDR=`{value}`: {message}")]
    BadAddr {
        /// The offending value.
        value: String,
        /// Parser message.
        message: String,
    },
    /// `QE_SERVER_MAX_CONCURRENCY` was set but is not a positive integer.
    #[error("invalid QE_SERVER_MAX_CONCURRENCY=`{value}`: expected a positive integer")]
    BadConcurrency {
        /// The offending value.
        value: String,
    },
}

impl ServerConfig {
    /// Resolve config from `QE_SERVER_ADDR` / `QE_SERVER_STATIC_DIR`, falling back to the defaults.
    ///
    /// # Errors
    /// Returns [`ConfigError::BadAddr`] if `QE_SERVER_ADDR` is set but not a valid `host:port`.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        if let Ok(raw) = std::env::var(ENV_ADDR) {
            cfg.addr = raw
                .parse()
                .map_err(|e: std::net::AddrParseError| ConfigError::BadAddr {
                    value: raw.clone(),
                    message: e.to_string(),
                })?;
        }
        if let Ok(dir) = std::env::var(ENV_STATIC_DIR) {
            cfg.static_dir = PathBuf::from(dir);
        }
        if let Ok(dir) = std::env::var(ENV_DATA_DIR) {
            cfg.data_dir = PathBuf::from(dir);
        }
        if let Ok(bin) = std::env::var(runs::spawn::ENV_CLI_BIN) {
            cfg.cli_bin = PathBuf::from(bin);
        }
        if let Ok(raw) = std::env::var(ENV_MAX_CONCURRENCY) {
            let n: usize = raw
                .parse()
                .ok()
                .filter(|&n| n >= 1)
                .ok_or_else(|| ConfigError::BadConcurrency { value: raw.clone() })?;
            cfg.max_concurrency = n;
        }
        cfg.config_path = config::resolve_config_path();
        Ok(cfg)
    }

    /// Build the [`RunManager`] for this config: a [`CliJobSpawner`] over `cli_bin` **pinned to
    /// `config_path`** (QE-419: the child reads the same `qe-config` the server guarded), a run store
    /// at `<data_dir>/runs`, and the configured worker-pool bound.
    pub fn run_manager(&self) -> Arc<RunManager> {
        let spawner = Arc::new(
            CliJobSpawner::new(self.cli_bin.clone()).with_config_path(self.config_path.clone()),
        );
        Arc::new(RunManager::new(
            self.data_dir.join("runs"),
            spawner,
            self.max_concurrency,
        ))
    }
}

/// Health check: `GET /api/health` → `200`, `{"status":"ok"}`.
async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// Fallback for unmatched `/api/*` paths: a plain `404`. Without an explicit fallback on the nested
/// API router, an unknown `/api/*` request would propagate to the outer SPA fallback and be answered
/// with `index.html`; this keeps `/api` a reserved JSON namespace so later tickets own it cleanly.
async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// The request-id header name used by the QE-413 tracing stack (set on the request, propagated onto
/// the response, and folded into the per-request span).
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Open the per-request span for the `/api` [`TraceLayer`] (QE-413).
///
/// Carries `method`, `path`, and `request_id`; the response event ([`on_http_response`]) adds
/// `status` and `latency_ms`. `/api/health` is polled continuously (readiness probes), so its span
/// is opened at `debug` — a production `info` filter suppresses that per-request spam while every
/// other route logs at `info`. Matching the trailing `/health` is robust to axum nesting (the inner
/// router may see `/health` or the full `/api/health`).
fn make_http_span(req: &Request) -> tracing::Span {
    let method = req.method().as_str();
    let path = req.uri().path();
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    if path.ends_with("/health") {
        tracing::debug_span!("http.request", method = %method, path = %path, request_id = %request_id)
    } else {
        tracing::info_span!("http.request", method = %method, path = %path, request_id = %request_id)
    }
}

/// Emit the single completion event for a request, carrying `status` and `latency_ms`. The event is
/// parented on the request span (so it inherits `method`/`path`/`request_id`) and is emitted at the
/// span's own level — so `/api/health` stays at `debug` and is filtered out in production.
fn on_http_response(res: &Response, latency: Duration, span: &tracing::Span) {
    let status = res.status().as_u16();
    let latency_ms = latency.as_secs_f64() * 1000.0;
    if span.metadata().map(|m| *m.level()) == Some(tracing::Level::INFO) {
        tracing::info!(parent: span, status, latency_ms, "http request completed");
    } else {
        tracing::debug!(parent: span, status, latency_ms, "http request completed");
    }
}

/// Build the application [`Router`] serving `static_dir` at `/`.
///
/// Layout:
/// - `/api/*` is a **nested** sub-router (health lives at `/api/health`); because the nest owns the
///   whole `/api` prefix, an unknown `/api/*` path returns `404` from the API router instead of being
///   swallowed by the SPA fallback — keeping `/api` a clean reserved namespace for later tickets.
/// - everything else is served from `static_dir` via `ServeDir`, with a per-request fallback to
///   `index.html` so client-side SPA routes still return the app shell. A missing dir/index yields a
///   graceful `404` (no panic), which is fine before QE-258 builds the real SPA.
///
/// The `/api` sub-router carries the shared [`AppState`]. QE-256 session auth is applied to the
/// **protected** subtree only (`/api/me` + the QE-255 `/api/runs*` routes) via [`auth::require_session`];
/// `/api/health` and `/api/auth/*` stay public (you cannot hold a session before logging in). An
/// unknown `/api/*` path still returns a reserved-namespace `404` (unauthenticated), unchanged from
/// QE-254.
pub fn build_router(static_dir: &Path, state: AppState) -> Router {
    let index = static_dir.join("index.html");
    let static_service = ServeDir::new(static_dir).fallback(ServeFile::new(index));

    let protected = auth::protected_routes(Arc::clone(&state.auth));

    // QE-413 per-request tracing, applied to the `/api` router only (static-file serving stays
    // quiet). Order is outermost→innermost: stamp `x-request-id` first (so the span and downstream
    // handlers see it), open the trace span, then propagate the id onto the response.
    let trace = ServiceBuilder::new()
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_http_span)
                .on_request(())
                .on_response(on_http_response),
        )
        .layer(PropagateRequestIdLayer::x_request_id());

    let api = Router::new()
        .route("/health", get(health))
        .merge(auth::public_routes())
        .merge(protected)
        .fallback(api_not_found)
        .with_state(state)
        .layer(trace);

    Router::new()
        .nest("/api", api)
        .fallback_service(static_service)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard that restores (or clears) an env var when dropped, so env-mutating tests don't leak
    /// into each other. Env access is process-global; these tests run serially via the single test
    /// binary and each restores its var.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
        fn clear(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    // The three cases share one test because `from_env` reads process-global env vars; running them as
    // separate `#[test]`s would let cargo's parallel harness race the mutations. Sequenced here, each
    // `EnvGuard` still restores the prior value so the surrounding process env is left untouched.
    #[test]
    fn from_env_defaults_overrides_and_rejects_bad_addr() {
        // Defaults when unset. QE-419: the storage dirs are no longer `ServerConfig` fields — they
        // come from `qe-config` `[storage]` (covered in `config::tests`); `config_path` defaults to
        // the CLI-shared `config.toml`.
        {
            let _a = EnvGuard::clear(ENV_ADDR);
            let _s = EnvGuard::clear(ENV_STATIC_DIR);
            let _d = EnvGuard::clear(ENV_DATA_DIR);
            let _c = EnvGuard::clear(ENV_MAX_CONCURRENCY);
            let _cp = EnvGuard::clear(config::ENV_CONFIG);
            let cfg = ServerConfig::from_env().expect("defaults resolve");
            assert_eq!(cfg.addr.to_string(), DEFAULT_ADDR);
            assert_eq!(cfg.static_dir, PathBuf::from(DEFAULT_STATIC_DIR));
            assert_eq!(cfg.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
            assert_eq!(cfg.max_concurrency, DEFAULT_MAX_CONCURRENCY);
            assert_eq!(cfg.config_path, PathBuf::from(config::DEFAULT_CONFIG_PATH));
        }

        // Env overrides every knob.
        {
            let _a = EnvGuard::set(ENV_ADDR, "0.0.0.0:9099");
            let _s = EnvGuard::set(ENV_STATIC_DIR, "/srv/spa");
            let _d = EnvGuard::set(ENV_DATA_DIR, "/srv/state");
            let _c = EnvGuard::set(ENV_MAX_CONCURRENCY, "5");
            let _cp = EnvGuard::set(config::ENV_CONFIG, "/srv/config.toml");
            let cfg = ServerConfig::from_env().expect("overrides resolve");
            assert_eq!(cfg.addr.to_string(), "0.0.0.0:9099");
            assert_eq!(cfg.static_dir, PathBuf::from("/srv/spa"));
            assert_eq!(cfg.data_dir, PathBuf::from("/srv/state"));
            assert_eq!(cfg.max_concurrency, 5);
            assert_eq!(cfg.config_path, PathBuf::from("/srv/config.toml"));
        }

        // A set-but-unparseable address is a hard error.
        {
            let _a = EnvGuard::set(ENV_ADDR, "not-an-addr");
            let err = ServerConfig::from_env().expect_err("invalid addr must error");
            assert!(matches!(err, ConfigError::BadAddr { .. }), "got {err:?}");
        }

        // A set-but-invalid (zero / non-numeric) concurrency is a hard error.
        {
            let _a = EnvGuard::clear(ENV_ADDR);
            let _c = EnvGuard::set(ENV_MAX_CONCURRENCY, "0");
            let err = ServerConfig::from_env().expect_err("zero concurrency must error");
            assert!(
                matches!(err, ConfigError::BadConcurrency { .. }),
                "got {err:?}"
            );
        }
    }
}
