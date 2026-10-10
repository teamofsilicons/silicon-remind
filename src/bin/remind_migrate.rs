//! Silicon Remind one-shot migration and identity-linking process.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use silicon_remind::{
    config::{MigrationSettings, RuntimeEnvironment, lookup_settings_from_env},
    infrastructure::{accounts::AccountsGateway, identity_links, postgres},
    telemetry,
};

/// Applies Remind's database migrations, or links rows from before Silicon
/// Accounts to the accounts their IAM principals became.
#[derive(Parser)]
#[command(name = "remind-migrate", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Applies the embedded migrations to the production database and, when
    /// `REMIND_TEST_MIGRATOR_DATABASE_URL` is set, to the testing database and
    /// every test-environment schema. This is the default.
    Migrate,
    /// Links legacy IAM principals to Silicon Accounts accounts from a CSV of
    /// `iam_principal_id,accounts_uuid` lines, in one transaction per database.
    /// With `REMIND_APP_SECRET` set, every account is checked with Silicon
    /// Accounts first. Prints a JSON report; any refused line rolls back.
    LinkIdentities {
        /// The mapping file.
        #[arg(long)]
        file: PathBuf,
        /// Report what would change, then roll everything back.
        #[arg(long)]
        dry_run: bool,
        /// Label recorded on each link (defaults to the file name).
        #[arg(long)]
        source: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let settings = MigrationSettings::from_env()?;
    // Logs go to standard error: standard output carries link-identities' JSON report.
    telemetry::init_command(settings.environment, &settings.log_filter)?;
    match cli.command.unwrap_or(Command::Migrate) {
        Command::Migrate => {
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
        Command::LinkIdentities {
            file,
            dry_run,
            source,
        } => link(&settings, &file, dry_run, source).await,
    }
}

async fn link(
    settings: &MigrationSettings,
    file: &std::path::Path,
    dry_run: bool,
    source: Option<String>,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", file.display()))?;
    let entries = identity_links::parse_mapping(&text).map_err(|errors| {
        anyhow::anyhow!(
            "{} has malformed lines; nothing was changed:\n{}",
            file.display(),
            errors.join("\n")
        )
    })?;
    let source = source.unwrap_or_else(|| {
        format!(
            "link-identities:{}",
            file.file_name()
                .map_or_else(|| "mapping".into(), |name| name.to_string_lossy())
        )
    });
    let gateway = lookup_settings_from_env(settings.environment)?
        .map(|accounts| AccountsGateway::new(&accounts))
        .transpose()?;
    if gateway.is_none() {
        anyhow::ensure!(
            dry_run || settings.environment != RuntimeEnvironment::Production,
            "REMIND_APP_SECRET is required to apply identity links in production; verify the destination accounts before changing ownership"
        );
        tracing::warn!(
            "REMIND_APP_SECRET is not set: linking offline, without checking accounts with Silicon Accounts"
        );
    }
    let pool = postgres::connect(&settings.database).await?;
    let testing = match &settings.testing_database {
        Some(database) => Some(postgres::connect(database).await?),
        None => None,
    };
    let report = identity_links::link_identities(
        &pool,
        testing.as_ref(),
        gateway.as_ref(),
        &entries,
        &source,
        dry_run,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    pool.close().await;
    if let Some(testing) = testing {
        testing.close().await;
    }
    if !report.refused.is_empty() {
        anyhow::bail!(
            "{} line(s) were refused, so nothing was committed; fix them and run again",
            report.refused.len()
        );
    }
    Ok(())
}
