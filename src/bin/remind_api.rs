//! Silicon Remind HTTP API process.

use silicon_remind::{api, config::Settings, telemetry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env()?;
    telemetry::init(&settings)?;
    api::serve(settings).await
}
