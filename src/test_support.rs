//! Test-only PostgreSQL provisioning shared by every database-backed test.
//!
//! When `REMIND_TEST_POSTGRES_URL` names an administrative connection (for
//! example `postgres://postgres@127.0.0.1:5460/postgres`), each test receives a
//! freshly created, uniquely named `remind_t_<uuid>` database that is dropped
//! again when the handle goes out of scope. Without the variable the tests keep
//! their original behaviour and start a disposable `postgres:17-alpine`
//! container through testcontainers (the CI path).

use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
use testcontainers_modules::postgres::Postgres;

/// Environment variable naming an administrative PostgreSQL URL for tests.
pub(crate) const TEST_POSTGRES_URL: &str = "REMIND_TEST_POSTGRES_URL";

/// One isolated database for one test.
pub(crate) struct TestPostgres {
    /// Connection URL for the isolated database.
    pub(crate) url: String,
    provisioned: Option<(String, String)>,
    _container: Option<ContainerAsync<Postgres>>,
}

impl std::fmt::Debug for TestPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestPostgres")
            .field("database", &self.provisioned.as_ref().map(|(_, name)| name))
            .finish_non_exhaustive()
    }
}

impl TestPostgres {
    /// Creates an isolated database on the configured server or in a container.
    ///
    /// # Errors
    ///
    /// Returns connection, provisioning, or container start failures.
    pub(crate) async fn start() -> anyhow::Result<Self> {
        if let Ok(admin_url) = std::env::var(TEST_POSTGRES_URL) {
            let name = format!("remind_t_{}", uuid::Uuid::now_v7().simple());
            let admin = sqlx::PgPool::connect(&admin_url).await?;
            // The name is generated here from a UUID, never from input.
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&admin)
                .await?;
            admin.close().await;
            let mut url = url::Url::parse(&admin_url)?;
            url.set_path(&format!("/{name}"));
            return Ok(Self {
                url: url.to_string(),
                provisioned: Some((admin_url, name)),
                _container: None,
            });
        }
        let container = Postgres::default().with_tag("17-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            container.get_host().await?,
            container.get_host_port_ipv4(5432).await?
        );
        Ok(Self {
            url,
            provisioned: None,
            _container: Some(container),
        })
    }

    /// Connects a small pool to the isolated database.
    ///
    /// # Errors
    ///
    /// Returns connection failures.
    pub(crate) async fn pool(&self, max_connections: u32) -> anyhow::Result<sqlx::PgPool> {
        Ok(sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(&self.url)
            .await?)
    }
}

impl Drop for TestPostgres {
    fn drop(&mut self) {
        let Some((admin_url, name)) = self.provisioned.take() else {
            return;
        };
        // Drop runs inside the test's runtime, so cleanup gets its own thread
        // and current-thread runtime. FORCE ends pools the test left open.
        let _ = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                if let Ok(admin) = sqlx::PgPool::connect(&admin_url).await {
                    let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
                    )))
                    .execute(&admin)
                    .await;
                    admin.close().await;
                }
            });
        })
        .join();
    }
}

/// Accounts settings for tests pointing at `url` (a local stub or the local stack).
///
/// # Errors
///
/// Returns an error for an unparsable URL.
pub(crate) fn accounts_settings(url: &str) -> anyhow::Result<crate::config::AccountsSettings> {
    let url: url::Url = url.parse()?;
    Ok(crate::config::AccountsSettings {
        url: url.clone(),
        api_url: url,
        app_id: "remind".to_owned(),
        app_secret: secrecy::SecretString::from("sa_app_remind_test_secret"),
        webhook_secrets: vec![secrecy::SecretString::from(TEST_WEBHOOK_SECRET)],
        request_timeout: std::time::Duration::from_secs(2),
        lookup_ttl: std::time::Duration::from_mins(15),
        proof_issuers: crate::config::ProofIssuers::parse("remind.schedules.read=interface")
            .map_err(|reason| anyhow::anyhow!(reason))?,
    })
}

/// Webhook secret used by [`accounts_settings`].
pub(crate) const TEST_WEBHOOK_SECRET: &str = "whsec_remind_test_webhook_secret";

/// An identity store over `pool` whose Silicon Accounts calls go to `url`.
///
/// # Errors
///
/// Returns an error for an unparsable URL.
pub(crate) fn identity_store(
    pool: sqlx::PgPool,
    url: &str,
) -> anyhow::Result<crate::infrastructure::identity::IdentityStore> {
    let settings = accounts_settings(url)?;
    Ok(crate::infrastructure::identity::IdentityStore::new(
        pool,
        crate::infrastructure::accounts::AccountsGateway::new(&settings)?,
        settings.lookup_ttl,
    ))
}
