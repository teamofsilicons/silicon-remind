//! Stateful CLI composed exclusively from the public Remind Rust client.
mod args;
mod state;
use anyhow::{Context as _, bail};
use args::{Auth, Cli, Command, Config, Environment, Toggle, Webhook};
use clap::Parser as _;
use silicon_remind_client::{Client, Mutation, Secret, models, updates};
use state::{Store, StoredSession, slot};
use std::io::{IsTerminal as _, Read as _};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(&cli).await {
        if cli.json {
            eprintln!("{}", machine_error(&error));
        } else {
            eprintln!("Error: {error}\nRun remind <command> --help for syntax and examples.");
        }
        let code = match error.downcast_ref::<silicon_remind_client::Error>() {
            Some(silicon_remind_client::Error::Api { status: 401, .. }) => 3,
            Some(silicon_remind_client::Error::Api { status: 403, .. }) => 4,
            Some(silicon_remind_client::Error::Invalid(_)) => 2,
            _ => 1,
        };
        std::process::exit(code);
    }
}

fn machine_error(error: &anyhow::Error) -> serde_json::Value {
    match error.downcast_ref::<silicon_remind_client::Error>() {
        Some(silicon_remind_client::Error::Api {
            status,
            code,
            message,
            request_id,
            retry_after,
        }) => {
            serde_json::json!({"error":{"code":code,"message":message,"status":status,"request_id":request_id,"retry_after":retry_after}})
        }
        Some(silicon_remind_client::Error::Invalid(message)) => {
            serde_json::json!({"error":{"code":"invalid_input","message":message}})
        }
        _ => serde_json::json!({"error":{"code":"cli_error","message":error.to_string()}}),
    }
}

async fn run(cli: &Cli) -> anyhow::Result<()> {
    let mut store = Store::open()?;
    let result = execute(cli, &mut store).await;
    store.save()?;
    let explicit = matches!(cli.command, Command::Update { .. });
    let due = store.state.auto_update
        && !cli.no_update
        && updates::unix_time().saturating_sub(store.state.last_update_check) >= 3600;
    if explicit || due {
        // Persist attempts before networking, including failure, so registry outages
        // cannot turn every CLI command into another network check.
        store.state.last_update_check = updates::unix_time();
        store.save()?;
        let apply = !matches!(cli.command, Command::Update { check: true });
        // Keep the process lock until maintenance completes: two invocations must
        // not overwrite one another's token or updater state.
        let status =
            updates::maintain("silicon-remind-cli", env!("CARGO_PKG_VERSION"), true, apply).await;
        if explicit {
            output(cli, &status)?;
        } else if !cli.json && matches!(status, updates::UpdateStatus::Updated { .. }) {
            eprintln!("CLI update installed. The next command uses the new version.");
        }
    }
    result
}

async fn execute(cli: &Cli, store: &mut Store) -> anyhow::Result<()> {
    let url = cli
        .url
        .as_deref()
        .unwrap_or(&store.state.url)
        .trim_end_matches('/')
        .to_owned();
    let mut client = Client::new(&url)?.auto_update(false);
    let session_slot = slot(&url, cli.test);
    if let Some(id) = cli.test {
        let key=store.state.test_keys.get(&slot(&url,Some(id))).context("test key is not saved for this server; run remind env import <id>, or remind env key <id> using your production session")?;
        client = client.with_test_environment(key.clone())?;
    }
    let needs_session = matches!(
        &cli.command,
        Command::Create { .. }
            | Command::List { .. }
            | Command::Get { .. }
            | Command::Edit { .. }
            | Command::Pause { .. }
            | Command::Resume { .. }
            | Command::Archive { .. }
            | Command::Executions { .. }
            | Command::Silicons { .. }
            | Command::Webhook { .. }
            | Command::Auth {
                command: Auth::Whoami | Auth::Refresh | Auth::Logout
            }
    ) || matches!(&cli.command,Command::Env{command} if !matches!(command,Environment::Import{..}|Environment::Forget{..}));
    if needs_session {
        let stored=store.state.sessions.get(&session_slot).context("no session for this server and environment; run remind auth login --org <org> (with --test <id> for a sandbox)")?;
        let org = cli.org.clone().unwrap_or_else(|| stored.org.clone());
        let refresh_due = stored.pending_refresh_key.is_some()
            || stored.expires_at <= chrono::Utc::now().timestamp() + 30;
        if refresh_due
            && !matches!(
                cli.command,
                Command::Auth {
                    command: Auth::Logout | Auth::Refresh
                }
            )
        {
            refresh_session(store, &session_slot, &client, None).await?;
        }
        let stored = store
            .state
            .sessions
            .get(&session_slot)
            .context("session disappeared")?;
        client = client.with_session(stored.session.access_token.clone(), org)?;
    }
    let mutation = match &cli.idempotency_key {
        Some(key) => Mutation::with_key(key)?,
        None => Mutation::new(),
    };
    match &cli.command {
        Command::Login { slt } => {
            login_with_token(cli, store, &client, &session_slot, Secret::new(slt.clone())).await?;
        }
        Command::Auth { command } => match command {
            Auth::Login { slt_stdin } => {
                let token = read_secret("IAM short-lived token: ", *slt_stdin)?;
                login_with_token(cli, store, &client, &session_slot, token).await?;
                suggest(
                    cli,
                    "Logged in. Optional next step: remind webhook subscribe <url>.",
                );
            }
            Auth::Whoami => output(cli, &client.me().await?)?,
            Auth::Refresh => {
                refresh_session(store, &session_slot, &client, Some(&mutation)).await?;
                output(cli, &serde_json::json!({"status":"session_refreshed"}))?;
            }
            Auth::Logout => {
                let stored = store
                    .state
                    .sessions
                    .get(&session_slot)
                    .context("no saved session")?;
                client
                    .logout(&stored.session.refresh_token, &mutation)
                    .await?;
                store.state.sessions.remove(&session_slot);
                store.save()?;
                output(cli, &serde_json::json!({"status":"logged_out"}))?;
            }
        },
        Command::Create {
            text,
            cron,
            timezone,
            kind,
        } => {
            let reminder = client
                .create_reminder(
                    &models::CreateScheduleRequest {
                        text: text.clone(),
                        cron: cron.clone(),
                        timezone: timezone.clone(),
                        kind: (*kind).into(),
                    },
                    &mutation,
                )
                .await?;
            output(cli, &reminder)?;
            suggest(
                cli,
                &format!(
                    "Created. Inspect delivery with remind executions {}. Pause it with remind pause {}.",
                    reminder.id, reminder.id
                ),
            );
        }
        Command::List {
            silicon,
            archived,
            status,
            page,
        } => output(
            cli,
            &client
                .reminders(&models::ListSchedules {
                    silicon_id: silicon.clone(),
                    section: if *archived {
                        models::ScheduleSection::Archived
                    } else {
                        models::ScheduleSection::Current
                    },
                    status: status.map(Into::into),
                    cursor: page.cursor(),
                    limit: Some(page.limit()),
                })
                .await?,
        )?,
        Command::Get { id } => output(cli, &client.reminder(*id).await?)?,
        Command::Edit {
            id,
            text,
            cron,
            timezone,
            kind,
        } => {
            if text.is_none() && cron.is_none() && timezone.is_none() && kind.is_none() {
                bail!(
                    "supply at least one of --text, --cron, --timezone or --kind; see remind edit --help"
                );
            }
            output(
                cli,
                &client
                    .update_reminder(
                        *id,
                        &models::PatchScheduleRequest {
                            text: text.clone(),
                            cron: cron.clone(),
                            timezone: timezone.clone(),
                            kind: kind.map(Into::into),
                            status: None,
                        },
                        &mutation,
                    )
                    .await?,
            )?;
        }
        Command::Pause { ids } => output(
            cli,
            &client
                .set_status(ids.clone(), models::ScheduleStatus::Paused, &mutation)
                .await?,
        )?,
        Command::Resume { ids } => output(
            cli,
            &client
                .set_status(ids.clone(), models::ScheduleStatus::Active, &mutation)
                .await?,
        )?,
        Command::Archive { id } => {
            client.archive_reminder(*id).await?;
            output(cli, &serde_json::json!({"status":"archived","id":id}))?;
            suggest(
                cli,
                "The reminder stays readable for 45 days. View it with remind list --archived.",
            );
        }
        Command::Executions { id, page } => output(
            cli,
            &client
                .executions(
                    *id,
                    &models::Paging {
                        cursor: page.cursor(),
                        limit: Some(page.limit()),
                    },
                )
                .await?,
        )?,
        Command::Silicons { after, limit } => output(cli, &client.silicons(*after, *limit).await?)?,
        Command::Webhook { command } => match command {
            Webhook::Set {
                endpoint_url,
                secret_stdin,
                unsigned,
            } => {
                let secret = if *unsigned {
                    None
                } else {
                    Some(read_secret("Webhook signing secret: ", *secret_stdin)?)
                };
                output(
                    cli,
                    &client
                        .configure_webhook(&models::Destination {
                            endpoint_url: endpoint_url.clone(),
                            signing_secret: secret,
                        })
                        .await?,
                )?;
                suggest(
                    cli,
                    "Webhook subscription added. Reminders can be created without a subscription.",
                );
            }
            Webhook::Subscribe {
                endpoint_url,
                secret_stdin,
                unsigned,
            } => {
                let secret = if *unsigned {
                    None
                } else {
                    Some(read_secret("Webhook signing secret: ", *secret_stdin)?)
                };
                output(
                    cli,
                    &client
                        .subscribe_webhook(&models::Destination {
                            endpoint_url: endpoint_url.clone(),
                            signing_secret: secret,
                        })
                        .await?,
                )?;
            }
            Webhook::Get => output(cli, &client.webhook().await?)?,
            Webhook::List => output(cli, &client.webhooks().await?)?,
            Webhook::Unsubscribe { id } => {
                client.unsubscribe_webhook(*id).await?;
                output(
                    cli,
                    &serde_json::json!({"status":"webhook_unsubscribed","id":id}),
                )?;
            }
            Webhook::Disable => {
                client.disable_webhook().await?;
                output(cli, &serde_json::json!({"status":"webhook_disabled"}))?;
            }
        },
        Command::Env { command } => match command {
            Environment::Create {
                name,
                description,
                iam_key_file,
                iam_app_secret_file,
            } => {
                let iam_test_key =
                    secret_file_or_prompt(iam_key_file, "IAM test-environment key: ")?;
                let iam_app_secret = iam_app_secret_file
                    .as_ref()
                    .map(|path| {
                        secret_file_or_prompt(&Some(path.clone()), "IAM test-only app secret: ")
                    })
                    .transpose()?;
                let created = client
                    .create_environment(&models::CreateEnvironment {
                        name: name.clone(),
                        description: description.clone(),
                        iam_test_key,
                        iam_app_secret,
                    })
                    .await?;
                store
                    .state
                    .test_keys
                    .insert(slot(&url, Some(created.environment.id)), created.key);
                store.save()?;
                output(cli, &created.environment)?;
                suggest(
                    cli,
                    &format!(
                        "Test key saved. Configure a test IAM app secret with remind --test {} configure-iam if omitted, then auth login --org <test-org>. This sandbox allows at most 100 retained reminders.",
                        created.environment.id
                    ),
                );
            }
            Environment::List {
                include_deleted,
                after,
                limit,
            } => output(
                cli,
                &client
                    .environments(*include_deleted, *after, *limit)
                    .await?,
            )?,
            Environment::Get { id } => output(cli, &client.environment(*id).await?)?,
            Environment::Key { id } | Environment::Rotate { id } | Environment::Restore { id } => {
                let key = match command {
                    Environment::Key { .. } => client.environment_key(*id).await?,
                    Environment::Rotate { .. } => client.rotate_environment_key(*id).await?,
                    _ => client.restore_environment(*id).await?,
                };
                store
                    .state
                    .test_keys
                    .insert(slot(&url, Some(*id)), key.key.clone());
                store.save()?;
                if matches!(command, Environment::Key { .. }) {
                    output(cli, &key)?;
                } else {
                    output(
                        cli,
                        &serde_json::json!({"environment_id":id,"status":"key_saved"}),
                    )?;
                }
            }
            Environment::Delete { id } => {
                client.delete_environment(*id).await?;
                store.state.test_keys.remove(&slot(&url, Some(*id)));
                store.state.sessions.remove(&slot(&url, Some(*id)));
                store.save()?;
                output(
                    cli,
                    &serde_json::json!({"environment_id":id,"status":"deleted","recovery_days":30}),
                )?;
            }
            Environment::Import { id, key_stdin } => {
                if cli.test.is_some() {
                    bail!("import a key without --test, then use --test <id> on ordinary commands");
                }
                let key = read_secret("Remind test-environment key: ", *key_stdin)?;
                let environment = client
                    .with_test_environment(key.clone())?
                    .current_environment()
                    .await?;
                if environment.id != *id {
                    bail!("this key belongs to a different environment; nothing was saved");
                }
                store.state.test_keys.insert(slot(&url, Some(*id)), key);
                store.save()?;
                output(cli, &environment)?;
            }
            Environment::Forget { id } => {
                store.state.test_keys.remove(&slot(&url, Some(*id)));
                store.state.sessions.remove(&slot(&url, Some(*id)));
                store.save()?;
                output(
                    cli,
                    &serde_json::json!({"environment_id":id,"status":"forgotten_locally"}),
                )?;
            }
        },
        Command::ConfigureIam {
            iam_app_secret_file,
        } => {
            if cli.test.is_none() {
                bail!(
                    "this action is only possible for a test environment; use remind --test <test_id> configure-iam"
                );
            }
            let secret = secret_file_or_prompt(iam_app_secret_file, "IAM test-only app secret: ")?;
            client.configure_environment_iam(&secret).await?;
            output(cli, &serde_json::json!({"status":"iam_configured"}))?;
        }
        Command::TestInfo => output(cli, &client.current_environment().await?)?,
        Command::Clean => {
            client.clean_environment().await?;
            output(
                cli,
                &serde_json::json!({"status":"cleaned","iam_environment_unchanged":true}),
            )?;
            suggest(
                cli,
                "Sandbox emptied. Webhook subscriptions are optional; add one when outbound delivery is wanted.",
            );
        }
        Command::Config { command } => match command {
            Config::Show => output(
                cli,
                &serde_json::json!({"url":store.state.url,"home":store.home_dir().display().to_string(),"auto_update":store.state.auto_update,"session_count":store.state.sessions.len(),"saved_test_environment_count":store.state.test_keys.len()}),
            )?,
            Config::SetUrl { service_url } => {
                Client::new(service_url)?;
                store.state.url = service_url.trim_end_matches('/').into();
                store.save()?;
                output(cli, &serde_json::json!({"url":store.state.url}))?;
            }
            Config::Home { location } => {
                Store::configure_home(location)?;
                output(cli, &serde_json::json!({"home": location}))?;
            }
            Config::AutoUpdate { value } => {
                store.state.auto_update = matches!(value, Toggle::On);
                store.save()?;
                output(
                    cli,
                    &serde_json::json!({"auto_update":store.state.auto_update}),
                )?;
            }
        },
        Command::Update { .. } => {}
        Command::Health { ready } => output(cli, &client.health(*ready).await?)?,
    }
    Ok(())
}

async fn login_with_token(
    cli: &Cli,
    store: &mut Store,
    client: &Client,
    session_slot: &str,
    token: Secret,
) -> anyhow::Result<()> {
    let mutation = match &cli.idempotency_key {
        Some(key) => Mutation::with_key(key)?,
        None => Mutation::new(),
    };
    let session = client.login(&token, &mutation).await?;
    let org = cli.org.clone().or_else(|| session.org_id.clone()).context(
        "an organization is required; use an organization-bound IAM SLT and --org <org>",
    )?;
    let identity = client
        .with_session(session.access_token.clone(), org.clone())?
        .me()
        .await?;
    save_session(store, session_slot, session, org)?;
    output(cli, &identity)?;
    suggest(
        cli,
        "Logged in. Optional next step: remind webhook subscribe <url>.",
    );
    Ok(())
}

// Persist the operation identity before consuming a rotating credential. On an
// uncertain response (including a process crash), the next invocation replays
// exactly this operation with the old token instead of revoking its family.
async fn refresh_session(
    store: &mut Store,
    key: &str,
    client: &Client,
    requested: Option<&Mutation>,
) -> anyhow::Result<()> {
    let stored = store
        .state
        .sessions
        .get_mut(key)
        .context("no saved session")?;
    let mutation = match &stored.pending_refresh_key {
        Some(pending) => Mutation::with_key(pending.clone())?,
        None => requested.cloned().unwrap_or_default(),
    };
    stored.pending_refresh_key = Some(mutation.key().into());
    let token = stored.session.refresh_token.clone();
    let org = stored.org.clone();
    store.save()?;
    let session = client.refresh(&token, &mutation).await?;
    save_session(store, key, session, org)
}

fn save_session(
    store: &mut Store,
    key: &str,
    session: models::Session,
    org: String,
) -> anyhow::Result<()> {
    let expires_at = chrono::Utc::now()
        .timestamp()
        .checked_add(session.expires_in)
        .context("invalid session expiry")?;
    store.state.sessions.insert(
        key.into(),
        StoredSession {
            session,
            org,
            expires_at,
            pending_refresh_key: None,
        },
    );
    store.save()
}
fn read_secret(prompt: &str, stdin: bool) -> anyhow::Result<Secret> {
    let text = if stdin {
        let mut value = String::new();
        std::io::stdin().take(65537).read_to_string(&mut value)?;
        value
    } else {
        if !std::io::stdin().is_terminal() {
            bail!(
                "no interactive terminal; use the command's --slt-stdin, --secret-stdin or --key-stdin option"
            );
        }
        rpassword::prompt_password(prompt)?
    };
    if text.trim().is_empty() || text.len() > 65536 {
        bail!("secret input is empty or too large");
    }
    Ok(Secret::new(text.trim()))
}
fn secret_file_or_prompt(
    path: &Option<std::path::PathBuf>,
    prompt: &str,
) -> anyhow::Result<Secret> {
    match path {
        Some(path) => {
            let text = std::fs::read_to_string(path).context("could not read the secret file")?;
            if text.trim().is_empty() || text.len() > 65536 {
                bail!("secret file is empty or too large");
            }
            Ok(Secret::new(text.trim()))
        }
        None => read_secret(prompt, false),
    }
}
fn output(cli: &Cli, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let value = serde_json::to_value(value)?;
    if cli.json {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}
fn suggest(cli: &Cli, text: &str) {
    if !cli.json {
        if let Some(id) = cli.test {
            eprintln!("{text}\nContinue using --test {id} for this sandbox.");
        } else {
            eprintln!("{text}");
        }
    }
}
