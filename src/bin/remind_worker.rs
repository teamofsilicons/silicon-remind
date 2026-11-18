//! Silicon Remind durable scheduling and webhook delivery process.

use clap::Parser;
use silicon_remind::{config::Settings, logging, worker};

#[derive(Parser)]
struct Options {
    /// Register the Remind notification type at Ting, then exit.
    #[arg(long)]
    register_ting: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = Options::parse();
    let _ = dotenvy::dotenv();
    let settings = Settings::from_env()?;
    logging::init(&settings)?;
    for name in silicon_remind::config::retired_variables_present() {
        tracing::warn!(
            variable = name,
            "ignoring a retired variable; Remind signs in with Silicon Accounts now"
        );
    }
    if options.register_ting {
        return worker::register_ting(&settings).await;
    }
    worker::run(settings).await
}
