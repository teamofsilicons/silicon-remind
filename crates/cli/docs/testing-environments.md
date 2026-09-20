# Test a complete reminder workflow

Create or import the `tos>remind` application in an [IAM test environment](https://docs.iam.teamofsilicons.com/api/testing-environments/), then use that application's `app_secret`. Remind discovers the sandbox and starts with empty data. You do not enter an IAM root key or manually pair environments.

## CLI

```sh
remind env use --secret-stdin < /private/remind-app-secret
remind login '<existing-test-public-id-or-test-SLT>'
remind login status --json
remind create --text 'Sandbox check' --cron '*/5 * * * *' --timezone Asia/Kolkata
remind list --json
remind env exit
```

The secret is prompted securely when `--secret-stdin` is omitted. `env use` persists the selected environment per server. Its name and ID appear on stderr at the end of every command, including failures, without contaminating JSON stdout. `--test <id>` overrides the selection for one invocation; `--production` runs one command with the separately saved production session. `env exit` restores production selection. Invalid secrets return an error and never cause production fallback.

## Website

Choose **Use a test environment** on the sign-in screen or in settings. Enter the application's `app_secret`. The banner shows the environment name, signed-in identity, and **Exit testing mode**. Sign in with an IAM test SLT or an existing active test Carbon/Silicon public ID. Exiting restores the production session, or asks you to sign in if it expired. Sessions and secrets are encrypted on the gateway and never returned to browser JavaScript.

## HTTP API

Send `X-Remind-Test-Key: <app_secret>` on every sandbox API request, including login and refresh. The header name is retained for backward compatibility; the new value is an IAM `ask_...` application secret. Keep the secret out of URLs and command history. This example reads it from a protected curl configuration file:

```sh
curl --config /private/remind-test.curl \
  https://backend.remind.teamofsilicons.com/api/v1/testing-environment
```

The configuration file contains `header = "X-Remind-Test-Key: ask_..."` and has mode 0600. Ordinary endpoints additionally require the sandbox user's bearer token and `X-Org-ID`. Login accepts `{"slt":"<test-SLT-or-public-ID>"}` at `/api/v1/auth/login`. Production login accepts only IAM SLTs; unknown/inactive test identities are rejected by IAM.

## Rust client

```rust,no_run
use silicon_remind_client::{Client, Mutation, Secret};
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let app_secret = Secret::new(std::env::var("REMIND_TEST_APP_SECRET")?);
let sandbox = Client::new("https://backend.remind.teamofsilicons.com")?
    .auto_update(false).with_test_environment(app_secret)?;
let environment = sandbox.current_environment().await?;
let session = sandbox.login(&Secret::new("existing-test-user"), &Mutation::new()).await?;
let signed_in = sandbox.with_session(session.access_token, "test-org")?;
let identity = signed_in.me().await?;
# Ok(()) }
```

Selecting an environment clears any bearer on the cloned client. Preserve separate session stores per origin/environment; never attach a production session to a sandbox client.

## Permissions and lifecycle

The application secret selects a world, not a god identity. Carbons can read organization reminders. Silicons can change only their own reminders. Every request revalidates the sandbox application with IAM and ordinary operations use live user authorization. Tokens from another sandbox or production fail the environment check.

Honeycomb owns creation, cleanup, key rotation, retirement, and recovery. Remind accepts its separately authenticated lifecycle operations even while IAM test sessions are disabled. Cleanup runs under an exclusive fence, preserves the participant link, and completes only after all sandbox records are cleared. Replays are safe; stale revisions and generations cannot recreate cleared data. IAM remains the runtime authority for shared readiness and permissions. Workers revalidate IAM before admission and before each outbound test dispatch, including retries. There is no 100-reminder quota for discovered worlds. See the [service integration contract](honeycomb-lifecycle.md).

All reminders, subscriptions, execution history, deleted-reminder records, permissions, idempotency records, webhook receipts, contract usage and audit records use a separate schema in a dedicated testing database. Pool caches hold connections only, not authorization. Cleaning resets environment data; production data is untouched.

## Webhooks and effects

IAM webhook signatures are verified over the complete raw envelope before reading its routing hint. The test root key carried by IAM is never persisted or logged. Routing compares its digest against live IAM metadata, then applies the normalized event only in that sandbox. Event receipts and aggregate revisions make duplicates and stale events safe.

Outbound reminder deliveries are **simulated by default in testing**: the execution completes without contacting the configured URL. To exercise a real test receiver, operators set `REMIND_TEST_WEBHOOK_URLS` to a comma-separated list of exact, canonical receiver URLs. Only those URLs receive sandbox requests. Use receivers dedicated to testing, never production email, SMS, payment, or notification endpoints. Production delivery is unchanged. Inspect execution history to verify processing; receipt semantics are explained in the [webhook guide](webhook-delivery.md).

## Legacy manually paired sandboxes

Existing 32-character Remind keys remain accepted. Legacy environments retain their old 100-reminder quota, 15-day inactivity retirement, and 30-day recovery window. `env create/import/key/rotate/restore`, `configure-iam`, and `clean` are legacy administrative commands. They are not needed for `app_secret` selection and cannot administer IAM-discovered worlds. Use Honeycomb lifecycle administration for new sandboxes.
