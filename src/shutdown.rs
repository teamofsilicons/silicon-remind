//! Cross-platform graceful-shutdown coordination.

/// Waits for an interrupt or termination request and then resolves.
///
/// Unix processes observe both `SIGINT` and `SIGTERM`. Other platforms observe
/// the runtime's cross-platform Ctrl+C notification. Signal-handler
/// installation failures are logged and resolve the future so the process does
/// not continue running without a functioning shutdown path.
pub async fn signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to install interrupt signal handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(error = %error, "failed to install termination signal handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
