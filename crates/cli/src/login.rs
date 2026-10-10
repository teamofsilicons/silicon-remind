//! `remind login` (device sign-in for Carbons, short-lived token for Silicons),
//! `remind login status` and `remind logout`.
use crate::{
    args::{LoginArgs, LoginCommand},
    output::{
        CliError, EXIT_AUTH, EXIT_FORBIDDEN, EXIT_INTERRUPTED, EXIT_OK, key_values, rfc3339, when,
    },
    session::{Ctx, SILICON_SIGN_IN, now, stored_from},
    state::StoredSignIn,
};
use serde_json::{Value, json};
use silicon_remind_client::{
    Error as ClientError, Secret,
    accounts::{DeviceProgress, SignIn, Tokens},
    models::{AccountKind, Identity},
};
use std::io::{IsTerminal as _, Read as _};

/// `remind login …`.
pub async fn login(ctx: &Ctx, args: &LoginArgs) -> anyhow::Result<u8> {
    if let Some(LoginCommand::Status { offline }) = &args.command {
        return status(ctx, *offline).await;
    }
    let slt = match (&args.token, &args.slt, args.slt_stdin) {
        (Some(token), _, _) | (None, Some(token), _) => Some(Secret::new(token.trim())),
        (None, None, true) => Some(read_slt_from_stdin()?),
        (None, None, false) => None,
    };
    // Check everything local first: a short-lived token is used up by its first exchange, so
    // make sure the sign-in can be saved and Remind's origin is usable before exchanging it.
    drop(ctx.home()?.lock()?);
    sign_in_client(ctx)?;
    let sign_in = ctx.sign_in_at(&ctx.accounts_url, &ctx.app_id)?;
    let (tokens, method) = match slt {
        Some(slt) => (sign_in.exchange_slt(&slt).await?, "slt"),
        None => match device(ctx, &sign_in, args).await? {
            Some(tokens) => (tokens, "device"),
            None => return Ok(EXIT_OK),
        },
    };
    finish(ctx, &sign_in, tokens, method).await
}

fn read_slt_from_stdin() -> anyhow::Result<Secret> {
    if std::io::stdin().is_terminal() {
        return Ok(Secret::new(
            rpassword::prompt_password("Short-lived token for Remind (slt_…): ")?
                .trim()
                .to_owned(),
        ));
    }
    let mut text = String::new();
    std::io::stdin().take(65_537).read_to_string(&mut text)?;
    let text = text.trim();
    if text.is_empty() || text.len() > 65_536 {
        return Err(CliError::usage(
            "--slt-stdin read nothing usable from standard input.",
            format!("Pipe the token in: `{SILICON_SIGN_IN}`."),
        )
        .into());
    }
    Ok(Secret::new(text))
}

/// The device sign-in. `None` when this home is already signed in and `--force` is absent.
async fn device(ctx: &Ctx, sign_in: &SignIn, args: &LoginArgs) -> anyhow::Result<Option<Tokens>> {
    let slot = ctx.sign_in_slot();
    if let Some(stored) = ctx.snapshot.state.sign_ins.get(&slot)
        && !args.force
        && !stored.ended(now())
    {
        let mut body = status_json(ctx, stored, false, false, None);
        body["already_signed_in"] = json!(true);
        ctx.out.either(
            &body,
            &format!(
                "Already signed in to Remind as {}.\nSign in again with `remind login --force`, or end this sign-in with `remind logout`.",
                who(stored)
            ),
        )?;
        return Ok(None);
    }
    let label = args.label.clone().unwrap_or_else(default_label);
    let started = sign_in.start_device(Some(&label)).await?;
    let opened = args.open && open_browser(started.browser_url());
    ctx.out.progress(
        &json!({
            "event": "device_code",
            "user_code": started.user_code,
            "verification_uri": started.verification_uri,
            "verification_uri_complete": started.verification_uri_complete,
            "expires_at": rfc3339(started.expires_at),
            "interval": started.interval,
            "browser_opened": opened,
        }),
        &format!(
            "To sign in to Remind, open {} and enter the code\n\n    {}\n\n{}Waiting for approval; the code expires in {} minutes (Ctrl-C to cancel).\nSilicons sign in with `{SILICON_SIGN_IN}` instead.",
            started.verification_uri,
            started.user_code,
            if opened {
                "Your browser was opened on that page.\n".to_owned()
            } else {
                format!("(Direct link: {})\n", started.browser_url())
            },
            started.expires_in.div_ceil(60),
        ),
    );
    let out = ctx.out;
    let wait = sign_in.wait_for_device(&started, |progress| match progress {
        DeviceProgress::SlowDown { interval } => out.progress(
            &json!({"event": "slow_down", "interval": interval}),
            &format!(
                "Silicon Accounts asked to check less often; checking every {interval} seconds."
            ),
        ),
        DeviceProgress::Retrying { message } => out.progress(
            &json!({"event": "retrying", "message": message}),
            &format!("warning: {message} Still waiting."),
        ),
        _ => {}
    });
    tokio::select! {
        tokens = wait => Ok(Some(tokens?)),
        _ = tokio::signal::ctrl_c() => Err(CliError::new(
            "interrupted",
            EXIT_INTERRUPTED,
            format!("Stopped waiting for approval of the code {}.", started.user_code),
            "Run `remind login` again for a new code.",
        )
        .into()),
    }
}

/// Checks the new tokens with Remind, saves them (replacing and ending any earlier sign-in
/// in this slot) and reports who signed in.
async fn finish(ctx: &Ctx, sign_in: &SignIn, tokens: Tokens, method: &str) -> anyhow::Result<u8> {
    let mut stored = stored_from(tokens, sign_in.accounts_url(), sign_in.app_id(), method);
    let client = sign_in_client(ctx)?.with_session(stored.access_token.clone())?;
    let (identity, warning) = match client.me().await {
        Ok(identity) => (Some(identity), None),
        Err(
            error @ ClientError::Api {
                status: 401 | 403, ..
            },
        ) => {
            let _ = sign_in.revoke(&stored.refresh_token).await;
            return Err(CliError::new(
                "token_refused_by_remind",
                if error.status() == Some(403) { EXIT_FORBIDDEN } else { EXIT_AUTH },
                format!(
                    "Silicon Accounts at {} signed {} in, but Remind at {} refused the token ({}: {}). Nothing was saved and that sign-in was ended.",
                    sign_in.accounts_url(),
                    stored.account.id,
                    ctx.url,
                    error.code(),
                    error.message()
                ),
                "Check that --url / REMIND_URL and --accounts-url / ACCOUNTS_URL name the same Remind deployment, then sign in again.",
            )
            .into());
        }
        Err(error) => (None, Some(error.to_string())),
    };
    if let Some(identity) = &identity {
        stored.account.id.clone_from(&identity.id);
        if !identity.display_name.is_empty() {
            stored
                .account
                .display_name
                .clone_from(&identity.display_name);
        }
    }
    let slot = ctx.sign_in_slot();
    let saved = stored.clone();
    let (previous, notices) = ctx
        .home()?
        .update(move |state| Ok(state.sign_ins.insert(slot, saved)))?;
    for notice in notices {
        ctx.out.warn(&notice);
    }
    if let Some(previous) = previous
        && previous.refresh_token != stored.refresh_token
        && let Ok(old) = ctx.sign_in_at(&previous.accounts_url, &previous.app_id)
        && old.revoke(&previous.refresh_token).await.is_err()
    {
        ctx.out.warn(&format!(
            "The previous sign-in here ({}) could not be ended at Silicon Accounts; end it from your list of sign-ins on the account site.",
            previous.account.id
        ));
    }
    if let Some(warning) = &warning {
        ctx.out.warn(&format!(
            "Signed in, but Remind at {} could not confirm it yet: {warning}",
            ctx.url
        ));
    }
    let fallback = ctx.test.is_some() && ctx.explicit_test.is_none();
    let mut body = status_json(
        ctx,
        &stored,
        identity.is_some(),
        fallback,
        identity.as_ref(),
    );
    body["method"] = json!(method);
    if let Some(warning) = warning {
        body["warning"] = json!(warning);
    }
    ctx.out.either(
        &body,
        &signed_in_text(ctx, &stored, identity.is_some(), false),
    )?;
    ctx.out.suggest(&next_steps(&stored));
    Ok(EXIT_OK)
}

/// The Remind client a new sign-in is checked with: inside the test environment named with
/// `--test`, else production (a selected environment uses the production sign-in).
fn sign_in_client(ctx: &Ctx) -> anyhow::Result<silicon_remind_client::Client> {
    if ctx.explicit_test.is_some() {
        ctx.client()
    } else {
        ctx.production_client()
    }
}

fn next_steps(stored: &StoredSignIn) -> String {
    match stored.account.kind {
        AccountKind::Silicon => "Next: remind webhook subscribe <url> (where due reminders go), then remind create --text '…' --cron '0 9 * * *' --timezone <IANA timezone>.".into(),
        AccountKind::Carbon => "Next: remind silicons (the Silicons whose reminders you can read), then remind list --silicon si:<id>.".into(),
    }
}

fn who(stored: &StoredSignIn) -> String {
    if stored.account.display_name.is_empty() {
        stored.account.id.clone()
    } else {
        format!("{} ({})", stored.account.id, stored.account.display_name)
    }
}

/// `{"authenticated":true,…}` for a saved sign-in.
fn status_json(
    ctx: &Ctx,
    stored: &StoredSignIn,
    verified: bool,
    fallback: bool,
    identity: Option<&Identity>,
) -> Value {
    let mut body = json!({
        "authenticated": true,
        "uuid": stored.account.uuid,
        "id": stored.account.id,
        "kind": stored.account.kind.as_str(),
        "expires_at": rfc3339(stored.expires_at),
        "refresh_expires_at": stored.refresh_expires_at.map(rfc3339),
        "verified": verified,
        "url": ctx.url,
        "accounts_url": stored.accounts_url,
        "app_id": stored.app_id,
        "method": stored.method,
    });
    if !stored.account.display_name.is_empty() {
        body["display_name"] = json!(stored.account.display_name);
    }
    if let Some(custodian) = &stored.account.custodian {
        body["custodian"] = json!({"uuid": custodian.uuid, "id": custodian.id});
    }
    if let Some(identity) = identity {
        body["can_manage_reminders"] = json!(identity.can_manage_reminders);
        body["visible_silicons"] = json!(identity.visible_silicons);
    }
    if let Some(id) = ctx.test {
        body["test_environment"] = json!(id);
        if fallback {
            body["uses_production_sign_in"] = json!(true);
        }
    }
    body
}

fn signed_in_text(ctx: &Ctx, stored: &StoredSignIn, verified: bool, fallback: bool) -> String {
    let now = now();
    let mut rows = vec![
        ("uuid", stored.account.uuid.clone()),
        ("remind", ctx.url.clone()),
        ("accounts", stored.accounts_url.clone()),
    ];
    if let Some(custodian) = &stored.account.custodian {
        rows.push(("custodian", custodian.id.clone()));
    }
    if let Some(id) = ctx.test {
        rows.push((
            "test environment",
            if fallback {
                format!("{id} (using the production sign-in)")
            } else {
                id.to_string()
            },
        ));
    }
    rows.push((
        "access token",
        format!("{} (refreshed automatically)", when(stored.expires_at, now)),
    ));
    rows.push((
        "sign-in ends",
        stored
            .refresh_expires_at
            .map_or_else(|| "not stated".into(), |at| when(at, now)),
    ));
    rows.push((
        "verified",
        if verified {
            "yes, Remind accepted it just now"
        } else {
            "no (not checked with Remind)"
        }
        .into(),
    ));
    format!(
        "Signed in to Remind as {}, a {}.\n{}",
        who(stored),
        stored.account.kind.title(),
        key_values(&rows)
    )
}

fn default_label() -> String {
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|host| host.trim().to_owned())
        .filter(|host| !host.is_empty());
    let label = match host {
        Some(host) => format!("remind on {host}"),
        None => format!(
            "remind on {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    };
    label.chars().take(100).collect()
}

fn open_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// What checking a saved sign-in with Remind found.
enum Check {
    /// Remind accepted it just now.
    Verified(Box<(Identity, StoredSignIn)>),
    /// It no longer works: a stable reason and a message.
    Ended(String, String),
    /// It could not be checked (network, Remind or Silicon Accounts unavailable).
    Unverified(String),
}

async fn check(ctx: &Ctx) -> Check {
    let mut authed = match crate::session::Authed::new(ctx).await {
        Ok(authed) => authed,
        Err(error) => return classify(&error),
    };
    match authed.call(async |client| client.me().await).await {
        Ok(identity) => Check::Verified(Box::new((identity, authed.sign_in().clone()))),
        Err(error) => classify(&error),
    }
}

fn classify(error: &anyhow::Error) -> Check {
    if let Some(error) = error.downcast_ref::<CliError>()
        && matches!(
            error.code,
            "sign_in_ended" | "not_signed_in" | "sign_in_changed"
        )
    {
        return Check::Ended(error.code.into(), error.message.clone());
    }
    if let Some(
        error @ ClientError::Api {
            status: 401 | 403, ..
        },
    ) = error.downcast_ref::<ClientError>()
    {
        return Check::Ended(error.code().into(), error.message());
    }
    if let Some(error @ ClientError::SignInRefused { .. }) = error.downcast_ref::<ClientError>() {
        return Check::Ended(error.code().into(), error.message());
    }
    Check::Unverified(format!("{error:#}"))
}

/// `remind login status [--offline]`. With `--json` it always exits 0.
pub async fn status(ctx: &Ctx, offline: bool) -> anyhow::Result<u8> {
    if let Err(error) = ctx.home() {
        return signed_out(ctx, Some(("home_unavailable", error.to_string())));
    }
    if let Some(reason) = &ctx.snapshot.unreadable {
        return signed_out(
            ctx,
            Some((
                "state_unreadable",
                format!(
                    "The saved state could not be read ({reason}); signing in again moves it aside."
                ),
            )),
        );
    }
    let Some((_, stored, fallback)) = ctx.effective(&ctx.snapshot.state) else {
        let reason = ctx.snapshot.legacy_sign_ins.then(|| {
            (
                "sign_in_again",
                "The sign-in saved by an earlier Remind no longer works; sign in again.".to_owned(),
            )
        });
        return signed_out(ctx, reason);
    };
    if stored.ended(now()) {
        return signed_out(
            ctx,
            Some((
                "sign_in_ended",
                format!("The sign-in of {} ended; sign in again.", stored.account.id),
            )),
        );
    }
    if offline {
        let body = status_json(ctx, &stored, false, fallback, None);
        ctx.out
            .either(&body, &signed_in_text(ctx, &stored, false, fallback))?;
        return Ok(EXIT_OK);
    }
    match check(ctx).await {
        Check::Verified(verified) => {
            let (identity, mut current) = *verified;
            if identity.id != current.account.id
                || (!identity.display_name.is_empty()
                    && identity.display_name != current.account.display_name)
            {
                current.account.id.clone_from(&identity.id);
                if !identity.display_name.is_empty() {
                    current
                        .account
                        .display_name
                        .clone_from(&identity.display_name);
                }
                remember_profile(ctx, &current);
            }
            let body = status_json(ctx, &current, true, fallback, Some(&identity));
            ctx.out
                .either(&body, &signed_in_text(ctx, &current, true, fallback))?;
            Ok(EXIT_OK)
        }
        Check::Ended(code, message) => signed_out(ctx, Some((&code, message))),
        Check::Unverified(warning) => {
            ctx.out.warn(&format!(
                "{warning} Reporting the saved sign-in without checking it."
            ));
            let mut body = status_json(ctx, &stored, false, fallback, None);
            body["warning"] = json!(warning);
            ctx.out
                .either(&body, &signed_in_text(ctx, &stored, false, fallback))?;
            Ok(EXIT_OK)
        }
    }
}

/// Saves the account's current id and display name after Remind reported a change.
fn remember_profile(ctx: &Ctx, current: &StoredSignIn) {
    let Ok(home) = ctx.home() else { return };
    let current = current.clone();
    let _ = home.update(move |state| {
        for stored in state.sign_ins.values_mut() {
            if stored.account.uuid == current.account.uuid {
                stored.account.id.clone_from(&current.account.id);
                stored
                    .account
                    .display_name
                    .clone_from(&current.account.display_name);
            }
        }
        Ok(())
    });
}

fn signed_out(ctx: &Ctx, reason: Option<(&str, String)>) -> anyhow::Result<u8> {
    let mut body = json!({"authenticated": false});
    let mut text = match ctx.test {
        Some(id) => format!(
            "Not signed in to Remind at {} (test environment {id}).",
            ctx.url
        ),
        None => format!("Not signed in to Remind at {}.", ctx.url),
    };
    if let Some((code, message)) = reason {
        body["reason"] = json!(code);
        body["message"] = json!(message);
        text = format!("{text}\n{message}");
    }
    ctx.out.either(&body, &text)?;
    ctx.out.suggest(&format!(
        "Sign in: `remind login` (Carbons) or `{SILICON_SIGN_IN}` (Silicons)."
    ));
    Ok(if ctx.out.json { EXIT_OK } else { 1 })
}

/// `remind logout`: ends this machine's sign-in at Silicon Accounts and forgets it.
pub async fn logout(ctx: &Ctx) -> anyhow::Result<u8> {
    let home = ctx.home()?;
    let own = ctx.sign_in_slot();
    let nothing = |ctx: &Ctx| -> anyhow::Result<u8> {
        let (reason, text) = if ctx.explicit_test.is_some()
            && ctx
                .effective_for(&ctx.snapshot.state, ctx.explicit_test)
                .is_some()
        {
            (
                "no_test_environment_sign_in",
                "This test environment has no sign-in of its own; it uses your production sign-in. End that with `remind logout` (without --test).".to_owned(),
            )
        } else {
            (
                "not_signed_in",
                format!(
                    "You were not signed in to Remind at {}; nothing to do.",
                    ctx.url
                ),
            )
        };
        ctx.out
            .either(&json!({"signed_out": false, "reason": reason}), &text)?;
        Ok(EXIT_OK)
    };
    if !ctx.snapshot.state.sign_ins.contains_key(&own) {
        return nothing(ctx);
    }
    let (removed, notices) = home.update(move |state| Ok(state.sign_ins.remove(&own)))?;
    for notice in notices {
        ctx.out.warn(&notice);
    }
    let Some(stored) = removed else {
        return nothing(ctx);
    };
    let revoked = match ctx.sign_in_at(&stored.accounts_url, &stored.app_id) {
        Ok(sign_in) => match sign_in.revoke(&stored.refresh_token).await {
            Ok(revoked) => revoked,
            Err(error) => {
                ctx.out.warn(&format!(
                    "Could not end the sign-in at Silicon Accounts ({}). It is deleted here anyway; end it from your list of sign-ins on the account site.",
                    error.message()
                ));
                false
            }
        },
        Err(error) => {
            ctx.out.warn(&format!("{error:#}"));
            false
        }
    };
    ctx.out.either(
        &json!({
            "signed_out": true,
            "uuid": stored.account.uuid,
            "id": stored.account.id,
            "kind": stored.account.kind.as_str(),
            "revoked": revoked,
        }),
        &format!("Signed out of Remind at {}: {}.", ctx.url, who(&stored)),
    )?;
    ctx.out.suggest(&format!(
        "Sign in again with `remind login` (Carbons) or `{SILICON_SIGN_IN}` (Silicons)."
    ));
    Ok(EXIT_OK)
}
