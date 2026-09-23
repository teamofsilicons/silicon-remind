use clap::{Args, Parser, Subcommand, ValueEnum};
use uuid::Uuid;

const TIMEZONE_GUIDANCE: &str = "Timezone is mandatory when creating a reminder. Provide an IANA timezone identifier, for example --timezone Asia/Kolkata or --timezone UTC.";

fn nonblank_timezone(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        Err(TIMEZONE_GUIDANCE)
    } else {
        Ok(value.to_owned())
    }
}

pub fn with_timezone_guidance(mut error: clap::Error) -> clap::Error {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if matches!(
        error.kind(),
        ErrorKind::MissingRequiredArgument | ErrorKind::InvalidValue
    ) {
        let timezone_missing = match error.get(ContextKind::InvalidArg) {
            Some(ContextValue::String(arg)) => arg == "--timezone <IANA_TIMEZONE>",
            Some(ContextValue::Strings(args)) => {
                args.iter().any(|arg| arg == "--timezone <IANA_TIMEZONE>")
            }
            _ => false,
        };
        if timezone_missing {
            error.insert(
                ContextKind::Suggested,
                ContextValue::StyledStrs(vec![TIMEZONE_GUIDANCE.into()]),
            );
        }
    }
    error
}

#[derive(Parser)]
#[command(
    name = "remind",
    version,
    about = "Create and manage Silicon reminders through Silicon IAM.",
    long_about = "Silicon Remind schedules one-time or recurring reminders using five-field Linux cron. Silicons manage their own reminders; Carbons and Silicons can read their organization's reminders.\n\nStart with: remind login <slt>\nOptionally subscribe a receiver: remind webhook subscribe <url>\nCreate a reminder: remind create --text 'Check the build' --cron '*/5 * * * *' --timezone Asia/Kolkata\nUse any command in a saved sandbox: remind --test <test_id> <command>.",
    after_help = "Authentication:\n  remind iam --json                 Discover the IAM app_id before obtaining an SLT\n  remind login <slt>                Exchange your IAM short-lived token\n  remind login status --json        Verify the saved Carbon or Silicon identity\n\nLocal state defaults to $SILICON_HOME/.remind when SILICON_HOME is set, otherwise ~/.remind. Use remind config home <directory> to select an existing directory.\n\nRun remind <command> --help for command-specific options and examples.\nManuals: remind docs <topic>\nDocs: https://docs.remind.teamofsilicons.com\nSource: https://github.com/teamofsilicons/silicon-remind\nRust: https://crates.io/crates/silicon-remind-client",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Override the server origin for this invocation (credentials stay scoped to that origin).
    #[arg(long, global = true, env = "REMIND_URL")]
    pub url: Option<String>,
    /// Select the organization for this invocation.
    #[arg(long, global = true, env = "REMIND_ORG")]
    pub org: Option<String>,
    /// Run the same command in a saved test environment, using its isolated session.
    #[arg(long, global = true)]
    pub test: Option<Uuid>,
    /// Use production for this command without changing the saved sandbox selection.
    #[arg(long, global = true, conflicts_with = "test")]
    pub production: bool,
    /// Print machine-readable JSON; suggestions go to stderr only in human mode.
    #[arg(long, global = true)]
    pub json: bool,
    /// Skip this invocation's automatic registry check.
    #[arg(long, global = true)]
    pub no_update: bool,
    /// Reuse a mutation key when retrying the exact same create, edit or status operation.
    #[arg(long, global = true)]
    pub idempotency_key: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Subcommand)]
pub enum Command {
    /// Read the complete bundled usage and development manuals offline.
    #[command(
        after_help = "Examples: remind docs cli; remind docs testing; remind docs api. Online: https://docs.remind.teamofsilicons.com"
    )]
    Docs {
        #[arg(default_value = "cli", value_parser = ["cli", "api", "client", "testing", "webhooks", "releases"])]
        topic: String,
    },
    /// Submit a bug report by email through Remind; optionally link a pull request.
    #[command(
        after_help = "Example: remind report 'Steps, expected result, actual result' --pr https://github.com/teamofsilicons/silicon-remind/pull/123\nUses your Remind session to queue a Postmark email. Sandbox reports simulate delivery. Do not include secrets."
    )]
    Report {
        message: String,
        #[arg(long)]
        pr: Option<String>,
    },
    /// Check whether your queued bug report was sent, failed, or simulated.
    ReportStatus { id: uuid::Uuid },
    /// Inspect or remove the retired updater service; updates are managed by Honeycomb.
    Daemon {
        #[command(subcommand)]
        command: Daemon,
    },
    /// Log in with an IAM short-lived token, or inspect live authentication status.
    #[command(args_conflicts_with_subcommands = true, arg_required_else_help = true)]
    Login {
        #[arg(value_name = "SLT")]
        slt: Option<String>,
        #[command(subcommand)]
        command: Option<Login>,
    },
    /// Show public IAM application information, including the app_id needed for an SLT.
    Iam,
    /// Log in using an IAM short-lived token; inspect or revoke the current session.
    Auth {
        #[command(subcommand)]
        command: Auth,
    },
    /// Create a reminder owned by the signed-in Silicon.
    #[command(
        after_help = "Examples:\n  remind create --text 'Daily standup' --cron '0 9 * * MON-FRI' --timezone Asia/Kolkata\n  remind create --text 'One reminder' --cron '30 16 * * *' --kind one-time --timezone UTC\n\nTimezone is mandatory for both recurring and one-time reminders. Use an IANA timezone identifier, such as Asia/Kolkata or UTC.\nAdd optional receivers with remind webhook subscribe <url>."
    )]
    Create {
        #[arg(long)]
        text: String,
        #[arg(long)]
        cron: String,
        /// Mandatory IANA timezone identifier, for example Asia/Kolkata or UTC.
        #[arg(long, value_name = "IANA_TIMEZONE", value_parser = nonblank_timezone)]
        timezone: String,
        #[arg(long, value_enum, default_value = "recurring")]
        kind: Kind,
    },
    /// List current reminders; use --archived for the 45-day archive.
    List {
        #[arg(long)]
        silicon: Option<String>,
        #[arg(long)]
        archived: bool,
        #[arg(long, value_enum)]
        status: Option<Status>,
        #[command(flatten)]
        page: Page,
    },
    /// Show one reminder, including its next trigger and archive deadline.
    Get { id: Uuid },
    /// Change reminder text, cron, timezone or kind. Archived reminders are immutable.
    Edit {
        id: Uuid,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<Kind>,
    },
    /// Atomically pause one or up to 100 owned reminders without archiving them.
    Pause {
        #[arg(required=true,num_args=1..=100)]
        ids: Vec<Uuid>,
    },
    /// Atomically resume owned reminders and calculate their next future occurrences.
    Resume {
        #[arg(required=true,num_args=1..=100)]
        ids: Vec<Uuid>,
    },
    /// Archive one owned reminder. It remains readable for 45 days.
    Archive { id: Uuid },
    /// Inspect delivery attempts, failures and webhook ingress receipt IDs.
    Executions {
        id: Uuid,
        #[command(flatten)]
        page: Page,
    },
    /// List registered Silicons in the current organization.
    Silicons {
        #[arg(long)]
        after: Option<Uuid>,
        #[arg(long,default_value_t=50,value_parser=clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Add, inspect or remove the signed-in Silicon's webhook subscriptions.
    Webhook {
        #[command(subcommand)]
        command: Webhook,
    },
    /// Manage organization-owned test environments using the production session.
    Env {
        #[command(subcommand)]
        command: Environment,
    },
    /// Inspect the selected sandbox; requires --test <id>.
    TestInfo,
    /// Configure this sandbox's test-only IAM Application secret (requires --test).
    ConfigureIam {
        #[arg(long)]
        iam_app_secret_file: Option<std::path::PathBuf>,
    },
    /// Erase all data in the selected sandbox while retaining its key and IAM binding.
    #[command(
        after_help = "Requires --test <id>. This erases reminders, delivery history, webhook configuration and sandbox logs. It does not clean the linked IAM environment."
    )]
    Clean,
    /// Show or set local server and updater preferences.
    Config {
        #[command(subcommand)]
        command: Config,
    },
    /// Show Honeycomb update instructions. Never replaces this executable.
    Update {
        #[arg(long)]
        check: bool,
    },
    /// Probe API liveness or database readiness.
    Health {
        #[arg(long)]
        ready: bool,
    },
}
#[derive(Subcommand)]
pub enum Login {
    /// Verify the saved session with IAM and report the Carbon or Silicon identity.
    Status,
}
#[derive(Subcommand)]
pub enum Auth {
    /// Exchange an IAM SLT; prompt securely, or read it from standard input.
    Login {
        #[arg(long)]
        slt_stdin: bool,
    },
    /// Show current live IAM identity and organization permissions.
    Whoami,
    /// Explicitly rotate the current refresh token.
    Refresh,
    /// Revoke the IAM session, then remove the saved credentials.
    Logout,
}
#[derive(Subcommand)]
pub enum Webhook {
    /// Register any HTTP(S) URL and optional HMAC signing secret.
    Set {
        #[arg(value_name = "URL")]
        endpoint_url: String,
        #[arg(long)]
        secret_stdin: bool,
        /// Send without an HMAC signing secret.
        #[arg(long, conflicts_with = "secret_stdin")]
        unsigned: bool,
    },
    /// Add another independent webhook subscription.
    Subscribe {
        #[arg(value_name = "URL")]
        endpoint_url: String,
        #[arg(long)]
        secret_stdin: bool,
        /// Send without an HMAC signing secret.
        #[arg(long, conflicts_with = "secret_stdin")]
        unsigned: bool,
    },
    /// Show the configured endpoint; the signing secret is never returned.
    Get,
    /// List every active webhook subscription.
    List,
    /// Disable one subscription by its UUID.
    Unsubscribe { id: Uuid },
    /// Disable delivery configuration until it is set again.
    Disable,
}
#[derive(Subcommand)]
pub enum Environment {
    /// Select a sandbox with its IAM app_secret. Environment and name are discovered automatically.
    #[command(
        after_help = "Example: remind env use --secret-stdin < /private/app-secret\nThen: remind login <test-public-id>; remind list --json. Exit with remind env exit."
    )]
    Use {
        #[arg(long)]
        secret_stdin: bool,
    },
    /// Return to the separately saved production session.
    Exit,
    /// Create an empty Remind environment bound to an existing IAM test environment.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        iam_key_file: Option<std::path::PathBuf>,
        #[arg(long)]
        iam_app_secret_file: Option<std::path::PathBuf>,
    },
    /// List active environments, optionally including recoverable deleted ones.
    List {
        #[arg(long)]
        include_deleted: bool,
        #[arg(long)]
        after: Option<Uuid>,
        #[arg(long,default_value_t=50,value_parser=clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Inspect one environment's metadata and retention deadlines.
    Get { id: Uuid },
    /// Retrieve and print an environment key; also save it locally.
    Key { id: Uuid },
    /// Rotate an environment key and save the successor locally.
    Rotate { id: Uuid },
    /// Retire an environment; recover it within 30 days using env restore.
    Delete { id: Uuid },
    /// Restore a retired environment with a fresh root key.
    Restore { id: Uuid },
    /// Save a shared root key locally; no production session is needed.
    Import {
        id: Uuid,
        #[arg(long)]
        key_stdin: bool,
    },
    /// Forget a saved key and session on this computer; leaves the server untouched.
    Forget { id: Uuid },
}
#[derive(Subcommand)]
pub enum Config {
    Show,
    SetUrl {
        #[arg(value_name = "URL")]
        service_url: String,
    },
    /// Set the parent directory used for the local Remind state directory.
    Home {
        #[arg(value_name = "DIRECTORY")]
        location: std::path::PathBuf,
    },
    /// Enable or disable operational telemetry for CLI and daemon requests.
    Telemetry {
        #[arg(value_enum)]
        value: Toggle,
    },
    AutoUpdate {
        #[arg(value_enum)]
        value: Toggle,
    },
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    OneTime,
    Recurring,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Status {
    Active,
    Paused,
    Completed,
}
#[derive(Args)]
pub struct Page {
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long,default_value_t=50,value_parser=clap::value_parser!(u32).range(1..=100))]
    limit: u32,
}
impl Page {
    pub fn cursor(&self) -> Option<String> {
        self.cursor.clone()
    }
    pub fn limit(&self) -> u32 {
        self.limit
    }
}
impl From<Kind> for silicon_remind_client::models::ScheduleKind {
    fn from(v: Kind) -> Self {
        match v {
            Kind::OneTime => Self::OneTime,
            Kind::Recurring => Self::Recurring,
        }
    }
}
impl From<Status> for silicon_remind_client::models::ScheduleStatus {
    fn from(v: Status) -> Self {
        match v {
            Status::Active => Self::Active,
            Status::Paused => Self::Paused,
            Status::Completed => Self::Completed,
        }
    }
}

#[derive(Subcommand)]
pub enum Daemon {
    /// Retired: use Honeycomb to manage updates.
    Install,
    /// Stop and remove the installed user service.
    Uninstall,
    /// Inspect the operating system service state.
    Status,
    /// Retired: use `honeycomb update 'remind'`.
    Run,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, with_timezone_guidance};
    use clap::Parser as _;

    #[test]
    fn create_rejects_missing_or_blank_timezone_with_actionable_guidance() {
        for kind in ["recurring", "one-time"] {
            for timezone_args in [
                vec![],
                vec!["--timezone"],
                vec!["--timezone", ""],
                vec!["--timezone", "   "],
            ] {
                let mut args = vec![
                    "remind",
                    "create",
                    "--text",
                    "Check the build",
                    "--cron",
                    "0 9 * * *",
                    "--kind",
                    kind,
                ];
                args.extend(timezone_args);
                let Err(error) = Cli::try_parse_from(args).map_err(with_timezone_guidance) else {
                    panic!("create must reject a missing or blank timezone");
                };
                assert_eq!(error.exit_code(), 2);
                let message = error.to_string();
                assert!(message.contains("Timezone is mandatory"), "{message}");
                assert!(message.contains("IANA timezone identifier"), "{message}");
                assert!(message.contains("--timezone Asia/Kolkata"), "{message}");
            }
        }
    }

    #[test]
    fn create_preserves_explicit_timezone_for_both_schedule_kinds() -> anyhow::Result<()> {
        for kind in ["recurring", "one-time"] {
            for supplied_timezone in ["Asia/Kolkata", "UTC"] {
                let cli = Cli::try_parse_from([
                    "remind",
                    "create",
                    "--text",
                    "Check the build",
                    "--cron",
                    "0 9 * * *",
                    "--kind",
                    kind,
                    "--timezone",
                    supplied_timezone,
                ])?;
                let Command::Create { timezone, .. } = cli.command else {
                    panic!("expected create command");
                };
                assert_eq!(timezone, supplied_timezone);
            }
        }
        Ok(())
    }

    #[test]
    fn edit_without_timezone_leaves_it_unchanged() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "remind",
            "edit",
            "00000000-0000-4000-8000-000000000001",
            "--text",
            "Updated reminder",
        ])?;
        let Command::Edit { timezone, .. } = cli.command else {
            panic!("expected edit command");
        };
        assert!(timezone.is_none());
        Ok(())
    }
}
