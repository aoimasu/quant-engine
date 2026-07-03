//! `qe-server` binary — boots the admin-UI backend (QE-254 scaffold).
//!
//! Resolves [`ServerConfig`] from the environment, installs telemetry, builds the router, and serves
//! it over TCP. Run lifecycle, auth, and read APIs land in later tickets (QE-255/256/257).

use std::process::ExitCode;

use qe_server::{build_router, ServerConfig};
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

    let router = build_router(&cfg.static_dir);

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
        "qe-server listening"
    );

    if let Err(e) = axum::serve(listener, router).await {
        tracing::error!(error = %e, "server error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
