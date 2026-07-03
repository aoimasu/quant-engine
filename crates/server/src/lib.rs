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

use axum::{http::StatusCode, routing::get, Json, Router};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

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

/// Environment variable naming the bind address (12-factor, `QE_`-prefixed like `qe-config`).
pub const ENV_ADDR: &str = "QE_SERVER_ADDR";

/// Environment variable naming the static-assets directory.
pub const ENV_STATIC_DIR: &str = "QE_SERVER_STATIC_DIR";

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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: DEFAULT_ADDR
                .parse()
                .expect("DEFAULT_ADDR is a valid socket address"),
            static_dir: PathBuf::from(DEFAULT_STATIC_DIR),
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
        Ok(cfg)
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

/// Build the application [`Router`] serving `static_dir` at `/`.
///
/// Layout:
/// - `/api/*` is a **nested** sub-router (health lives at `/api/health`); because the nest owns the
///   whole `/api` prefix, an unknown `/api/*` path returns `404` from the API router instead of being
///   swallowed by the SPA fallback — keeping `/api` a clean reserved namespace for later tickets.
/// - everything else is served from `static_dir` via `ServeDir`, with a per-request fallback to
///   `index.html` so client-side SPA routes still return the app shell. A missing dir/index yields a
///   graceful `404` (no panic), which is fine before QE-258 builds the real SPA.
pub fn build_router(static_dir: &Path) -> Router {
    let index = static_dir.join("index.html");
    let static_service = ServeDir::new(static_dir).fallback(ServeFile::new(index));

    let api = Router::new()
        .route("/health", get(health))
        .fallback(api_not_found);

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
        // Defaults when unset.
        {
            let _a = EnvGuard::clear(ENV_ADDR);
            let _s = EnvGuard::clear(ENV_STATIC_DIR);
            let cfg = ServerConfig::from_env().expect("defaults resolve");
            assert_eq!(cfg.addr.to_string(), DEFAULT_ADDR);
            assert_eq!(cfg.static_dir, PathBuf::from(DEFAULT_STATIC_DIR));
        }

        // Env overrides both knobs.
        {
            let _a = EnvGuard::set(ENV_ADDR, "0.0.0.0:9099");
            let _s = EnvGuard::set(ENV_STATIC_DIR, "/srv/spa");
            let cfg = ServerConfig::from_env().expect("overrides resolve");
            assert_eq!(cfg.addr.to_string(), "0.0.0.0:9099");
            assert_eq!(cfg.static_dir, PathBuf::from("/srv/spa"));
        }

        // A set-but-unparseable address is a hard error.
        {
            let _a = EnvGuard::set(ENV_ADDR, "not-an-addr");
            let err = ServerConfig::from_env().expect_err("invalid addr must error");
            assert!(matches!(err, ConfigError::BadAddr { .. }), "got {err:?}");
        }
    }
}
