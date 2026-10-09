//! Silicon Remind durable scheduling and webhook delivery process.

use silicon_remind::{config::Settings, telemetry, worker};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env()?;
    telemetry::init(&settings)?;
    for name in silicon_remind::config::retired_variables_present() {
        tracing::warn!(
            variable = name,
            "ignoring a retired variable; Remind signs in with Silicon Accounts now"
        );
    }
    worker::run(settings).await
}
