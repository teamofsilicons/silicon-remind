//! The `remind` command tree. Every command says what it is for, how it combines with the
//! others, and its flags; `remind <command> --help` shows examples.
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

/// Adds the timezone guidance to clap's error when `--timezone` is missing or blank.
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

const ABOUT: &str = "Durable reminders for Silicons: one-time or recurring, on five-field cron, in any IANA timezone.";
const LONG_ABOUT: &str = "\
Silicon Remind keeps a Silicon's reminders and, when one comes due, posts it to every webhook \
subscription the Silicon set up. A Silicon creates and changes its own reminders. The Carbon who \
looks after a Silicon (its custodian) and the custodian's other Silicons can read them, and so can \
any account the Silicon or its custodian shares them with. Carbons read; they never write reminders.

Sign in with Silicon Accounts first:
  Carbon:   remind login              shows a code; approve it on the account site
  Silicon:  silicon-accounts login --app remind -q | remind login --slt-stdin

Then, as a Silicon:
  remind webhook subscribe https://hook.example/remind     optional: where due reminders go
  remind create --text 'Check the build' --cron '*/30 * * * *' --timezone Asia/Kolkata
  remind list

As a Carbon:
  remind silicons                     the Silicons whose reminders you can read
  remind list --silicon si:scout      one Silicon's reminders

Run any command inside a test environment with: remind --test <test_id> <command>.";

const AFTER_HELP: &str = "\
Signing in:
  remind accounts --json      app id and Silicon Accounts origin (no sign-in needed)
  remind login                Carbon: prints a code to approve on the account site, then waits
  remind login --slt-stdin    Silicon: takes a token from silicon-accounts login --app remind -q
  remind login status --json  who is signed in; {\"authenticated\":false} when nobody is
  remind logout               ends the sign-in at Silicon Accounts and forgets it here

Local state lives in $SILICON_HOME/.remind when SILICON_HOME is set, otherwise ~/.remind
(`remind config home <directory>` moves it). Silicon Apps installs Remind and keeps it up to
date: silicon-apps install remind.

Exit status: 0 ok, 1 failed, 2 invalid input, 3 not signed in or sign-in refused (HTTP 401),
4 not allowed (HTTP 403), 130 interrupted.

Run `remind <command> --help` for that command's flags and examples.
Manuals: remind docs <topic>, or https://docs.remind.teamofsilicons.com
Source: https://github.com/teamofsilicons/silicon-remind
Rust client: https://crates.io/crates/silicon-remind-client";

const GLOBAL: &str = "Global options";

/// The whole command line.
#[derive(Parser)]
#[command(
    name = "remind",
    version,
    about = ABOUT,
    long_about = LONG_ABOUT,
    after_help = AFTER_HELP,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Remind API origin for this command (default: the one saved with `remind config set-url`, else https://api.remind.teamofsilicons.com). Sign-ins are kept per origin.
    #[arg(long, global = true, env = "REMIND_URL", value_name = "URL", help_heading = GLOBAL)]
    pub url: Option<String>,
    /// Silicon Accounts origin to sign in with (default: the one saved with `remind config set-accounts-url`, else https://accounts.teamofsilicons.com).
    #[arg(long, global = true, env = "ACCOUNTS_URL", value_name = "URL", help_heading = GLOBAL)]
    pub accounts_url: Option<String>,
    /// Run this command inside a test environment whose key is saved here (remind env key|import <id>).
    #[arg(long, global = true, value_name = "TEST_ID", help_heading = GLOBAL)]
    pub test: Option<Uuid>,
    /// Run this command in production even when a test environment is selected (remind env use).
    #[arg(long, global = true, conflicts_with = "test", help_heading = GLOBAL)]
    pub production: bool,
    /// Print one JSON object on stdout; progress and warnings go to stderr as JSON lines.
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub json: bool,
    /// Reuse this key when retrying the exact same create, edit, pause, resume or report.
    #[arg(long, global = true, value_name = "KEY", help_heading = GLOBAL)]
    pub idempotency_key: Option<String>,
    /// Accepted and ignored: Silicon Apps keeps Remind up to date.
    #[arg(long, global = true, hide = true)]
    pub no_update: bool,
    #[command(subcommand)]
    pub command: Command,
}

/// `remind login`: sign-in options. With no option, a Carbon device sign-in.
#[derive(Args)]
pub struct LoginArgs {
    /// A short-lived token as the only argument; the same as --slt (the Silicon runtime signs in this way).
    #[arg(value_name = "SLT", conflicts_with_all = ["slt", "slt_stdin"])]
    pub token: Option<String>,
    /// Silicon sign-in: the short-lived token (slt_…) from `silicon-accounts login --app remind -q`. Prefer --slt-stdin, which keeps it out of the process list.
    #[arg(long, value_name = "TOKEN", conflicts_with = "slt_stdin")]
    pub slt: Option<String>,
    /// Silicon sign-in: read the short-lived token from standard input.
    #[arg(long)]
    pub slt_stdin: bool,
    /// Carbon sign-in: also open the approval page in the default browser.
    #[arg(long, conflicts_with_all = ["token", "slt", "slt_stdin"])]
    pub open: bool,
    /// Carbon sign-in: how this device appears in your list of sign-ins (default: remind on <host>).
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["token", "slt", "slt_stdin"])]
    pub label: Option<String>,
    /// Carbon sign-in: sign in again even when this home is already signed in.
    #[arg(long)]
    pub force: bool,
    #[command(subcommand)]
    pub command: Option<LoginCommand>,
}

/// `remind login status`.
#[derive(Subcommand)]
pub enum LoginCommand {
    /// Report who is signed in to Remind on this machine; with --json always exits 0.
    #[command(
        long_about = "Report who is signed in to Remind on this machine. The access token is refreshed when less than a minute is left, then checked with Remind, so `verified` is true when Remind accepted it just now. --offline reads only the saved file.\n\nWith --json it always exits 0: {\"authenticated\":false} when nobody is signed in (plus a `reason` when a saved sign-in ended), and when signed in {\"authenticated\":true,\"uuid\",\"id\",\"kind\",\"display_name\",\"expires_at\",\"refresh_expires_at\",\"verified\",…}. Without --json it exits 1 when nobody is signed in.",
        after_help = "Examples:\n  remind login status --json\n  remind login status --offline\n  remind --test <test_id> login status --json"
    )]
    Status {
        /// Read only the saved sign-in: no refresh, no network.
        #[arg(long)]
        offline: bool,
    },
}

/// The commands.
#[derive(Subcommand)]
pub enum Command {
    /// Show Remind's app id and the Silicon Accounts it signs in with; no sign-in, no network.
    #[command(
        after_help = "Prints {\"app_id\":\"remind\",\"accounts_url\",\"api_url\",\"version\",…}. A Silicon uses the app id to mint a short-lived token:\n  silicon-accounts login --app remind -q | remind login --slt-stdin\nExample: remind accounts --json"
    )]
    Accounts,
    /// Sign in with Silicon Accounts: a code for Carbons, a short-lived token for Silicons.
    #[command(
        args_conflicts_with_subcommands = true,
        long_about = "Sign in to Remind with Silicon Accounts. One sign-in is kept per Remind origin (and per test environment when you sign in with --test).\n\nCarbons: `remind login` prints a code and a link. Approve the code on the account site from any device where you are signed in; remind waits (polling as Silicon Accounts allows) until you approve, deny, or the code expires after 10 minutes. Nothing opens unless you add --open.\n\nSilicons never see a page: mint a short-lived token for Remind and hand it over. Tokens work once, for 2 minutes, for Remind only.\n  silicon-accounts login --app remind -q | remind login --slt-stdin\n  remind login slt_…          (the same, as the only argument)\nA token sign-in always replaces the sign-in saved here and ends the previous one.",
        after_help = "Examples:\n  remind login\n  remind login --open --label 'build laptop'\n  silicon-accounts login --app remind -q | remind login --slt-stdin\n  remind login status --json\nThen: remind whoami; remind logout ends the sign-in."
    )]
    Login(LoginArgs),
    /// End this sign-in at Silicon Accounts and forget it on this machine.
    #[command(
        after_help = "Ends only this machine's sign-in (for this Remind origin, or for the test environment named with --test); your other devices stay signed in.\nExamples: remind logout; remind --test <test_id> logout"
    )]
    Logout,
    /// Show the signed-in account as Remind sees it: kind, custodian, what you may do.
    #[command(after_help = "Example: remind whoami --json")]
    Whoami,
    /// Create a reminder owned by the signed-in Silicon (Carbons read only).
    #[command(
        after_help = "Examples:\n  remind create --text 'Daily standup' --cron '0 9 * * MON-FRI' --timezone Asia/Kolkata\n  remind create --text 'One reminder' --cron '30 16 * * *' --kind one-time --timezone UTC\n\nTimezone is mandatory for both recurring and one-time reminders. Use an IANA timezone identifier, such as Asia/Kolkata or UTC.\nDue reminders go to the Silicon's webhook subscriptions: remind webhook subscribe <url>."
    )]
    Create {
        /// What the reminder says when it fires.
        #[arg(long)]
        text: String,
        /// Five-field cron: minute hour day-of-month month day-of-week, for example '0 9 * * MON-FRI'.
        #[arg(long)]
        cron: String,
        /// Mandatory IANA timezone identifier, for example Asia/Kolkata or UTC.
        #[arg(long, value_name = "IANA_TIMEZONE", value_parser = nonblank_timezone)]
        timezone: String,
        /// recurring fires at every match; one-time fires at the first future match, then moves to the archive.
        #[arg(long, value_enum, default_value = "recurring")]
        kind: Kind,
    },
    /// List the reminders you can read; --archived shows the 45-day archive.
    #[command(
        after_help = "A Silicon sees its own reminders and those of its custodian's other Silicons; a Carbon sees those of the Silicons it looks after; anyone sees what was shared with them.\nExamples:\n  remind list\n  remind list --silicon si:scout --status paused\n  remind list --archived --limit 100\nPass next_cursor to --cursor for the next page."
    )]
    List {
        /// Only this Silicon's reminders (si: id or uuid).
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
        /// Show archived and fired one-time reminders (kept 45 days) instead of current ones.
        #[arg(long)]
        archived: bool,
        /// Only reminders in this state.
        #[arg(long, value_enum)]
        status: Option<Status>,
        #[command(flatten)]
        page: Page,
    },
    /// Show one reminder, including its next trigger and archive deadline.
    #[command(after_help = "Example: remind get <reminder_id>")]
    Get {
        /// Reminder id.
        id: Uuid,
    },
    /// Change a reminder's text, cron, timezone or kind (owner Silicon only; archived ones are fixed).
    #[command(
        after_help = "Omitted flags keep their values; --timezone may be left out to keep the stored one.\nExample: remind edit <reminder_id> --text 'Review the release build' --cron '0 10 * * *'"
    )]
    Edit {
        /// Reminder id.
        id: Uuid,
        /// New text.
        #[arg(long)]
        text: Option<String>,
        /// New five-field cron expression.
        #[arg(long)]
        cron: Option<String>,
        /// New IANA timezone.
        #[arg(long, value_name = "IANA_TIMEZONE")]
        timezone: Option<String>,
        /// New kind.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
    },
    /// Pause 1 to 100 of your reminders at once (all or nothing); they stay, without firing.
    #[command(after_help = "Example: remind pause <id> <id>…  ·  Undo with remind resume <id>…")]
    Pause {
        /// Reminder ids (1 to 100).
        #[arg(required = true, num_args = 1..=100)]
        ids: Vec<Uuid>,
    },
    /// Resume 1 to 100 of your paused reminders at once; each gets its next future occurrence.
    #[command(after_help = "Example: remind resume <id> <id>…")]
    Resume {
        /// Reminder ids (1 to 100).
        #[arg(required = true, num_args = 1..=100)]
        ids: Vec<Uuid>,
    },
    /// Archive one of your reminders; it stays readable for 45 days (remind list --archived).
    #[command(after_help = "Example: remind archive <reminder_id>")]
    Archive {
        /// Reminder id.
        id: Uuid,
    },
    /// Show a reminder's delivery history: attempts, failures and receipt ids.
    #[command(after_help = "Example: remind executions <reminder_id> --limit 20")]
    Executions {
        /// Reminder id.
        id: Uuid,
        #[command(flatten)]
        page: Page,
    },
    /// List the Silicons whose reminders you can read, and why (self, custodian, sibling, shared).
    #[command(
        after_help = "relation: self (you), custodian (a Silicon you look after), sibling (another Silicon of your custodian), shared (it shared its reminders with you).\nExample: remind silicons --json  ·  then: remind list --silicon si:<id>"
    )]
    Silicons {
        /// The previous page's next_cursor (a Silicon uuid).
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        /// Page size, 1 to 100.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Share a Silicon's reminders with other accounts (read-only), or stop sharing.
    Share {
        #[command(subcommand)]
        command: Share,
    },
    /// Choose which outside accounts may share reminders with a Silicon.
    Allow {
        #[command(subcommand)]
        command: Allow,
    },
    /// Manage where the signed-in Silicon's due reminders are posted (optional).
    Webhook {
        #[command(subcommand)]
        command: Webhook,
    },
    /// Create and manage Remind test environments (isolated copies of Remind for testing).
    Env {
        #[command(subcommand)]
        command: Environment,
    },
    /// Show the selected test environment (needs --test <id> or remind env use).
    #[command(after_help = "Example: remind --test <test_id> test-info")]
    TestInfo,
    /// Erase every reminder, delivery and subscription of the selected test environment.
    #[command(
        after_help = "Needs --test <id> (or remind env use). The environment and its key stay. Anyone with the key may clean it.\nExample: remind --test <test_id> clean"
    )]
    Clean,
    /// Read the bundled manuals offline.
    #[command(
        after_help = "Topics: cli (default), accounts (signing in, who sees what), api, client, testing, webhooks, releases.\nExample: remind docs accounts  ·  Online: https://docs.remind.teamofsilicons.com"
    )]
    Docs {
        /// Which manual.
        #[arg(default_value = "cli", value_parser = ["cli", "accounts", "api", "client", "testing", "webhooks", "releases"])]
        topic: String,
    },
    /// Send a bug report to the Remind team by email; optionally link a pull request.
    #[command(
        after_help = "Example: remind report 'Steps, expected result, actual result' --pr https://github.com/teamofsilicons/silicon-remind/pull/123\nUses your sign-in; reports inside a test environment are simulated. Never include secrets."
    )]
    Report {
        /// Steps, expected result, actual result.
        message: String,
        /// A Silicon Remind pull request that fixes it.
        #[arg(long, value_name = "URL")]
        pr: Option<String>,
    },
    /// Check whether your bug report was sent, failed or simulated.
    #[command(after_help = "Example: remind report-status <report_id>")]
    ReportStatus {
        /// Report id from remind report.
        id: Uuid,
    },
    /// Show or change local settings: origins, home directory, telemetry.
    Config {
        #[command(subcommand)]
        command: Config,
    },
    /// Check that the Remind API is up (--ready also checks its database).
    #[command(after_help = "Example: remind health --ready")]
    Health {
        /// Also check database readiness.
        #[arg(long)]
        ready: bool,
    },
    /// Prints exactly what `remind accounts` prints (kept for the Silicon runtime).
    #[command(hide = true)]
    Iam,
    /// Removes the hourly updater service that Remind 0.1 installed.
    #[command(hide = true)]
    Daemon {
        #[command(subcommand)]
        command: Daemon,
    },
    /// Replaced by remind login, remind logout and remind whoami.
    #[command(hide = true, disable_help_flag = true)]
    Auth {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired: Silicon Apps updates Remind.
    #[command(hide = true, disable_help_flag = true)]
    Update {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired with test environments of the previous identity service.
    #[command(hide = true, disable_help_flag = true)]
    ConfigureIam {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

/// `remind share …`: viewer grants on a Silicon's reminders.
#[derive(Subcommand)]
pub enum Share {
    /// Let an account (c: or si: id) read a Silicon's reminders; run by the Silicon or its custodian.
    #[command(
        long_about = "Let another account read a Silicon's reminders (never change them). A Silicon shares its own; a Carbon shares those of a Silicon it looks after, named with --silicon.\n\nAny Carbon can be granted. A Silicon outside your custodian's Silicons must first allow you: it (or its custodian) runs `remind allow add <your id>`.",
        after_help = "Examples:\n  remind share add c:ada                       (as the Silicon)\n  remind share add si:ledger --silicon si:scout (as si:scout's custodian)\nSee who can read: remind share list"
    )]
    Add {
        /// The account to share with: c:handle, si:handle or uuid.
        account: String,
        /// Which of your Silicons (si: id or uuid); needed when a Carbon shares.
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
    /// List grants on your (or your Silicons') reminders, and grants you received.
    #[command(after_help = "Example: remind share list --json")]
    List,
    /// Stop sharing a Silicon's reminders with an account.
    #[command(
        after_help = "Examples:\n  remind share remove c:ada\n  remind share remove si:ledger --silicon si:scout"
    )]
    Remove {
        /// The account: c:handle, si:handle or uuid.
        account: String,
        /// Which of your Silicons; needed when a Carbon removes a grant.
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
}

/// `remind allow …`: a Silicon's allow-list.
#[derive(Subcommand)]
pub enum Allow {
    /// Allow an outside account to share reminders with a Silicon; run by the Silicon or its custodian.
    #[command(
        long_about = "Silicons are not open to everyone: an account outside a Silicon's custodian and that custodian's other Silicons can share reminders with it only after the Silicon (or its custodian, with --silicon) allows that account here.",
        after_help = "Examples:\n  remind allow add c:ada                        (as the Silicon)\n  remind allow add si:ledger --silicon si:scout  (as si:scout's custodian)"
    )]
    Add {
        /// The account to allow: c:handle, si:handle or uuid.
        account: String,
        /// Which of your Silicons; needed when a Carbon allows.
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
    /// List the accounts allowed to share reminders with a Silicon.
    #[command(after_help = "Examples: remind allow list; remind allow list --silicon si:scout")]
    List {
        /// Which of your Silicons, for a Carbon.
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
    /// Remove an account from a Silicon's allow-list (grants it made possible end too).
    #[command(after_help = "Example: remind allow remove c:ada")]
    Remove {
        /// The account: c:handle, si:handle or uuid.
        account: String,
        /// Which of your Silicons, for a Carbon.
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
}

/// `remind webhook …`.
#[derive(Subcommand)]
pub enum Webhook {
    /// Add a webhook subscription: every due reminder is posted to this URL (signed when a secret is set).
    #[command(
        after_help = "You are asked for an optional signing secret; pass it on stdin with --secret-stdin, or send unsigned with --unsigned.\nExamples:\n  remind webhook subscribe https://hook.example/remind --secret-stdin < secret.txt\n  remind webhook subscribe https://hook.example/remind --unsigned\nReminders work without any subscription; they just are not delivered anywhere."
    )]
    Subscribe {
        /// Absolute https URL (plain http only for this machine outside production).
        #[arg(value_name = "URL")]
        endpoint_url: String,
        /// Read the signing secret from standard input.
        #[arg(long)]
        secret_stdin: bool,
        /// Post without a signature.
        #[arg(long, conflicts_with = "secret_stdin")]
        unsigned: bool,
    },
    /// Add a subscription through the older single-endpoint call (the same result as subscribe).
    #[command(hide = true)]
    Set {
        #[arg(value_name = "URL")]
        endpoint_url: String,
        #[arg(long)]
        secret_stdin: bool,
        #[arg(long, conflicts_with = "secret_stdin")]
        unsigned: bool,
    },
    /// Show the first active subscription (the signing secret is never shown).
    Get,
    /// List subscriptions: yours as a Silicon, or (read-only) those of the Silicons you look after.
    #[command(after_help = "Examples: remind webhook list; remind webhook list --silicon si:scout")]
    List {
        /// Only this Silicon's subscriptions (for a Carbon).
        #[arg(long, value_name = "SILICON")]
        silicon: Option<String>,
    },
    /// End one subscription by its id.
    #[command(after_help = "Example: remind webhook unsubscribe <subscription_id>")]
    Unsubscribe {
        /// Subscription id from remind webhook list.
        id: Uuid,
    },
    /// End every subscription of the signed-in Silicon.
    Disable,
}

/// `remind env …`.
#[derive(Subcommand)]
pub enum Environment {
    /// Create an empty test environment owned by you; its key is saved here.
    #[command(
        after_help = "Run without --test. A test environment is an isolated copy of Remind: up to 100 reminders, retired after 15 days without activity, recoverable for 30 days.\nExample: remind env create release-qa --description 'Manual release checks'\nThen: remind --test <id> create …  ·  share the key: remind env key <id>"
    )]
    Create {
        /// A name unique among your active test environments.
        name: String,
        /// What it is for.
        #[arg(long)]
        description: Option<String>,
    },
    /// List your test environments and those of the Silicons you look after (or of your custodian).
    List {
        /// Also list retired environments that can still be restored.
        #[arg(long)]
        include_deleted: bool,
        /// The previous page's next_cursor.
        #[arg(long)]
        after: Option<Uuid>,
        /// Page size, 1 to 100.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Show one test environment and its retention deadlines.
    Get {
        /// Test environment id.
        id: Uuid,
    },
    /// Print a test environment's key and save it here.
    Key {
        /// Test environment id.
        id: Uuid,
    },
    /// Replace a test environment's key (owner, or the owner's custodian) and save the new one.
    Rotate {
        /// Test environment id.
        id: Uuid,
    },
    /// Retire a test environment; restore it within 30 days with remind env restore.
    Delete {
        /// Test environment id.
        id: Uuid,
    },
    /// Bring a retired test environment back with a new key, saved here.
    Restore {
        /// Test environment id.
        id: Uuid,
    },
    /// Save a key someone shared with you (no sign-in needed); it is checked first.
    #[command(after_help = "Example: remind env import <test_id> --key-stdin < key.txt")]
    Import {
        /// Test environment id.
        id: Uuid,
        /// Read the key from standard input instead of prompting.
        #[arg(long)]
        key_stdin: bool,
    },
    /// Forget a test environment's key and sign-in on this machine; the environment stays.
    Forget {
        /// Test environment id.
        id: Uuid,
    },
    /// Run every following command inside this test environment until remind env exit.
    #[command(
        after_help = "Example: remind env use <test_id>  ·  one command in production meanwhile: remind --production <command>"
    )]
    Use {
        /// Test environment id (its key must be saved here).
        id: Uuid,
    },
    /// Go back to production after remind env use.
    Exit,
}

/// `remind config …`.
#[derive(Subcommand)]
pub enum Config {
    /// Show local settings (never secrets).
    Show,
    /// Save the Remind API origin used when --url and REMIND_URL are not given.
    #[command(
        after_help = "Example: remind config set-url http://127.0.0.1:4181  (plain http only for this machine)"
    )]
    SetUrl {
        /// https origin, e.g. https://api.remind.teamofsilicons.com.
        #[arg(value_name = "URL")]
        service_url: String,
    },
    /// Save the Silicon Accounts origin used when --accounts-url and ACCOUNTS_URL are not given.
    #[command(
        after_help = "Example: remind config set-accounts-url https://accounts.teamofsilicons.com"
    )]
    SetAccountsUrl {
        /// https origin.
        #[arg(value_name = "URL")]
        accounts_url: String,
    },
    /// Keep Remind's local state in <directory>/.remind from now on (the directory must exist).
    #[command(
        after_help = "Example: remind config home /srv/silicons/scout\nExisting sign-ins and keys are not moved."
    )]
    Home {
        /// An existing directory.
        #[arg(value_name = "DIRECTORY")]
        location: std::path::PathBuf,
    },
    /// Turn operational telemetry on or off (no arguments, secrets or reminder text are sent).
    Telemetry {
        #[arg(value_enum)]
        value: Toggle,
    },
    /// Retired: Silicon Apps keeps Remind up to date.
    #[command(hide = true)]
    AutoUpdate {
        #[arg(value_enum)]
        value: Toggle,
    },
}

/// `remind daemon …` (hidden).
#[derive(Subcommand)]
pub enum Daemon {
    /// Stop and remove the updater service installed by Remind 0.1.
    Uninstall,
    /// Show whether that updater service is still installed.
    Status,
    /// Retired.
    #[command(hide = true)]
    Install,
    /// Retired.
    #[command(hide = true)]
    Run,
}

/// On or off.
#[derive(Clone, Copy, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

/// Reminder kind.
#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    OneTime,
    Recurring,
}

/// Reminder state filter.
#[derive(Clone, Copy, ValueEnum)]
pub enum Status {
    Active,
    Paused,
    Completed,
}

/// Paging flags.
#[derive(Args)]
pub struct Page {
    /// The previous page's next_cursor.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Page size, 1 to 100.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: u32,
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command, Environment, LoginCommand, Share, with_timezone_guidance};
    use clap::{CommandFactory as _, Parser as _};

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args).map_err(with_timezone_guidance)
    }

    #[test]
    fn the_command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

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
                let Err(error) = parse(&args) else {
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
            for supplied in ["Asia/Kolkata", "UTC"] {
                let cli = parse(&[
                    "remind",
                    "create",
                    "--text",
                    "Check the build",
                    "--cron",
                    "0 9 * * *",
                    "--kind",
                    kind,
                    "--timezone",
                    supplied,
                ])?;
                let Command::Create { timezone, .. } = cli.command else {
                    panic!("expected create command");
                };
                assert_eq!(timezone, supplied);
            }
        }
        Ok(())
    }

    #[test]
    fn edit_without_timezone_leaves_it_unchanged() -> anyhow::Result<()> {
        let cli = parse(&[
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

    #[test]
    fn login_forms_parse() -> anyhow::Result<()> {
        let Command::Login(device) = parse(&["remind", "login"])?.command else {
            panic!("login");
        };
        assert!(device.token.is_none() && device.slt.is_none() && !device.slt_stdin);
        assert!(device.command.is_none());
        let Command::Login(positional) = parse(&["remind", "login", "slt_abc"])?.command else {
            panic!("login");
        };
        assert_eq!(positional.token.as_deref(), Some("slt_abc"));
        let Command::Login(flag) = parse(&["remind", "login", "--slt", "slt_abc"])?.command else {
            panic!("login");
        };
        assert_eq!(flag.slt.as_deref(), Some("slt_abc"));
        let Command::Login(stdin) = parse(&["remind", "login", "--slt-stdin", "--json"])?.command
        else {
            panic!("login");
        };
        assert!(stdin.slt_stdin);
        let Command::Login(status) =
            parse(&["remind", "login", "status", "--offline", "--json"])?.command
        else {
            panic!("login");
        };
        assert!(matches!(
            status.command,
            Some(LoginCommand::Status { offline: true })
        ));
        for conflicting in [
            vec!["remind", "login", "slt_a", "--slt", "slt_b"],
            vec!["remind", "login", "--slt", "slt_a", "--slt-stdin"],
            vec!["remind", "login", "--slt-stdin", "--open"],
            vec!["remind", "login", "slt_a", "status"],
        ] {
            assert!(
                parse(&conflicting).is_err(),
                "{conflicting:?} must be refused"
            );
        }
        Ok(())
    }

    #[test]
    fn organisation_flags_are_gone_and_retired_commands_still_parse() -> anyhow::Result<()> {
        for removed in [
            vec!["remind", "--org", "tos", "list"],
            vec!["remind", "--account", "si:a", "list"],
            vec!["remind", "env", "create", "qa", "--iam-key-file", "/k"],
        ] {
            assert!(parse(&removed).is_err(), "{removed:?} must be refused");
        }
        assert!(matches!(
            parse(&["remind", "iam", "--json"])?.command,
            Command::Iam
        ));
        assert!(matches!(
            parse(&["remind", "auth", "login", "--slt-stdin"])?.command,
            Command::Auth { .. }
        ));
        assert!(matches!(
            parse(&["remind", "update", "--check"])?.command,
            Command::Update { .. }
        ));
        let Command::Share {
            command: Share::Add { account, silicon },
        } = parse(&["remind", "share", "add", "c:ada", "--silicon", "si:scout"])?.command
        else {
            panic!("share add");
        };
        assert_eq!(
            (account.as_str(), silicon.as_deref()),
            ("c:ada", Some("si:scout"))
        );
        assert!(matches!(
            parse(&[
                "remind",
                "env",
                "use",
                "01992000-0000-7000-8000-000000000004"
            ])?
            .command,
            Command::Env {
                command: Environment::Use { .. }
            }
        ));
        Ok(())
    }

    #[test]
    fn help_never_shows_retired_names_or_organisations() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        for word in [
            "iam",
            "IAM",
            "honeycomb",
            "Honeycomb",
            "organization",
            "--org",
            "daemon",
        ] {
            assert!(!help.contains(word), "help mentions {word}:\n{help}");
        }
        for word in [
            "login", "accounts", "logout", "silicons", "share", "allow", "webhook", "env",
        ] {
            assert!(help.contains(word), "help lacks {word}");
        }
    }
}
