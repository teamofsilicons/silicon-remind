//! Structured process telemetry configured without secret-bearing values.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::{RuntimeEnvironment, Settings};

/// Installs the global tracing subscriber for the API or worker process.
///
/// # Errors
///
/// Returns an error when the configured filter is invalid or another global
/// subscriber was installed first.
pub fn init(settings: &Settings) -> anyhow::Result<()> {
    init_process(settings.environment, &settings.log_filter)
}

/// Installs tracing for a process with intentionally minimal settings.
///
/// Development and test use compact human-readable output. Production emits
/// newline-delimited JSON suitable for centralized ingestion.
///
/// # Errors
///
/// Returns an error when the configured filter is invalid or another global
/// subscriber was installed first.
pub fn init_process(environment: RuntimeEnvironment, log_filter: &str) -> anyhow::Result<()> {
    let filter = build_filter(log_filter)?;
    let registry = tracing_subscriber::registry().with(filter);

    match environment {
        RuntimeEnvironment::Development | RuntimeEnvironment::Test => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(false),
            )
            .try_init()?,
        RuntimeEnvironment::Production => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_ansi(false)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .try_init()?,
    }

    Ok(())
}

fn build_filter(log_filter: &str) -> anyhow::Result<EnvFilter> {
    Ok(EnvFilter::try_new(log_filter)?)
}

#[cfg(test)]
mod tests {
    use super::build_filter;

    #[test]
    fn accepts_scoped_filter_directives() {
        assert!(build_filter("silicon_remind=debug,tower_http=info").is_ok());
    }

    #[test]
    fn rejects_malformed_filter_directives() {
        assert!(build_filter("silicon_remind[broken=debug").is_err());
    }
}
