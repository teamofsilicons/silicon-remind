//! `remind`: the Silicon Remind command line, built only on the public
//! `silicon-remind-client` crate. It signs in with Silicon Accounts and keeps its state under
//! `{home}/.remind/`.
mod args;
mod commands;
mod daemon;
mod environments;
mod login;
mod output;
mod session;
mod state;

use args::{Cli, Command};
use clap::Parser as _;
use output::{CliError, EXIT_OK, exit_code, human_error, machine_error};
use serde_json::json;
use session::{Ctx, CtxInputs, now};
use state::{Home, slot};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse().map_err(args::with_timezone_guidance) {
        Ok(cli) => cli,
        Err(error) => {
            let code = u8::try_from(error.exit_code()).unwrap_or(output::EXIT_USAGE);
            let _ = error.print();
            footer(None);
            return std::process::ExitCode::from(code);
        }
    };
    let ctx = Ctx::new(CtxInputs {
        json: cli.json,
        url: cli.url.as_deref(),
        accounts_url: cli.accounts_url.as_deref(),
        test: cli.test,
        production: cli.production,
        idempotency_key: cli.idempotency_key.clone(),
    });
    let started = std::time::Instant::now();
    let result = dispatch(&ctx, &cli.command).await;
    if uses_sign_in(&cli.command) {
        emit_telemetry(
            &ctx,
            result.as_ref().is_ok_and(|code| *code == EXIT_OK),
            started.elapsed(),
        )
        .await;
    }
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            if ctx.out.json {
                eprintln!("{}", machine_error(&error));
            } else {
                eprintln!("{}", human_error(&error));
            }
            exit_code(&error)
        }
    };
    footer(Some(&cli));
    std::process::ExitCode::from(code)
}

async fn dispatch(ctx: &Ctx, command: &Command) -> anyhow::Result<u8> {
    match command {
        Command::Accounts | Command::Iam => commands::accounts(ctx),
        Command::Login(login) => login::login(ctx, login).await,
        Command::Logout => login::logout(ctx).await,
        Command::Daemon { command } => daemon::execute(ctx.out, command),
        Command::Auth { .. } => Err(CliError::usage(
            "`remind auth …` was replaced.",
            "Sign in with `remind login`, check with `remind login status`, see who you are with `remind whoami`, sign out with `remind logout`.",
        )
        .into()),
        Command::ConfigureIam { .. } => Err(CliError::usage(
            "Test environments no longer need an identity service of their own, so there is nothing to configure.",
            "Use remind --test <test_id> <command> with your usual sign-in.",
        )
        .into()),
        Command::Update { .. } => {
            ctx.out.either(
                &json!({"status": "managed", "manager": "silicon-apps", "command": "silicon-apps update remind"}),
                "Silicon Apps keeps Remind up to date, so remind never updates itself. To check right away: silicon-apps update remind",
            )?;
            Ok(EXIT_OK)
        }
        other => commands::run(ctx, other).await,
    }
}

/// Commands that call Remind with the saved sign-in (they also send a telemetry event).
fn uses_sign_in(command: &Command) -> bool {
    use args::Environment as E;
    match command {
        Command::Whoami
        | Command::Create { .. }
        | Command::List { .. }
        | Command::Get { .. }
        | Command::Edit { .. }
        | Command::Pause { .. }
        | Command::Resume { .. }
        | Command::Archive { .. }
        | Command::Executions { .. }
        | Command::Silicons { .. }
        | Command::Share { .. }
        | Command::Allow { .. }
        | Command::Webhook { .. }
        | Command::Report { .. }
        | Command::ReportStatus { .. } => true,
        Command::Env { command } => !matches!(
            command,
            E::Import { .. } | E::Forget { .. } | E::Use { .. } | E::Exit
        ),
        _ => false,
    }
}

/// One bounded, best-effort `command_completed` event through Remind (Space Station), with the
/// saved sign-in when it is still fresh. Sends no arguments, secrets or reminder text.
async fn emit_telemetry(ctx: &Ctx, success: bool, duration: std::time::Duration) {
    if !ctx.telemetry {
        return;
    }
    let Ok(home) = ctx.home() else { return };
    let snapshot = home.read();
    let Some((_, stored, _)) = ctx.effective(&snapshot.state) else {
        return;
    };
    if !stored.fresh(now(), 10) {
        return;
    }
    let Ok(client) = ctx
        .client()
        .and_then(|c| Ok(c.with_session(stored.access_token)?))
    else {
        return;
    };
    client
        .track(&silicon_remind_client::models::TelemetryEvent {
            source: "cli".into(),
            event: "command_completed".into(),
            step: "command".into(),
            success,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            status_code: None,
        })
        .await;
}

/// Names the test environment a command ran in, on stderr, after everything else (also after
/// usage errors, from the raw arguments).
fn footer(cli: Option<&Cli>) {
    let (url, test, production) = match cli {
        Some(cli) => (cli.url.clone(), cli.test, cli.production),
        None => raw_selection(),
    };
    let Ok(home) = Home::resolve() else {
        if let Some(id) = test.filter(|_| !production) {
            eprintln!("Test environment: {id} (local state unavailable)");
        }
        return;
    };
    let snapshot = home.read();
    let url = url
        .unwrap_or_else(|| snapshot.state.url.clone())
        .trim_end_matches('/')
        .to_owned();
    let selected = snapshot.state.selected_tests.get(&url).copied();
    let Some(id) = (if production { None } else { test.or(selected) }) else {
        return;
    };
    let name = snapshot
        .state
        .test_names
        .get(&slot(&url, Some(id)))
        .map_or("unnamed test environment", String::as_str);
    if selected == Some(id) && test.is_none() {
        eprintln!("Test environment: {name} ({id}) · back to production: remind env exit");
    } else {
        eprintln!("Test environment: {name} ({id})");
    }
}

/// `--url`, `--test` and `--production` recovered from arguments clap refused.
fn raw_selection() -> (Option<String>, Option<uuid::Uuid>, bool) {
    let mut args = std::env::args().skip(1);
    let (mut url, mut test, mut production) = (std::env::var("REMIND_URL").ok(), None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => break,
            "--production" => production = true,
            "--url" => url = args.next(),
            "--test" => test = args.next().and_then(|v| v.parse().ok()),
            _ => {
                if let Some(value) = arg.strip_prefix("--test=") {
                    test = value.parse().ok();
                }
                if let Some(value) = arg.strip_prefix("--url=") {
                    url = Some(value.into());
                }
            }
        }
    }
    (url, test, production)
}
