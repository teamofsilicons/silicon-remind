# silicon-remind-client

A stateless Rust client for the public Silicon Remind API. It has no dependency
on the backend crate or database. The CLI is a separate consumer of this package;
there is no CLI-only server capability.

## Use from a Rust application

While developing in this repository:

```toml
[dependencies]
silicon-remind-client = { path = "../silicon-remind/crates/client" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Use a registry version only after that version has actually been published.
Rust 1.98 or newer is required. Constructing the client performs no network call.
The service URL is a pathless HTTPS origin; literal loopback HTTP is accepted for
local development. Redirects are not followed, so a redirect cannot move the
request's credentials to another service. Requests time out after 30 seconds,
connection establishment after five seconds, and response bodies are bounded to
16 MiB.

```rust,no_run
use silicon_remind_client::{Client, Mutation, Secret, models};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let anonymous = Client::new("https://backend.remind.teamofsilicons.com")?
        .auto_update(false);
    // Your caller supplies this single-use SLT from IAM, not a password or OTP.
    let slt = Secret::new(std::env::var("REMIND_SLT")?);
    let session = anonymous.login(&slt, &Mutation::new()).await?;
    let client = anonymous.with_session(session.access_token, "tos")?;
    let identity = client.me().await?;
    println!("Signed in as {:?}", identity.public_id);

    let reminder = client.create_reminder(&models::CreateScheduleRequest {
        text: "Review the build results".to_owned(),
        kind: models::ScheduleKind::Recurring,
        cron: "*/15 * * * *".to_owned(),
        timezone: "UTC".to_owned(),
    }, &Mutation::new()).await?;
    println!("Created {}", reminder.id);
    Ok(())
}
```

Webhook subscriptions are optional; use `configure_webhook` or
`subscribe_webhook` when a receiver should receive deliveries. A Carbon may log
in and read reminders but cannot create one.

Use `anonymous.iam().await?` before login to discover the server's `app_id` for
obtaining an IAM SLT. `client.login_status().await?` verifies the attached session
and returns `LoginStatus { authenticated: true, identity: Some(identity) }` on
success. HTTP 401 returns an unauthenticated status with no identity; all other
failures remain errors. The client does not refresh automatically. These methods
respect `with_test_environment` like other public operations. The CLI's
`SILICON_HOME` setting does not affect this stateless package.

## State and secrets

`Client` is immutable configuration: `with_session` and `with_test_environment`
return new clients. It does not save credentials, refresh tokens automatically,
cache IAM authority, or select a user's organization for them. Store session
state in the embedding application. Clone the returned `Session` if both your
state store and a constructed client need its access token.

`Secret` redacts `Debug` output and exposes its value only through `expose()`.
Its `Serialize` implementation deliberately emits the value for requests and
caller-owned secure persistence. Do not serialize sessions into ordinary logs.
`Session` contains rotating access/refresh tokens, expiry seconds, actor and org.
After `refresh`, replace both saved tokens atomically. A fresh `Mutation` means a
new logical exchange; reuse its key when retrying the same exchange.

`Mutation::new()` creates a unique request key. `Mutation::with_key` adopts a
16–255-character visible ASCII key for retries. Reuse only for identical inputs
on the same operation, actor and environment. The client sends each API call
once; retry decisions belong to the caller.

## Method reference

| Method | Input / result |
| --- | --- |
| `health(ready)` | Liveness or readiness `Health` |
| `iam()` | Public `IamInfo`: app ID, IAM URL and optional IAM sandbox UUID; no session needed |
| `login_status()` | `LoginStatus` with verified identity, or `authenticated: false` for HTTP 401 |
| `login(slt, mutation)` | `Session` from IAM SLT |
| `refresh(refresh_token, mutation)` | Successor `Session` |
| `logout(token, mutation)` | Revocation; refresh token revokes its family |
| `me()` | Current `Identity` |
| `create_reminder(input, mutation)` | `CreateScheduleRequest` → `ScheduleResponse` |
| `reminders(filters)` | `ListSchedules` → `Page<ScheduleResponse>` |
| `reminder(id)` | Visible `ScheduleResponse` |
| `update_reminder(id, patch, mutation)` | Partial replacement → updated reminder |
| `set_status(ids, status, mutation)` | Atomic pause/resume → `StatusBatch` |
| `archive_reminder(id)` | Archive owned reminder |
| `executions(id, paging)` | `Page<ExecutionResponse>` |
| `configure_webhook(destination)` | Owner endpoint and signing secret → receipt |
| `webhook()` | Configured URL and version, without secret |
| `disable_webhook()` | Disable owner destination |
| `subscribe_webhook(destination)` | Add an independent subscription |
| `webhooks()` | List active subscriptions |
| `unsubscribe_webhook(id)` | Disable one subscription |
| `silicons(after, limit)` | `Page<Silicon>` for the selected org |
| `create_environment(input)` | `EnvironmentCreated` with root key |
| `environments(include_deleted, after, limit)` | `Page<TestEnvironment>` |
| `environment(id)` | Environment metadata |
| `environment_key(id)` | Active root key |
| `rotate_environment_key(id)` | New root key |
| `delete_environment(id)` | Begin 30-day recovery window |
| `restore_environment(id)` | Restore and return fresh key |
| `current_environment()` | Root-key-only sandbox metadata |
| `configure_environment_iam(&secret)` | Install/rotate a sandbox's IAM test app secret |
| `clean_environment()` | Root-key-only clear of the selected sandbox |

`ListSchedules::default()` selects current reminders. Set `section` to Archived
for retained history. A page's `next_cursor` is opaque; pass it unchanged with
the same filters into the next call. The execution API uses the same cursor/limit
pattern. Environment and Silicon directory pages use UUID `after` cursors.

`PatchScheduleRequest` uses `Option` fields. `None` omits a property and retains
the current value; `Some` replaces it. Clearing required fields is not supported.
The server enforces cron, timezone, text and lifecycle rules. A single reminder
can be paused through `update_reminder`; the batch method handles 1–100 UUIDs
atomically. Archived reminders cannot be edited.

## Test environments

```rust,no_run
use silicon_remind_client::{Client, Secret, Mutation, models};

async fn example() -> silicon_remind_client::Result<()> {
    let base = Client::new("http://127.0.0.1:8086")?.auto_update(false);
    let key = Secret::new(std::env::var("REMIND_TEST_KEY")?);
    let sandbox = base.with_test_environment(key)?;
    let environment = sandbox.current_environment().await?;
    let session = sandbox.login(&Secret::new("slt_from_test_IAM"), &Mutation::new()).await?;
    let signed_in = sandbox.with_session(session.access_token, "test-org")?;
    let reminders = signed_in.reminders(&models::ListSchedules::default()).await?;
    println!("{}: {} reminders", environment.name, reminders.items.len());
    sandbox.clean_environment().await?;
    Ok(())
}
```

`with_test_environment` clears any previously attached bearer and organization,
preventing accidental production credentials from being carried into a sandbox.
Attach the test session afterward. Environment IDs are selectors for your state
store; only the 32-character root key authenticates environment access.
Management methods require a production org session and reject a test-scoped
client locally. `current_environment` and `clean_environment` reject a client
without a test key locally. All ordinary reminder methods use the same paths.

## Errors

Match `Error::Api { status, code, request_id, retry_after, .. }` for server failures.
`401` is missing/expired/mismatched authority; `403` is a permission denial; `404`
also hides other organizations' resources; `409` describes a state or idempotency
conflict. `test_reminder_limit` applies only in sandboxes. `Invalid` is local input
validation, `Transport` means no usable HTTP exchange, `Decode` means an
incompatible response, and `ResponseTooLarge` bounds memory consumption.

For an ambiguous create/update response, repeat the exact request with the same
`Mutation`. Do not retry validation errors unchanged. For backpressure, honor
`retry_after` when supplied and choose a bounded retry policy.

## Automatic dependency maintenance

By default, after an API call finishes, the package may check crates.io if its
process-local last attempt is at least an hour old. Concurrent calls share a
single check. A discovered update runs `cargo update -p silicon-remind-client
--precise <version>` against the enclosing Cargo project. This changes the
lockfile; compiled code changes only after the next build. Idle clients do not
run a timer or daemon. A restart resets the package's in-memory hourly throttle.

Disable maintenance with `.auto_update(false)` or
`SILICON_REMIND_CLIENT_AUTO_UPDATE=false`. Set
`SILICON_REMIND_CLIENT_MANIFEST=/absolute/path/Cargo.toml` when the process working
directory does not identify the intended consuming project. Missing manifests,
unpublished crates, registry failures and Cargo failures do not change API
results. An explicit `updates::maintain` call returns an `UpdateStatus` if a host
application wants to display maintenance progress. The CLI disables package
maintenance and manages its own executable update after each command instead.
