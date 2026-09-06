//! Silicon Remind one-shot database migration process.

use silicon_remind::{config::MigrationSettings, infrastructure::postgres, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = MigrationSettings::from_env()?;
    telemetry::init_process(settings.environment, &settings.log_filter)?;

    let pool = postgres::connect(&settings.database).await?;
    postgres::migrate(&pool).await?;
    pool.close().await;
    if let Some(database) = &settings.testing_database {
        let pool = postgres::connect(database).await?;
        silicon_remind::infrastructure::testing::TestEnvironments::migrate(&pool).await?;
        pool.close().await;
    }
    Ok(())
}
