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
use session::{Ctx, CtxInputs};
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
    let result = dispatch(&ctx, &cli.command).await;
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
