//! `qe-server` binary — boots the admin-UI backend (QE-254 scaffold).
//!
//! Resolves [`ServerConfig`] from the environment, installs telemetry, builds the router, and serves
//! it over TCP. Run lifecycle, auth, and read APIs land in later tickets (QE-255/256/257).

use std::process::ExitCode;
use std::sync::Arc;

use qe_server::auth::{AuthConfig, AuthContext, IdTokenVerifier};
use qe_server::{build_router, AppState, ServerConfig, DEFAULT_SHUTDOWN_DRAIN};
use qe_telemetry::{init as init_telemetry, TelemetryConfig};

#[tokio::main]
async fn main() -> ExitCode {
    // Telemetry first so config/bind errors are structured-logged. A guard flushes on drop.
    let _telemetry = match init_telemetry(&TelemetryConfig::from_env()) {
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

    // QE-407 startup reconciler (widens QE-263): a hard kill leaves `running`/`queued` runs with no
    // supervisor and a `meta.json` that says `running` forever. Fail them before serving so the run
    // list is honest from the first request.
    match manager.reconcile_orphans() {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            reconciled = n,
            "failed orphaned runs left `running`/`queued` by a prior hard shutdown"
        ),
        Err(e) => tracing::error!(error = %e, "failed to reconcile orphaned runs on startup"),
    }

    // QE-257 read APIs: open the market store once and build the sealed-vintage repository. A failure
    // to open the store is fatal (mirrors the bind-failure path) — the read endpoints could not serve.
    let read = match cfg.read_state() {
        Ok(read) => read,
        Err(e) => {
            tracing::error!(error = %e, market_dir = %cfg.market_dir.display(), "failed to open market store");
            return ExitCode::FAILURE;
        }
    };

    // QE-256 auth: resolve OAuth + session config, then wire the ID-token verifier. The real Google
    // verifier is only available under the `http` feature; otherwise a disabled verifier keeps the
    // server bootable (health/static work) but login cannot complete.
    let auth_config = AuthConfig::from_env();

    // QE-409 (AR-9): the random ephemeral session-secret fallback is safe only on loopback. Refuse to
    // boot when bound to a non-loopback address without an explicit QE_SESSION_SECRET, rather than
    // silently guarding a network-exposed deployment with a restart-invalidated secret.
    if let Err(e) = qe_server::auth::check_session_secret_policy(
        &cfg.addr,
        auth_config.session_secret_is_ephemeral,
    ) {
        tracing::error!(error = %e, "refusing to boot");
        return ExitCode::FAILURE;
    }

    // QE-409 (advisory, non-fatal): bound off-loopback while session cookies are minted without
    // `Secure` (the `redirect_uri` is not https) — cookies could traverse the network unprotected.
    // Likely a misconfigured `redirect_uri` scheme or a TLS-terminating proxy the scheme doesn't
    // reflect. Warn only; unlike the missing-secret case this does NOT refuse boot.
    if qe_server::auth::should_warn_insecure_cookies(&cfg.addr, auth_config.cookie_secure) {
        tracing::warn!(
            addr = %cfg.addr,
            "bound to a non-loopback address but session cookies are not marked `Secure` \
             (redirect_uri is not https) — cookies may traverse the network unprotected; \
             check QE_OAUTH_REDIRECT_URI / TLS termination"
        );
    }

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

    // Keep a handle to the manager for the post-serve drain (QE-407); `AppState` takes ownership of one
    // `Arc` clone.
    let shutdown_manager = Arc::clone(&manager);
    let state = AppState::new(manager, auth, read);
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

    // QE-407: serve until a shutdown signal, then stop the listener and drain in-flight run
    // supervisors (terminally marking any that don't finish in time) so no `running` `meta.json`
    // survives a clean shutdown.
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    tracing::info!("shutdown signal received; draining in-flight runs");
    shutdown_manager.shutdown(DEFAULT_SHUTDOWN_DRAIN).await;

    if let Err(e) = serve_result {
        tracing::error!(error = %e, "server error");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Resolve when the process should begin graceful shutdown: a `Ctrl-C` (SIGINT) or, on unix, a
/// `SIGTERM` (the orchestrator/container stop signal). If a handler cannot be installed the branch is
/// disabled (a never-resolving future) rather than panicking — the workspace denies `unwrap`, and a
/// missing SIGTERM handler must not take down the SIGINT path.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler; relying on Ctrl-C only");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
