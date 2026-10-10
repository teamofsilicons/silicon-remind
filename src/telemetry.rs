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
    init_with_writer(environment, log_filter, std::io::stdout)
}

/// Installs tracing like [`init_process`], but writes every log line to
/// standard error, for a command whose standard output is its result (the
/// `remind-migrate link-identities` JSON report).
///
/// # Errors
///
/// Returns an error when the configured filter is invalid or another global
/// subscriber was installed first.
pub fn init_command(environment: RuntimeEnvironment, log_filter: &str) -> anyhow::Result<()> {
    init_with_writer(environment, log_filter, std::io::stderr)
}

fn init_with_writer<W>(
    environment: RuntimeEnvironment,
    log_filter: &str,
    writer: W,
) -> anyhow::Result<()>
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    install_tls_provider();
    let filter = build_filter(log_filter)?;
    let registry = tracing_subscriber::registry().with(filter);

    match environment {
        RuntimeEnvironment::Development | RuntimeEnvironment::Test => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_writer(writer),
            )
            .try_init()?,
        RuntimeEnvironment::Production => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_ansi(false)
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_writer(writer),
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

/// Bounded event recorder using the official Space Station client in production.
#[derive(Clone)]
pub(crate) struct Recorder {
    enabled: bool,
    #[cfg(unix)]
    client: Option<std::sync::Arc<space_station::SpaceClient>>,
}
impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}
impl Recorder {
    pub(crate) fn new(settings: &Settings) -> Self {
        install_tls_provider();
        #[cfg(unix)]
        let client = if settings.telemetry_enabled {
            use secrecy::ExposeSecret as _;
            settings.telemetry_table_key.as_ref().and_then(|key| {
                space_station::SpaceClient::builder(key.expose_secret())
                    .url("https://backend.spacestation.teamofsilicons.com")
                    .home(&settings.telemetry_home)
                    .flush_timeout(std::time::Duration::from_millis(100))
                    .on_error(|_| {})
                    .build()
                    .ok()
                    .map(std::sync::Arc::new)
            })
        } else {
            None
        };
        Self {
            enabled: settings.telemetry_enabled,
            #[cfg(unix)]
            client,
        }
    }
    pub(crate) async fn record(
        &self,
        pool: &sqlx::PgPool,
        is_test: bool,
        event: serde_json::Value,
    ) {
        self.record_with_budget(pool, is_test, event, std::time::Duration::from_millis(100))
            .await;
    }

    async fn record_with_budget(
        &self,
        pool: &sqlx::PgPool,
        is_test: bool,
        mut event: serde_json::Value,
        budget: std::time::Duration,
    ) {
        if !self.enabled {
            return;
        }
        event["application"] = "remind".into();
        event["service_version"] = env!("CARGO_PKG_VERSION").into();
        event["environment"] = if is_test { "testing" } else { "production" }.into();
        event["occurred_at"] = chrono::Utc::now().to_rfc3339().into();
        if is_test {
            // Never hand sandbox events to the production daemon or its disk spool.
            let _ = tokio::time::timeout(budget, async {
                sqlx::query("INSERT INTO telemetry_events(id,event) VALUES($1,$2)")
                    .bind(uuid::Uuid::now_v7()).bind(event).execute(pool).await?;
                sqlx::query("DELETE FROM telemetry_events WHERE id IN (SELECT id FROM telemetry_events ORDER BY recorded_at DESC OFFSET 10000)").execute(pool).await?;
                Ok::<(),sqlx::Error>(())
            }).await;
        } else {
            #[cfg(unix)]
            if let Some(client) = &self.client {
                client.record(event);
            }
        }
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    #[tokio::test]
    async fn sandbox_events_are_local_and_opt_out_writes_nothing() -> anyhow::Result<()> {
        let container = crate::test_support::TestPostgres::start().await?;
        let pool = sqlx::PgPool::connect(&container.url).await?;
        crate::infrastructure::postgres::migrate(&pool).await?;
        let mut recorder = Recorder {
            enabled: true,
            #[cfg(unix)]
            client: None,
        };
        // This test verifies routing and opt-out, not the deliberately lossy
        // production latency budget. Loaded CI must still observe the fixture.
        let budget = std::time::Duration::from_secs(5);
        recorder
            .record_with_budget(
                &pool,
                true,
                serde_json::json!({"source":"cli","event":"command_completed"}),
                budget,
            )
            .await;
        let event: serde_json::Value = sqlx::query_scalar("SELECT event FROM telemetry_events")
            .fetch_one(&pool)
            .await?;
        assert_eq!(event["environment"], "testing");
        recorder.enabled = false;
        recorder
            .record_with_budget(
                &pool,
                true,
                serde_json::json!({"event":"should_not_exist"}),
                budget,
            )
            .await;
        recorder.enabled = true;
        recorder
            .record_with_budget(
                &pool,
                false,
                serde_json::json!({"event":"production_should_not_write_locally"}),
                budget,
            )
            .await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM telemetry_events")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);
        Ok(())
    }
}

// SQLx enables ring while the HTTP clients also enable aws-lc. Select a process
// default before Space Station creates a TLS connector on its sender thread.
fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
#[cfg(test)]
mod tls_tests {
    #[test]
    fn tls_configuration_works_with_both_dependency_providers_enabled() {
        super::install_tls_provider();
        let _ = rustls::ClientConfig::builder();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
