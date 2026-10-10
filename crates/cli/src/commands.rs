//! Every command except signing in: discovery, reminders, sharing, webhooks, test
//! environments, settings, manuals and reports.
use crate::{
    args::{Allow, Command, Config, Share, Toggle, Webhook},
    output::{CliError, EXIT_OK, key_values},
    session::{Authed, Ctx},
    state::{configure_home, slot},
};
use serde_json::{Value, json};
use silicon_remind_client::{API_VERSION, Client, Secret, models};
use std::io::{IsTerminal as _, Read as _};

/// The `remind accounts --json` object (also printed by the hidden `remind iam --json`).
pub fn accounts_json(ctx: &Ctx) -> Value {
    json!({
        "app_id": ctx.app_id,
        "client_id": ctx.app_id,
        "accounts_url": ctx.accounts_url,
        "api_url": ctx.url,
        "version": env!("CARGO_PKG_VERSION"),
        "api_version": API_VERSION,
        "sign_in": {
            "carbon": "remind login",
            "silicon": format!("silicon-accounts login --app {} -q | remind login --slt-stdin", ctx.app_id),
            "status": "remind login status --json",
        },
        "docs_url": "https://docs.remind.teamofsilicons.com",
    })
}

/// `remind accounts`: offline; never fails.
pub fn accounts(ctx: &Ctx) -> anyhow::Result<u8> {
    let body = accounts_json(ctx);
    let text = format!(
        "Remind signs in with Silicon Accounts.\n{}Carbons: remind login\nSilicons: silicon-accounts login --app {} -q | remind login --slt-stdin",
        key_values(&[
            ("app id", ctx.app_id.clone()),
            ("accounts", ctx.accounts_url.clone()),
            ("remind api", ctx.url.clone()),
            ("version", env!("CARGO_PKG_VERSION").to_owned()),
        ]),
        ctx.app_id
    );
    ctx.out.either(&body, &text)?;
    Ok(EXIT_OK)
}

/// Runs one command that needs neither `login` nor the hidden ones.
pub async fn run(ctx: &Ctx, command: &Command) -> anyhow::Result<u8> {
    match command {
        Command::Whoami => {
            let mut authed = Authed::new(ctx).await?;
            ctx.out
                .result(&authed.call(async |c| c.me().await).await?)?;
        }
        Command::Create {
            text,
            cron,
            timezone,
            kind,
        } => {
            let mutation = ctx.mutation()?;
            let input = models::CreateScheduleRequest {
                text: text.clone(),
                cron: cron.clone(),
                timezone: timezone.clone(),
                kind: (*kind).into(),
            };
            let mut authed = Authed::new(ctx).await?;
            let reminder = authed
                .call(async |c| c.create_reminder(&input, &mutation).await)
                .await?;
            ctx.out.result(&reminder)?;
            ctx.out.suggest(&format!(
                "Created. Delivery history: remind executions {id}; pause it: remind pause {id}.",
                id = reminder.id
            ));
        }
        Command::List {
            silicon,
            archived,
            status,
            page,
        } => {
            let query = models::ListSchedules {
                silicon_id: silicon.clone(),
                section: if *archived {
                    models::ScheduleSection::Archived
                } else {
                    models::ScheduleSection::Current
                },
                status: status.map(Into::into),
                cursor: page.cursor.clone(),
                limit: Some(page.limit),
            };
            let mut authed = Authed::new(ctx).await?;
            ctx.out
                .result(&authed.call(async |c| c.reminders(&query).await).await?)?;
        }
        Command::Get { id } => {
            let mut authed = Authed::new(ctx).await?;
            ctx.out
                .result(&authed.call(async |c| c.reminder(*id).await).await?)?;
        }
        Command::Edit {
            id,
            text,
            cron,
            timezone,
            kind,
        } => {
            if text.is_none() && cron.is_none() && timezone.is_none() && kind.is_none() {
                return Err(CliError::usage(
                    "Nothing to change.",
                    "Give at least one of --text, --cron, --timezone or --kind; see remind edit --help.",
                )
                .into());
            }
            let mutation = ctx.mutation()?;
            let input = models::PatchScheduleRequest {
                text: text.clone(),
                cron: cron.clone(),
                timezone: timezone.clone(),
                kind: kind.map(Into::into),
                status: None,
            };
            let mut authed = Authed::new(ctx).await?;
            let reminder = authed
                .call(async |c| c.update_reminder(*id, &input, &mutation).await)
                .await?;
            ctx.out.result(&reminder)?;
        }
        Command::Pause { ids } | Command::Resume { ids } => {
            let status = if matches!(command, Command::Pause { .. }) {
                models::ScheduleStatus::Paused
            } else {
                models::ScheduleStatus::Active
            };
            let mutation = ctx.mutation()?;
            let mut authed = Authed::new(ctx).await?;
            let batch = authed
                .call(async |c| c.set_status(ids.clone(), status, &mutation).await)
                .await?;
            ctx.out.result(&batch)?;
        }
        Command::Archive { id } => {
            let mut authed = Authed::new(ctx).await?;
            authed.call(async |c| c.archive_reminder(*id).await).await?;
            ctx.out.result(&json!({"status": "archived", "id": id}))?;
            ctx.out
                .suggest("It stays readable for 45 days: remind list --archived.");
        }
        Command::Executions { id, page } => {
            let paging = models::Paging {
                cursor: page.cursor.clone(),
                limit: Some(page.limit),
            };
            let mut authed = Authed::new(ctx).await?;
            ctx.out.result(
                &authed
                    .call(async |c| c.executions(*id, &paging).await)
                    .await?,
            )?;
        }
        Command::Silicons { after, limit } => {
            let mut authed = Authed::new(ctx).await?;
            let page = authed
                .call(async |c| c.silicons(after.as_deref(), *limit).await)
                .await?;
            ctx.out.result(&page)?;
        }
        Command::Share { command } => share(ctx, command).await?,
        Command::Allow { command } => allow(ctx, command).await?,
        Command::Webhook { command } => webhook(ctx, command).await?,
        Command::Env { command } => return crate::environments::run(ctx, command).await,
        Command::TestInfo => {
            let client = test_client(ctx, "test-info")?;
            ctx.out.result(&client.current_environment().await?)?;
        }
        Command::Clean => {
            let client = test_client(ctx, "clean")?;
            client.clean_environment().await?;
            ctx.out.result(&json!({"status": "cleaned"}))?;
            ctx.out.suggest(
                "Every reminder, delivery, subscription and log of this test environment was erased; the environment and its key stay.",
            );
        }
        Command::Docs { topic } => docs(ctx, topic)?,
        Command::Report { message, pr } => report(ctx, message, pr.as_deref()).await?,
        Command::ReportStatus { id } => {
            let mut authed = Authed::new(ctx).await?;
            ctx.out
                .result(&authed.call(async |c| c.report_status(*id).await).await?)?;
        }
        Command::Config { command } => config(ctx, command)?,
        Command::Health { ready } => {
            ctx.out
                .result(&ctx.production_client()?.health(*ready).await?)?;
        }
        Command::Accounts
        | Command::Login(_)
        | Command::Logout
        | Command::Iam
        | Command::Daemon { .. }
        | Command::Auth { .. }
        | Command::Update { .. }
        | Command::ConfigureIam { .. } => unreachable!("dispatched by main"),
    }
    Ok(EXIT_OK)
}

/// A client inside the selected test environment, for commands that only exist there.
fn test_client(ctx: &Ctx, command: &str) -> anyhow::Result<Client> {
    if ctx.test.is_none() {
        return Err(CliError::usage(
            "This action is only possible inside a test environment.",
            format!("Use remind --test <test_id> {command} (or select one with remind env use <test_id>)."),
        )
        .into());
    }
    ctx.client()
}

async fn share(ctx: &Ctx, command: &Share) -> anyhow::Result<()> {
    let mut authed = Authed::new(ctx).await?;
    match command {
        Share::Add { account, silicon } => {
            let target = models::AccountTarget {
                id: account.trim().to_owned(),
                silicon_id: silicon.clone(),
            };
            let grant = authed.call(async |c| c.grant_viewer(&target).await).await?;
            ctx.out.result(&grant)?;
            ctx.out.suggest(&format!(
                "{} can now read {}'s reminders. Undo with remind share remove {}.",
                display(&grant.viewer),
                display(&grant.owner),
                grant.viewer.id
            ));
        }
        Share::List => ctx
            .out
            .result(&authed.call(async |c| c.viewers().await).await?)?,
        Share::Remove { account, silicon } => {
            authed
                .call(async |c| c.revoke_viewer(account, silicon.as_deref()).await)
                .await?;
            ctx.out
                .result(&json!({"status": "share_removed", "account": account.trim()}))?;
        }
    }
    Ok(())
}

async fn allow(ctx: &Ctx, command: &Allow) -> anyhow::Result<()> {
    let mut authed = Authed::new(ctx).await?;
    match command {
        Allow::Add { account, silicon } => {
            let target = models::AccountTarget {
                id: account.trim().to_owned(),
                silicon_id: silicon.clone(),
            };
            let allowance = authed
                .call(async |c| c.allow_account(&target).await)
                .await?;
            ctx.out.result(&allowance)?;
            ctx.out.suggest(&format!(
                "{} may now share reminders with {}.",
                display(&allowance.allowed),
                display(&allowance.silicon)
            ));
        }
        Allow::List { silicon } => ctx.out.result(
            &authed
                .call(async |c| c.allowed_accounts(silicon.as_deref()).await)
                .await?,
        )?,
        Allow::Remove { account, silicon } => {
            authed
                .call(async |c| c.disallow_account(account, silicon.as_deref()).await)
                .await?;
            ctx.out
                .result(&json!({"status": "allowance_removed", "account": account.trim()}))?;
        }
    }
    Ok(())
}

fn display(account: &models::AccountRef) -> String {
    if account.id.is_empty() {
        account.uuid.clone()
    } else {
        account.id.clone()
    }
}

async fn webhook(ctx: &Ctx, command: &Webhook) -> anyhow::Result<()> {
    let mut authed = Authed::new(ctx).await?;
    match command {
        Webhook::Subscribe {
            endpoint_url,
            secret_stdin,
            unsigned,
        }
        | Webhook::Set {
            endpoint_url,
            secret_stdin,
            unsigned,
        } => {
            let signing_secret = if *unsigned {
                None
            } else {
                Some(read_secret(
                    "Webhook signing secret: ",
                    *secret_stdin,
                    "--secret-stdin or --unsigned",
                )?)
            };
            let destination = models::Destination {
                endpoint_url: endpoint_url.clone(),
                signing_secret,
            };
            let receipt = if matches!(command, Webhook::Set { .. }) {
                authed
                    .call(async |c| c.configure_webhook(&destination).await)
                    .await?
            } else {
                authed
                    .call(async |c| c.subscribe_webhook(&destination).await)
                    .await?
            };
            ctx.out.result(&receipt)?;
            ctx.out.suggest("Subscribed: every due reminder is posted there. See them all with remind webhook list.");
        }
        Webhook::Get => ctx
            .out
            .result(&authed.call(async |c| c.webhook().await).await?)?,
        Webhook::List { silicon } => ctx.out.result(
            &authed
                .call(async |c| c.webhooks(silicon.as_deref()).await)
                .await?,
        )?,
        Webhook::Unsubscribe { id } => {
            authed
                .call(async |c| c.unsubscribe_webhook(*id).await)
                .await?;
            ctx.out
                .result(&json!({"status": "webhook_unsubscribed", "id": id}))?;
        }
        Webhook::Disable => {
            authed.call(async |c| c.disable_webhook().await).await?;
            ctx.out.result(&json!({"status": "webhook_disabled"}))?;
        }
    }
    Ok(())
}

/// Reads a secret from stdin (`from_stdin`) or a hidden prompt on a terminal.
pub fn read_secret(prompt: &str, from_stdin: bool, alternatives: &str) -> anyhow::Result<Secret> {
    let text = if from_stdin {
        let mut value = String::new();
        std::io::stdin().take(65_537).read_to_string(&mut value)?;
        value
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(CliError::usage(
                "There is no terminal to ask for the secret.",
                format!("Use {alternatives}."),
            )
            .into());
        }
        rpassword::prompt_password(prompt)?
    };
    if text.trim().is_empty() || text.len() > 65_536 {
        return Err(CliError::usage("The secret is empty or longer than 64 KiB.", "").into());
    }
    Ok(Secret::new(text.trim()))
}

fn docs(ctx: &Ctx, topic: &str) -> anyhow::Result<()> {
    let markdown = match topic {
        "accounts" => include_str!("../docs/accounts.md"),
        "api" => include_str!("../docs/api/README.md"),
        "client" => include_str!("../docs/client/README.md"),
        "testing" => include_str!("../docs/testing-environments.md"),
        "webhooks" => include_str!("../docs/webhook-delivery.md"),
        "releases" => include_str!("../docs/releases.md"),
        _ => include_str!("../docs/cli/README.md"),
    };
    if ctx.out.json {
        ctx.out
            .result(&json!({"topic": topic, "markdown": markdown}))
    } else {
        println!("{markdown}");
        Ok(())
    }
}

async fn report(ctx: &Ctx, message: &str, pr: Option<&str>) -> anyhow::Result<()> {
    if message.trim().is_empty() {
        return Err(CliError::usage(
            "The report message is empty.",
            "Describe the steps, the expected result and the actual result.",
        )
        .into());
    }
    if let Some(pr) = pr
        && (!pr.starts_with("https://github.com/teamofsilicons/silicon-remind/pull/")
            || !pr
                .rsplit('/')
                .next()
                .is_some_and(|n| n.parse::<u64>().is_ok()))
    {
        return Err(CliError::usage(
            "--pr must link a Silicon Remind pull request.",
            "For example --pr https://github.com/teamofsilicons/silicon-remind/pull/123.",
        )
        .into());
    }
    let mutation = ctx.mutation()?;
    let input = models::BugReportRequest {
        message: message.to_owned(),
        pr: pr.map(str::to_owned),
    };
    let mut authed = Authed::new(ctx).await?;
    let receipt = authed
        .call(async |c| c.report(&input, &mutation).await)
        .await?;
    ctx.out.result(&receipt)?;
    ctx.out.suggest(&format!(
        "Check delivery with remind report-status {}.{}",
        receipt.id,
        if pr.is_none() {
            " You can also propose a fix: https://github.com/teamofsilicons/silicon-remind"
        } else {
            ""
        }
    ));
    Ok(())
}

fn config(ctx: &Ctx, command: &Config) -> anyhow::Result<()> {
    match command {
        Config::Show => {
            let home = ctx.home()?;
            let state = &ctx.snapshot.state;
            let sign_ins: Vec<Value> = state
                .sign_ins
                .iter()
                .map(|(slot, stored)| {
                    json!({"slot": slot, "id": stored.account.id, "kind": stored.account.kind.as_str()})
                })
                .collect();
            ctx.out.result(&json!({
                "url": ctx.url,
                "accounts_url": ctx.accounts_url,
                "app_id": ctx.app_id,
                "home": home.parent().display().to_string(),
                "state_dir": home.dir().display().to_string(),
                "telemetry": state.telemetry,
                "sign_ins": sign_ins,
                "saved_test_environment_count": state.test_keys.len(),
                "selected_test_environment": state.selected_tests.get(&ctx.url),
            }))?;
        }
        Config::SetUrl { service_url } => {
            let url = Client::new(service_url)?.base_url().to_owned();
            let saved = url.clone();
            save(ctx, move |state| state.url = saved)?;
            ctx.out.result(&json!({"url": url}))?;
            ctx.out.suggest("Sign-ins are kept per Remind origin: run remind login status to see whether you are signed in there.");
        }
        Config::SetAccountsUrl { accounts_url } => {
            let url = silicon_remind_client::accounts::SignIn::new(accounts_url, &ctx.app_id)?
                .accounts_url()
                .to_owned();
            let saved = url.clone();
            save(ctx, move |state| state.accounts_url = Some(saved))?;
            ctx.out.result(&json!({"accounts_url": url}))?;
            ctx.out.suggest("Existing sign-ins keep the Silicon Accounts that issued them; new sign-ins use this one.");
        }
        Config::Home { location } => {
            let location = configure_home(location)?;
            ctx.out.result(&json!({"home": location}))?;
            ctx.out.suggest(
                "Later commands keep their state there; existing sign-ins and keys were not moved.",
            );
        }
        Config::Telemetry { value } => {
            let enabled = matches!(value, Toggle::On);
            save(ctx, move |state| state.telemetry = enabled)?;
            ctx.out.result(&json!({"telemetry": enabled}))?;
        }
        Config::AutoUpdate { .. } => {
            ctx.out
                .result(&json!({"auto_update": "managed", "manager": "silicon-apps"}))?;
            ctx.out.suggest(
                "Silicon Apps keeps Remind up to date; there is nothing to configure here.",
            );
        }
    }
    Ok(())
}

/// Changes the saved state under the lock and prints any notices.
pub fn save(ctx: &Ctx, change: impl FnOnce(&mut crate::state::State)) -> anyhow::Result<()> {
    let ((), notices) = ctx.home()?.update(|state| {
        change(state);
        Ok(())
    })?;
    for notice in notices {
        ctx.out.warn(&notice);
    }
    Ok(())
}

/// The saved key slot of test environment `id` on this origin.
pub fn key_slot(ctx: &Ctx, id: uuid::Uuid) -> String {
    slot(&ctx.url, Some(id))
}
