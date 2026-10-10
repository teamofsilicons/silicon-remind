//! `remind env …`: Remind's own test environments. They are managed from production (the
//! production sign-in, no test key), and used by adding `--test <id>` to any command, or by
//! selecting one with `remind env use <id>`.
use crate::{
    args::Environment,
    commands::{key_slot, read_secret, save},
    output::{CliError, EXIT_OK},
    session::{Authed, Ctx},
    state::StoredSignIn,
};
use serde_json::json;
use silicon_remind_client::models;
use uuid::Uuid;

/// Runs `remind env …`.
pub async fn run(ctx: &Ctx, command: &Environment) -> anyhow::Result<u8> {
    match command {
        Environment::Create { name, description } => {
            let input = models::CreateEnvironment {
                name: name.clone(),
                description: description.clone(),
            };
            let mut authed = manage(ctx).await?;
            let created = authed
                .call(async |c| c.create_environment(&input).await)
                .await?;
            let id = created.environment.id;
            let (key, label) = (created.key.clone(), created.environment.name.clone());
            let slot = key_slot(ctx, id);
            save(ctx, move |state| {
                state.test_keys.insert(slot.clone(), key);
                state.test_names.insert(slot, label);
            })?;
            ctx.out.result(&created.environment)?;
            ctx.out.suggest(&format!(
                "Created; its key is saved here. Use it: remind --test {id} <command> (or remind env use {id}). Share it: remind env key {id}. Test environments hold at most 100 reminders."
            ));
        }
        Environment::List {
            include_deleted,
            after,
            limit,
        } => {
            let mut authed = manage(ctx).await?;
            let page = authed
                .call(async |c| c.environments(*include_deleted, *after, *limit).await)
                .await?;
            ctx.out.result(&page)?;
        }
        Environment::Get { id } => {
            let mut authed = manage(ctx).await?;
            ctx.out
                .result(&authed.call(async |c| c.environment(*id).await).await?)?;
        }
        Environment::Key { id } | Environment::Rotate { id } | Environment::Restore { id } => {
            let mut authed = manage(ctx).await?;
            let key = match command {
                Environment::Key { .. } => {
                    authed.call(async |c| c.environment_key(*id).await).await?
                }
                Environment::Rotate { .. } => {
                    authed
                        .call(async |c| c.rotate_environment_key(*id).await)
                        .await?
                }
                _ => {
                    authed
                        .call(async |c| c.restore_environment(*id).await)
                        .await?
                }
            };
            let slot = key_slot(ctx, *id);
            let saved = key.key.clone();
            save(ctx, move |state| {
                state.test_keys.insert(slot, saved);
            })?;
            match command {
                Environment::Key { .. } => ctx.out.result(&key)?,
                Environment::Rotate { .. } => ctx.out.result(
                    &json!({"environment_id": id, "status": "key_rotated", "key_saved": true}),
                )?,
                _ => ctx.out.result(
                    &json!({"environment_id": id, "status": "restored", "key_saved": true}),
                )?,
            }
        }
        Environment::Delete { id } => {
            let mut authed = manage(ctx).await?;
            authed
                .call(async |c| c.delete_environment(*id).await)
                .await?;
            forget(ctx, *id).await?;
            ctx.out
                .result(&json!({"environment_id": id, "status": "deleted", "recovery_days": 30}))?;
            ctx.out.suggest(&format!(
                "Restore it within 30 days with remind env restore {id}."
            ));
        }
        Environment::Import { id, key_stdin } => {
            refuse_explicit_test(ctx, "env import")?;
            let key = read_secret("Remind test environment key: ", *key_stdin, "--key-stdin")?;
            if !silicon_remind_client::is_test_environment_key(key.expose()) {
                return Err(CliError::usage(
                    "That is not a Remind test environment key: keys are 32 letters and digits.",
                    "Ask whoever shared it to run remind env key <test_id> again.",
                )
                .into());
            }
            let environment = ctx
                .production_client()?
                .with_test_environment(key.clone())?
                .current_environment()
                .await?;
            if environment.id != *id {
                return Err(CliError::usage(
                    format!(
                        "This key belongs to test environment {}, not {id}; nothing was saved.",
                        environment.id
                    ),
                    format!(
                        "Import it as remind env import {} --key-stdin.",
                        environment.id
                    ),
                )
                .into());
            }
            let slot = key_slot(ctx, *id);
            let label = environment.name.clone();
            save(ctx, move |state| {
                state.test_keys.insert(slot.clone(), key);
                state.test_names.insert(slot, label);
            })?;
            ctx.out.result(&environment)?;
            ctx.out
                .suggest(&format!("Saved. Use it: remind --test {id} <command>."));
        }
        Environment::Forget { id } => {
            forget(ctx, *id).await?;
            ctx.out
                .result(&json!({"environment_id": id, "status": "forgotten_locally"}))?;
        }
        Environment::Use { id } => {
            let key = ctx.test_key(*id)?;
            let environment = ctx
                .production_client()?
                .with_test_environment(key)?
                .current_environment()
                .await?;
            let (url, slot, label) = (
                ctx.url.clone(),
                key_slot(ctx, *id),
                environment.name.clone(),
            );
            let selected = *id;
            save(ctx, move |state| {
                state.selected_tests.insert(url, selected);
                state.test_names.insert(slot, label);
            })?;
            ctx.out.result(&environment)?;
            ctx.out.suggest(&format!(
                "Every command now runs in {} until remind env exit; one command in production: remind --production <command>.",
                environment.name
            ));
        }
        Environment::Exit => {
            let url = ctx.url.clone();
            save(ctx, move |state| {
                state.selected_tests.remove(&url);
            })?;
            ctx.out.result(&json!({"environment": "production"}))?;
        }
    }
    Ok(EXIT_OK)
}

/// The production sign-in for managing test environments; an explicit --test is refused
/// (a selection made with `remind env use` is simply not used here).
async fn manage(ctx: &Ctx) -> anyhow::Result<Authed<'_>> {
    refuse_explicit_test(ctx, "env")?;
    Authed::production(ctx).await
}

fn refuse_explicit_test(ctx: &Ctx, command: &str) -> anyhow::Result<()> {
    let selected = ctx.snapshot.state.selected_tests.get(&ctx.url).copied();
    if ctx.test.is_some() && ctx.test != selected {
        return Err(CliError::usage(
            format!("`remind {command}` manages test environments from production, so --test does not apply."),
            "Run it without --test.",
        )
        .into());
    }
    Ok(())
}

/// Forgets a test environment here and ends its own sign-in, if it had one.
async fn forget(ctx: &Ctx, id: Uuid) -> anyhow::Result<()> {
    let url = ctx.url.clone();
    let ((ended,), notices) = ctx
        .home()?
        .update(move |state| Ok((state.forget_test(&url, id),)))?;
    for notice in notices {
        ctx.out.warn(&notice);
    }
    if let Some(stored) = ended {
        end_quietly(ctx, &stored).await;
    }
    Ok(())
}

async fn end_quietly(ctx: &Ctx, stored: &StoredSignIn) {
    let Ok(sign_in) = ctx.sign_in_at(&stored.accounts_url, &stored.app_id) else {
        return;
    };
    if sign_in.revoke(&stored.refresh_token).await.is_err() {
        ctx.out.warn(&format!(
            "The test environment's own sign-in ({}) could not be ended at Silicon Accounts; end it from your list of sign-ins on the account site.",
            stored.account.id
        ));
    }
}
