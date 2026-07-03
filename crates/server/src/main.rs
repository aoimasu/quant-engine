//! `qe-server` binary — boots the admin-UI backend (QE-254 scaffold).
//!
//! Resolves [`ServerConfig`] from the environment, installs telemetry, builds the router, and serves
//! it over TCP. Run lifecycle, auth, and read APIs land in later tickets (QE-255/256/257).

use std::process::ExitCode;
use std::sync::Arc;

use qe_server::auth::{AuthConfig, AuthContext, IdTokenVerifier};
use qe_server::{build_router, AppState, ServerConfig};
use qe_telemetry::{init as init_telemetry, TelemetryConfig};

#[tokio::main]
async fn main() -> ExitCode {
    // Telemetry first so config/bind errors are structured-logged. A guard flushes on drop.
    let _telemetry = match init_telemetry(&TelemetryConfig::default()) {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("failed to install telemetry: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cfg = match ServerConfig::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(error = %e, "invalid server configuration");
            return ExitCode::FAILURE;
        }
    };

    let manager = cfg.run_manager();

    // QE-256 auth: resolve OAuth + session config, then wire the ID-token verifier. The real Google
    // verifier is only available under the `http` feature; otherwise a disabled verifier keeps the
    // server bootable (health/static work) but login cannot complete.
    let auth_config = AuthConfig::from_env();
    #[cfg(feature = "http")]
    let verifier: Arc<dyn IdTokenVerifier> = Arc::new(
        qe_server::auth::google::GoogleOidcVerifier::new(&auth_config),
    );
    #[cfg(not(feature = "http"))]
    let verifier: Arc<dyn IdTokenVerifier> = Arc::new(qe_server::auth::DisabledVerifier);
    if auth_config.allowed_emails.is_empty() {
        tracing::warn!(
            "QE_ADMIN_ALLOWED_EMAILS is empty — the allowlist fails closed (nobody can sign in)"
        );
    }
    let auth = Arc::new(AuthContext::new(auth_config, verifier));

    let state = AppState::new(manager, auth);
    let router = build_router(&cfg.static_dir, state);

    let listener = match tokio::net::TcpListener::bind(cfg.addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(error = %e, addr = %cfg.addr, "failed to bind");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        addr = %cfg.addr,
        static_dir = %cfg.static_dir.display(),
        data_dir = %cfg.data_dir.display(),
        cli_bin = %cfg.cli_bin.display(),
        max_concurrency = cfg.max_concurrency,
        "qe-server listening"
    );

    if let Err(e) = axum::serve(listener, router).await {
        tracing::error!(error = %e, "server error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
