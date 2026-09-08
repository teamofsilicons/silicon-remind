//! Silicon Remind durable scheduling and webhook delivery process.

use silicon_remind::{config::Settings, telemetry, worker};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env()?;
    telemetry::init(&settings)?;
    worker::run(settings).await
}
