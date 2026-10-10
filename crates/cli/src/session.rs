//! The invocation context and authenticated calls: the saved sign-in is refreshed when less
//! than a minute is left, single-flight under the state lock (a used refresh token ends the
//! whole sign-in, so two processes must never present the same one), and a request Remind
//! answers with 401 is retried once after a refresh.
use crate::{
    output::{CliError, EXIT_AUTH, EXIT_USAGE, Output},
    state::{Home, Snapshot, State, StoredSignIn, slot},
};
use silicon_remind_client::{
    Client, Error as ClientError, Refusal, Secret,
    accounts::{self, SignIn, Tokens},
};
use uuid::Uuid;

/// Refresh when the access token has less than this many seconds left.
pub const REFRESH_MARGIN: i64 = 60;
/// How a Silicon signs in, in one line.
pub const SILICON_SIGN_IN: &str =
    "silicon-accounts login --app remind -q | remind login --slt-stdin";

/// 401 codes a refresh cannot cure; everything else gets one refresh and one retry.
const FINAL_401: &[&str] = &[
    "account_deleted",
    "token_kind_mismatch",
    "token_wrong_audience",
    "legacy_token_rejected",
    "proof_as_bearer",
    "proof_not_accepted",
    "proof_invalid",
    "test_key_invalid",
];

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    accounts::now()
}

/// Everything one invocation needs to know about where it runs.
pub struct Ctx {
    /// Output mode.
    pub out: Output,
    home: Result<Home, String>,
    /// Remind API origin, without a trailing slash.
    pub url: String,
    /// Silicon Accounts origin for new sign-ins, without a trailing slash.
    pub accounts_url: String,
    /// Remind's app id at Silicon Accounts (its public `client_id`).
    pub app_id: String,
    /// The test environment this command runs in, if any.
    pub test: Option<Uuid>,
    /// Telemetry is on.
    pub telemetry: bool,
    /// `--idempotency-key`.
    pub idempotency_key: Option<String>,
    /// The state as it was when the command started.
    pub snapshot: Snapshot,
}

/// Command-line inputs for [`Ctx::new`].
pub struct CtxInputs<'a> {
    pub json: bool,
    pub url: Option<&'a str>,
    pub accounts_url: Option<&'a str>,
    pub test: Option<Uuid>,
    pub production: bool,
    pub idempotency_key: Option<String>,
}

impl Ctx {
    /// Resolves home, origins, app id and test environment. Never writes anything.
    pub fn new(inputs: CtxInputs<'_>) -> Self {
        let home = Home::resolve().map_err(|error| format!("{error:#}"));
        let snapshot = home.as_ref().map_or_else(
            |_| Snapshot {
                state: State::default(),
                exists: false,
                legacy_sign_ins: false,
                legacy: false,
                unreadable: None,
            },
            Home::read,
        );
        let url = inputs
            .url
            .map(str::to_owned)
            .unwrap_or_else(|| snapshot.state.url.clone())
            .trim()
            .trim_end_matches('/')
            .to_owned();
        let accounts_url = inputs
            .accounts_url
            .map(str::to_owned)
            .or_else(|| snapshot.state.accounts_url.clone())
            .unwrap_or_else(|| accounts::DEFAULT_ACCOUNTS_URL.to_owned())
            .trim()
            .trim_end_matches('/')
            .to_owned();
        let app_id = std::env::var("REMIND_APP_ID")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| accounts::APP_ID.to_owned());
        let test = if inputs.production {
            None
        } else {
            inputs
                .test
                .or_else(|| snapshot.state.selected_tests.get(&url).copied())
        };
        let telemetry = snapshot.state.telemetry
            && std::env::var("REMIND_TELEMETRY_ENABLED").as_deref() != Ok("false");
        Self {
            out: Output { json: inputs.json },
            home,
            url,
            accounts_url,
            app_id,
            test,
            telemetry,
            idempotency_key: inputs.idempotency_key,
            snapshot,
        }
    }

    /// The selected home, or why there is none.
    pub fn home(&self) -> anyhow::Result<&Home> {
        self.home.as_ref().map_err(|reason| {
            CliError::new(
                "home_unavailable",
                EXIT_USAGE,
                format!("Remind has nowhere to keep its state: {reason}."),
                "Set SILICON_HOME (or HOME) to an existing directory.",
            )
            .into()
        })
    }

    /// This command's own sign-in slot.
    pub fn slot(&self) -> String {
        slot(&self.url, self.test)
    }

    /// A Remind client for this origin with telemetry set and, inside a test environment, its
    /// key attached. No credential.
    pub fn client(&self) -> anyhow::Result<Client> {
        let client = Client::new(&self.url)
            .map_err(|error| bad_origin("--url / REMIND_URL", &error))?
            .with_telemetry(self.telemetry);
        match self.test {
            Some(id) => Ok(client.with_test_environment(self.test_key(id)?)?),
            None => Ok(client),
        }
    }

    /// A client for production on this origin, ignoring any test environment.
    pub fn production_client(&self) -> anyhow::Result<Client> {
        Ok(Client::new(&self.url)
            .map_err(|error| bad_origin("--url / REMIND_URL", &error))?
            .with_telemetry(self.telemetry))
    }

    /// The saved key of test environment `id` on this origin.
    pub fn test_key(&self, id: Uuid) -> anyhow::Result<Secret> {
        self.snapshot
            .state
            .test_keys
            .get(&slot(&self.url, Some(id)))
            .cloned()
            .ok_or_else(|| {
                CliError::usage(
                    format!("No key is saved here for test environment {id} on {}.", self.url),
                    format!(
                        "Fetch it with `remind env key {id}` (signed in, without --test), or save a shared one with `remind env import {id} --key-stdin`."
                    ),
                )
                .into()
            })
    }

    /// Signs in to this command's Silicon Accounts as Remind.
    pub fn sign_in_at(&self, accounts_url: &str, app_id: &str) -> anyhow::Result<SignIn> {
        Ok(SignIn::new(accounts_url, app_id)
            .map_err(|error| bad_origin("--accounts-url / ACCOUNTS_URL", &error))?
            .with_telemetry(self.telemetry))
    }

    /// The sign-in this command uses: its own slot, else (inside a test environment) the
    /// production sign-in of the same origin. The flag says the fallback was taken.
    pub fn effective(&self, state: &State) -> Option<(String, StoredSignIn, bool)> {
        self.effective_for(state, self.test)
    }

    /// [`Ctx::effective`] for an explicit test environment (`None`: production).
    pub fn effective_for(
        &self,
        state: &State,
        test: Option<Uuid>,
    ) -> Option<(String, StoredSignIn, bool)> {
        let own = slot(&self.url, test);
        if let Some(stored) = state.sign_ins.get(&own) {
            return Some((own, stored.clone(), false));
        }
        if test.is_some() {
            let production = slot(&self.url, None);
            if let Some(stored) = state.sign_ins.get(&production) {
                return Some((production, stored.clone(), true));
            }
        }
        None
    }

    /// The idempotency key for this invocation's mutation.
    pub fn mutation(&self) -> anyhow::Result<silicon_remind_client::Mutation> {
        Ok(match &self.idempotency_key {
            Some(key) => silicon_remind_client::Mutation::with_key(key)?,
            None => silicon_remind_client::Mutation::new(),
        })
    }

    /// The "not signed in" failure, explaining an outdated saved sign-in when there is one.
    pub fn not_signed_in(&self) -> anyhow::Error {
        let place = match self.test {
            Some(id) => format!("{} (test environment {id})", self.url),
            None => self.url.clone(),
        };
        let message = if self.snapshot.legacy_sign_ins {
            format!(
                "Not signed in to Remind at {place}: the sign-in saved by an earlier Remind no longer works."
            )
        } else {
            format!("Not signed in to Remind at {place}.")
        };
        CliError::new(
            "not_signed_in",
            EXIT_AUTH,
            message,
            format!("Sign in: `remind login` (Carbons) or `{SILICON_SIGN_IN}` (Silicons)."),
        )
        .into()
    }
}

fn bad_origin(source: &str, error: &ClientError) -> anyhow::Error {
    CliError::usage(
        format!("{} (from {source}).", error.message().trim_end_matches('.')),
        "Use an https origin such as https://backend.remind.teamofsilicons.com; plain http works only for localhost, 127.0.0.1 or ::1.",
    )
    .into()
}

/// A saved sign-in made from fresh tokens.
pub fn stored_from(tokens: Tokens, accounts_url: &str, app_id: &str, method: &str) -> StoredSignIn {
    StoredSignIn {
        accounts_url: accounts_url.trim_end_matches('/').to_owned(),
        app_id: app_id.to_owned(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
        refresh_expires_at: tokens.refresh_expires_at,
        scope: tokens.scope,
        account: tokens.account,
        method: method.to_owned(),
        signed_in_at: now(),
        refreshed_at: None,
    }
}

/// The saved sign-in after a refresh: new tokens, the account's current id and profile.
fn merged(current: &StoredSignIn, tokens: Tokens) -> anyhow::Result<StoredSignIn> {
    if tokens.account.uuid != current.account.uuid {
        anyhow::bail!(
            "Silicon Accounts refreshed the sign-in of {} as another account ({}); sign in again.",
            current.account.id,
            tokens.account.id
        );
    }
    let mut next = current.clone();
    next.access_token = tokens.access_token;
    next.refresh_token = tokens.refresh_token;
    next.expires_at = tokens.expires_at;
    if tokens.refresh_expires_at.is_some() {
        next.refresh_expires_at = tokens.refresh_expires_at;
    }
    if !tokens.scope.is_empty() {
        next.scope = tokens.scope;
    }
    next.account = tokens.account;
    next.refreshed_at = Some(now());
    Ok(next)
}

/// Refreshes the sign-in in `slot` under the state lock. When another process refreshed it
/// while this one waited for the lock, its result is used instead of refreshing again.
/// `force` refreshes even a fresh token (after Remind refused it).
pub async fn refresh_single_flight(
    ctx: &Ctx,
    slot: &str,
    seen: &StoredSignIn,
    force: bool,
) -> anyhow::Result<StoredSignIn> {
    let home = ctx.home()?.clone();
    let locked = tokio::task::spawn_blocking(move || home.lock()).await??;
    let mut snapshot = locked.read();
    let Some(current) = snapshot.state.sign_ins.get(slot).cloned() else {
        return Err(ctx.not_signed_in());
    };
    if current.account.uuid != seen.account.uuid {
        return Err(CliError::new(
            "sign_in_changed",
            EXIT_AUTH,
            format!(
                "The saved sign-in changed while this command ran: it is now {}.",
                current.account.id
            ),
            "Run the command again.",
        )
        .into());
    }
    let now = now();
    let refreshed_elsewhere = current.access_token != seen.access_token;
    if (refreshed_elsewhere || !force) && current.fresh(now, REFRESH_MARGIN) {
        return Ok(current);
    }
    let sign_in = ctx.sign_in_at(&current.accounts_url, &current.app_id)?;
    match sign_in.refresh(&current.refresh_token).await {
        Ok(tokens) => {
            let updated = merged(&current, tokens)?;
            snapshot
                .state
                .sign_ins
                .insert(slot.to_owned(), updated.clone());
            for notice in locked.write(&snapshot)? {
                ctx.out.warn(&notice);
            }
            Ok(updated)
        }
        Err(
            error @ ClientError::SignInRefused {
                refusal: Refusal::SignInEnded,
                ..
            },
        ) => {
            snapshot.state.sign_ins.remove(slot);
            locked.write(&snapshot)?;
            Err(CliError::new(
                "sign_in_ended",
                EXIT_AUTH,
                format!(
                    "The sign-in of {} has ended, so it was removed from this machine: {}",
                    current.account.id,
                    error.message()
                ),
                format!(
                    "Sign in again: `remind login` (Carbons) or `{SILICON_SIGN_IN}` (Silicons)."
                ),
            )
            .into())
        }
        Err(error) => Err(error.into()),
    }
}

/// A signed-in Remind client that refreshes itself.
pub struct Authed<'a> {
    ctx: &'a Ctx,
    slot: String,
    sign_in: StoredSignIn,
    base: Client,
}

impl<'a> Authed<'a> {
    /// Loads this command's sign-in (inside its test environment, if any) and refreshes it
    /// when less than a minute is left.
    pub async fn new(ctx: &'a Ctx) -> anyhow::Result<Self> {
        Self::scoped(ctx, ctx.test).await
    }

    /// The production sign-in and a production client, whatever test environment is selected
    /// (test environments are managed from production).
    pub async fn production(ctx: &'a Ctx) -> anyhow::Result<Self> {
        Self::scoped(ctx, None).await
    }

    async fn scoped(ctx: &'a Ctx, test: Option<Uuid>) -> anyhow::Result<Self> {
        ctx.home()?;
        let base = if test.is_some() {
            ctx.client()?
        } else {
            ctx.production_client()?
        };
        let Some((slot, sign_in, _)) = ctx.effective_for(&ctx.snapshot.state, test) else {
            return Err(ctx.not_signed_in());
        };
        let mut authed = Self {
            ctx,
            slot,
            sign_in,
            base,
        };
        if !authed.sign_in.fresh(now(), REFRESH_MARGIN) {
            authed.sign_in =
                refresh_single_flight(ctx, &authed.slot, &authed.sign_in, false).await?;
        }
        Ok(authed)
    }

    /// The signed-in account.
    pub fn sign_in(&self) -> &StoredSignIn {
        &self.sign_in
    }

    fn client(&self) -> anyhow::Result<Client> {
        Ok(self.base.with_session(self.sign_in.access_token.clone())?)
    }

    /// Runs `f` with a signed-in client; on a 401 that a refresh can cure, refreshes once and
    /// runs it again.
    pub async fn call<T>(
        &mut self,
        f: impl AsyncFn(&Client) -> silicon_remind_client::Result<T>,
    ) -> anyhow::Result<T> {
        let client = self.client()?;
        match f(&client).await {
            Err(ClientError::Api {
                status: 401, code, ..
            }) if !FINAL_401.contains(&code.as_str()) => {
                self.sign_in =
                    refresh_single_flight(self.ctx, &self.slot, &self.sign_in, true).await?;
                let client = self.client()?;
                Ok(f(&client).await?)
            }
            other => Ok(other?),
        }
    }
}
