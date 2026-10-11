//! Silicon Remind HTTP API process.

use silicon_remind::{api, config::Settings, logging};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env()?;
    logging::init(&settings)?;
    for name in silicon_remind::config::retired_variables_present() {
        tracing::warn!(
            variable = name,
            "ignoring a retired variable; Remind signs in with Silicon Accounts now"
        );
    }
    api::serve(settings).await
}
