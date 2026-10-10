# silicon-remind-client

A stateless Rust client for Silicon Remind: sign a Carbon or Silicon in with Silicon Accounts,
then create and read reminders, share them, manage webhook subscriptions and test environments.
It has no dependency on the Remind service or its database. The [`remind` CLI](../cli/README.md)
is built only on this crate, so everything the CLI does, your program can do.

```toml
[dependencies]
silicon-remind-client = "0.6"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Rust 1.98 or newer. Version 0.6 speaks Remind's API contract 2 (`/api/v2`) and signs in with
Silicon Accounts; 0.5 and earlier spoke the retired contract 1.

## Two parts

- `accounts::SignIn` signs an account in to Remind at Silicon Accounts, as Remind's own public
  client: no secret, only the app id `remind`. It returns `accounts::Tokens`: an access token
  (30 minutes), a rotating refresh token and the account. Where you keep them is up to you.
- `Client` calls the Remind API with an access token (`with_session`), or, for another app
  reading on an account's behalf, a User verification proof (`with_proof`).
  `with_test_environment` selects a test environment's data without changing any method.

Both are immutable configuration: every `with_*` method returns a new value, nothing is cached,
nothing refreshes by itself, and constructing one makes no network call. Origins must be https;
plain http is accepted only for `localhost`, `127.0.0.1` and `::1`. Redirects are not followed,
requests time out after 30 seconds (connections after 5), and response bodies are capped at
16 MiB.

## Sign in a Silicon

A Silicon mints a short-lived token for Remind (`silicon-accounts login --app remind -q`) and
hands it to your program:

```rust,no_run
use silicon_remind_client::{Client, Mutation, Secret, accounts::{SignIn, DEFAULT_ACCOUNTS_URL}, models};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sign_in = SignIn::new(DEFAULT_ACCOUNTS_URL, "remind")?;
    let slt = Secret::new(std::env::var("REMIND_SLT")?); // slt_…: single use, 2 minutes
    let tokens = sign_in.exchange_slt(&slt).await?;
    println!("signed in as {} ({})", tokens.account.id, tokens.account.uuid);

    let remind = Client::new(silicon_remind_client::DEFAULT_URL)?
        .with_session(tokens.access_token.clone())?;
    let reminder = remind
        .create_reminder(
            &models::CreateScheduleRequest {
                text: "Review the build results".into(),
                kind: models::ScheduleKind::Recurring,
                cron: "*/15 * * * *".into(),
                timezone: "UTC".into(),
            },
            &Mutation::new(),
        )
        .await?;
    println!("created {}", reminder.id);
    Ok(())
}
```

A refused token is an `Error::SignInRefused` whose `refusal` says why:
`SltAlreadyUsed`, `SltExpired`, `SltWrongApp { app }`, `SltUnknown`, `NotAShortLivedToken`
(nothing was sent) or `Other`; its `hint` says to mint a fresh one with
`silicon-accounts login --app remind -q`. Every refused token is used up.

## Sign in a Carbon (device flow)

```rust,no_run
use silicon_remind_client::accounts::{DeviceProgress, SignIn};

async fn carbon(sign_in: &SignIn) -> silicon_remind_client::Result<()> {
    let device = sign_in.start_device(Some("my tool on build-box")).await?;
    println!("Open {} and enter {}", device.verification_uri, device.user_code);
    let tokens = sign_in
        .wait_for_device(&device, |progress| {
            if let DeviceProgress::SlowDown { interval } = progress {
                eprintln!("checking every {interval}s");
            }
        })
        .await?;
    println!("signed in as {}", tokens.account.id);
    Ok(())
}
```

`wait_for_device` polls as Silicon Accounts allows (`interval`, plus 5 seconds after each
`slow_down`), retries passing network failures, and ends with `Refusal::DeviceDenied` or
`Refusal::DeviceExpired` (codes last 10 minutes). Use `poll_device` to drive polling yourself.
Remind's sign-in setup must have device sign-in on (`Refusal::NotAllowed` otherwise).

## Keep the sign-in fresh, end it

```rust,no_run
use silicon_remind_client::{Secret, accounts::SignIn};

async fn renew(sign_in: &SignIn, refresh_token: &Secret) -> silicon_remind_client::Result<()> {
    let tokens = sign_in.refresh(refresh_token).await?; // store tokens.refresh_token BEFORE using it
    let ended = sign_in.revoke(&tokens.refresh_token).await?; // sign-out; false: it was already over
    println!("revoked: {ended}");
    Ok(())
}
```

Refresh tokens rotate on every use, and presenting a used one ends the whole sign-in
(`Refusal::SignInEnded`). Refresh one at a time (the CLI holds a file lock), store the new pair
before using it, and never retry a refresh that may have reached Silicon Accounts with the same
token. `Tokens::expires_at` is in Unix seconds by this machine's clock; refresh a little before.

## Read on someone's behalf (other apps)

```rust,no_run
use silicon_remind_client::{Client, Secret, models};

async fn read_for(proof: Secret) -> silicon_remind_client::Result<()> {
    let remind = Client::new(silicon_remind_client::DEFAULT_URL)?.with_proof(proof)?; // sap_…
    let page = remind.reminders(&models::ListSchedules::default()).await?;
    println!("{} reminders", page.items.len());
    Ok(())
}
```

Remind accepts a User verification proof (`Authorization: Proof sap_…`) issued for receiving app
`remind` with the scope `remind.schedules.read`, from an app its deployment trusts, on the read
methods only: `reminders`, `reminder`, `executions`, `silicons` and `me`. See
[signing in and who sees what](../accounts.md#for-other-apps-reading-for-an-account).

## State and secrets

`Secret` hides its value from `Debug` output and gives it out only through `expose()`. Its
`Serialize` deliberately writes the value (request bodies, your own secure storage); never
serialize tokens into ordinary logs. Inside a test environment you stay the account you signed
in as: `with_test_environment` keeps the credential and only adds the key.

`Mutation::new()` makes a unique idempotency key; `Mutation::with_key` adopts your own (16 to 255
visible ASCII characters). Reuse the same `Mutation` when retrying the exact same create, edit,
status change or report after an uncertain failure. The client sends each call once; retrying is
your decision.

## Method reference

| method | what it does |
| --- | --- |
| `SignIn::new(accounts_url, app_id)` | sign in to `app_id` (normally `"remind"`) at that Silicon Accounts |
| `start_device(label)`, `poll_device(&device)`, `wait_for_device(&device, progress)` | Carbon device sign-in |
| `exchange_slt(&slt)` | Silicon sign-in with a short-lived token |
| `refresh(&refresh_token)` | rotate the pair |
| `revoke(&refresh_token)` | sign out; `true` when something was revoked |
| `Client::new(url)` | client for the Remind API at a pathless origin |
| `with_session(access_token)`, `with_proof(proof)` | attach a credential |
| `with_test_environment(key)`, `with_telemetry(on)` | select a test environment; telemetry |
| `health(ready)`, `versions()` | liveness/readiness; served contracts (no credential needed) |
| `me()` | the calling account as Remind sees it (`Identity`) |
| `login_status()` | `LoginStatus`: `authenticated: false` only for HTTP 401 |
| `create_reminder(input, mutation)` | create a reminder for the signed-in Silicon |
| `reminders(filters)` | `ListSchedules` → `Page<ScheduleResponse>` |
| `reminder(id)` | one reminder you can read |
| `update_reminder(id, patch, mutation)` | change text, cron, timezone, kind or status |
| `set_status(ids, status, mutation)` | pause or resume 1 to 100 reminders, all or nothing |
| `archive_reminder(id)` | archive one of your reminders (45 days) |
| `executions(id, paging)` | delivery history |
| `silicons(after, limit)` | `Page<VisibleSilicon>`: the Silicons you can read, with `relation` |
| `viewers()`, `grant_viewer(target)`, `revoke_viewer(account, silicon)` | sharing (viewer grants) |
| `allowed_accounts(silicon)`, `allow_account(target)`, `disallow_account(account, silicon)` | a Silicon's allow-list |
| `subscribe_webhook(destination)`, `webhooks(silicon)`, `unsubscribe_webhook(id)` | delivery subscriptions |
| `configure_webhook(destination)`, `webhook()`, `disable_webhook()` | older single-endpoint forms |
| `create_environment(input)`, `environments(..)`, `environment(id)` | test environments (from production) |
| `environment_key(id)`, `rotate_environment_key(id)`, `delete_environment(id)`, `restore_environment(id)` | keys and lifecycle |
| `current_environment()`, `clean_environment()` | inside a test environment, by key alone |
| `report(input, mutation)`, `report_status(id)` | bug reports |
| `track(event)` | one best-effort telemetry event (never fails the caller) |

Accounts are `AccountRef { uuid, id, kind }`: key on `uuid` (short, case-sensitive text such as
`zQo`, never an RFC 4122 UUID) and show `id` (`c:ada`, `si:scout`), which can change. A reminder
has `owner`; `silicon_id` is the owner's current id. `AccountTarget { id, silicon_id }` names
another account by `c:`/`si:` id or uuid, and, for a custodian, which of its Silicons the request
is about.

`ListSchedules::default()` selects current reminders; set `section` to `Archived` for the
archive. Pass a page's `next_cursor` unchanged, with the same filters, to get the next page.
`PatchScheduleRequest` fields are `Option`s: `None` keeps the current value.
`CreateScheduleRequest.timezone` is mandatory (an IANA identifier such as `Asia/Kolkata` or
`UTC`); a blank one is refused before anything is sent.

## Test environments

```rust,no_run
use silicon_remind_client::{Client, Secret, models};

async fn try_it(access_token: Secret, key: Secret) -> silicon_remind_client::Result<()> {
    let remind = Client::new("http://127.0.0.1:4181")?.with_session(access_token)?;
    let created = remind
        .create_environment(&models::CreateEnvironment { name: "release-qa".into(), description: None })
        .await?;
    let sandbox = remind.with_test_environment(created.key)?; // same account, isolated data
    let page = sandbox.reminders(&models::ListSchedules::default()).await?;
    println!("{}: {} reminders", created.environment.name, page.items.len());
    let shared = Client::new("http://127.0.0.1:4181")?.with_test_environment(key)?;
    println!("{}", shared.current_environment().await?.name); // by key alone
    Ok(())
}
```

Managing environments needs a client without a key (`Error::Invalid` otherwise);
`current_environment` and `clean_environment` need one. See [testing](../testing-environments.md).

## Errors

Every error has `code()`, `message()`, `hint()`, `status()` and `request_id()`, and
`is_transient()` says whether a later retry may work.

- `Error::Api` is Remind's own error body `{"error":{"code","message","hint"?,"request_id"}}`
  with the HTTP status and `retry_after`. 401: no or ended sign-in (`token_revoked`,
  `token_expired`, `account_deleted`, `token_wrong_audience`…); 403: not allowed
  (`not_reminder_owner`, `silicon_only`, `not_custodian`…); 404 also hides what you cannot see;
  409: a state or idempotency conflict; 410 `api_version_retired`: a contract 1 client.
- `Error::SignInRefused { refusal, message, hint }`: Silicon Accounts refused a sign-in.
- `Error::Accounts`: any other Silicon Accounts failure, or it could not be reached
  (`connection_failed`, `request_timeout`).
- `Error::Invalid` (refused locally), `Error::Transport` (no answer from Remind),
  `Error::Decode` (an answer this client does not understand), `Error::ResponseTooLarge`.

## Versions, reports and telemetry

`versions()` reads `/api/versions`; every request names contract 2 (`X-Remind-API-Version: 2`).
See the [version policy](../version-policy.md). `report` and `report_status` send and track a
bug report. Telemetry is on by default: `with_telemetry(false)` turns off client events and
Remind's request observations for that client. Events go through the Remind API; no Space
Station key ships with the client. See [diagnostics](../diagnostics.md).

The crate never updates itself: update it in your `Cargo.toml` and rebuild.
