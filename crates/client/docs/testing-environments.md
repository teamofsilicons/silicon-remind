# Remind testing environments

A test environment is the same Remind API, scheduler, delivery worker, and
retention implementation against isolated storage. It begins empty. It can create
real reminders and submit real deliveries to its configured test destination.
It is not a mock-only API and does not use a second set of reminder routes.

## The two credentials and two identities

There are two independent services and root keys:

| Value | Role |
| --- | --- |
| IAM test-environment key | Selects the IAM sandbox on every request Remind makes to IAM |
| Remind test-environment key | Selects and administers the Remind sandbox |
| IAM test Application secret | Authenticates `tos>remind` inside that IAM sandbox |
| Test IAM SLT/access token | Authenticates the Carbon or Silicon performing an ordinary reminder action |
| Remind environment UUID | Public selector for saved local context; it is not a credential |

A production IAM Application secret cannot be substituted for a test-only
Application secret. The backend verifies the supplied IAM root key through the
official client's `environments().current()`, obtains its authoritative ID, then
verifies a supplied test Application credential. Every sandbox login, refresh,
introspection and revocation uses that environment key. Production fallback is
never attempted.

The Remind key is 32 random alphanumeric characters and grants sandbox access
and root-only cleaning. An ordinary reminder operation still runs as its signed-in
IAM test actor: a test Carbon remains read-only, a test Silicon can mutate only
its own reminders, and org boundaries still apply. This is necessary for the
sandbox to prove production permission behavior.

## Prepare IAM

Follow the official [IAM testing-environment guide](https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html).
Create an IAM sandbox, bootstrap the test identities, and create or import
`tos>remind` into it. Import preserves the canonical Application ID and normally
inherits the production webhook signing secret, while returning a fresh test-only
Application secret. Keep that returned secret; it is separate from the root key.

For a test-only application with a different webhook signing secret, the Remind
receiver's retained signing-key configuration must match the secret/version IAM
will use. The current receiver uses its configured shared IAM keyring; importing
the production Remind app with the inherited signing key is the supported setup.
Never point a test Silicon at another party's production delivery endpoint.

## Create Remind's empty environment

Any production Carbon or Silicon member can create an environment for their
current organization. That org owns it, and the principal is recorded as creator.
Creation requires a name and IAM test root key, with optional description and
test Application secret. If the app secret is omitted, the empty environment
is created immediately; ordinary authenticated actions wait for root configuration. No reminder, endpoint, session or audit data is copied
from production.

API:

```http
POST /api/v1/test-environments
Authorization: Bearer <production-Remind-application-access-token>
X-Org-ID: tos
Content-Type: application/json

{
  "name": "release-qa",
  "description": "Manual integration checks",
  "iam_test_key": "<32-character IAM root key>",
  "iam_app_secret": "<test-only tos>remind Application secret>"
}
```

The `201` response contains `environment` metadata and `key`. It uses
`Cache-Control: no-store`. Active names are unique within an org. A failed IAM
binding or schema initialization does not publish a usable partial environment.

CLI:

```sh
remind env create release-qa \
  --iam-key-file /secure/iam-test-key \
  --iam-app-secret-file /secure/iam-test-app-secret
```

The CLI saves the root key with owner-only permissions. To share it explicitly,
use `remind env key <id>`; a teammate imports it with
`remind env import <id> --key-stdin < /secure/remind-test-key`.

## Configure the IAM Application after creation

The IAM environment key alone does not authenticate an Application. If its secret
was omitted, import/create `tos>remind` in that IAM world, then run:

```sh
remind --test <id> configure-iam --iam-app-secret-file /secure/test-app-secret
```

The Rust method is `configure_environment_iam(&Secret)`. HTTP is
`PUT /api/v1/testing-environment/iam`, root-key header, JSON
`{"iam_app_secret":"<test-only-secret>"}`; it returns 204 after verifying the
credential in the linked IAM world. This also replaces a rotated app secret.
An unconfigured environment supports metadata and root cleanup/configuration;
ordinary actions return 409 `test_iam_application_not_configured`. There is no
production credential fallback. Root key rotation and cleanup preserve the IAM
configuration.

## Use the same commands and client methods

```sh
remind --test <id> test-info
remind --test <id> auth login --org test-org --slt-stdin < /secure/test-slt
remind --test <id> webhook set <test-Hook-endpoint> --secret-stdin < /secure/hook-secret
remind --test <id> create --text 'Manual trigger' --cron '* * * * *'
remind --test <id> list
remind --test <id> executions <reminder-id>
```

Use an SLT minted by the linked IAM environment specifically for `tos>remind`
and the intended test organization. CLI production/test sessions occupy separate
slots. In Rust, call `with_test_environment(Secret)` first and then
`with_session(test_access_token, test_org)`. That selection clears any previously
attached session to avoid carrying production credentials into a sandbox.

For raw HTTP, add `X-Remind-Test-Key` to the usual path. Never put the key in a
query string. Missing/invalid/duplicate/revoked environment headers fail closed.
The test-only `test-info`/`clean` equivalents return `test_environment_required`
without a key. Production environment-management paths reject the test header.

## Permissions and lifecycle

| Action | Authority |
| --- | --- |
| Create | Any active production org Carbon/Silicon member |
| List/get metadata | Active members of the owning production org |
| Retrieve key | Creator or current org owner/admin |
| Rotate key | Creator or current org owner/admin |
| Clean data | Anyone with the active Remind environment key |
| Delete/restore | Creator or current org owner/admin |
| Ordinary reminders | Authenticated IAM test actor's normal permissions |

Rotation invalidates the old key when its transaction commits. Clean/delete/key
rotation acquire an exclusive lifecycle lock. API requests and worker cycles
hold shared locks while using an environment, so an acknowledged cleanup or
retirement cannot be followed by an already-admitted delivery still running in
that environment.

Cleaning retains metadata, key and IAM binding, but erases all Remind identities,
webhook configuration, reminders, executions, idempotency records, event receipts,
audit records and deletion logs. It does not clean IAM. Configure the test
Silicon's webhook again afterward.

Deletion immediately disables the key and stops work. Data is retained for 30
days, during which `env restore` restores it with a fresh Remind key. The old key
never becomes valid again. After the deadline the worker drops the isolated
schema and deletes the control metadata permanently. An active environment with
no successful user activity for 15 days is retired automatically. Background
worker polling and IAM webhooks do not count as user activity. Deadline checks
also run when requests/workers are admitted, so a delayed sweep cannot prolong
an inactive environment's access.

## Reminder limit and retention

Each sandbox permits at most 100 retained reminders, counting current and
archived reminders. A database trigger enforces this across concurrent Silicon
creation requests. Limit failures return `409 test_reminder_limit` and explicitly
identify it as a test-only limit. Production has no such 100-reminder quota.
Clean the sandbox to start a new empty run.

Reminder archive behavior remains unchanged: 45 days, then permanent deletion
with a text snapshot of reminder, trigger and creator. The sandbox deletion
ledger retains its own latest 100,000 records, and cleaning erases it. A sandbox's
30-day retirement deadline can erase the entire sandbox before an individual
reminder's 45-day retention expires.

## IAM webhook routing

IAM test webhooks contain `test.testing_key`, `test.metadata`, and `test.data`.
The receiver verifies the signature over the exact raw outer body through the
official SDK before reading the routing key. It finds active Remind replicas
bound to that IAM root key, verifies the expected key with the SDK, normalizes
the event, and writes it only inside those replicas. Root keys are excluded from
stored event payloads and logs. Production events use production storage.
Receipt IDs deduplicate retries, and lifecycle revocations share a transaction
with their receipt. Multiple replicas bound to the same IAM sandbox each receive
the projection independently.

## Deployment storage

Configure a dedicated PostgreSQL database through `REMIND_TEST_DATABASE_URL` and
its migration credential through `REMIND_TEST_MIGRATOR_DATABASE_URL`. Database
names must differ from production. The shared database has one control table and
one generated schema per environment, containing the same data migrations as
production. The runtime never accepts a caller-supplied schema name.

Run `remind-migrate` before starting the new version. It applies production and
test control migrations and brings existing sandbox schemas up to the same data
migration version. New environment DDL and metadata publish atomically. The test
database runtime role needs schema creation/removal privileges for disposable
environments; isolate that authority from production's database role. Credential
bundles are encrypted using Remind's versioned data-encryption keyring.

## Manual acceptance expectations

The acceptance run must use real API calls and CLI/client operations through
these sandboxes. Cover at least: empty startup; both actor types; another org;
wrong/no/rotated keys; login/refresh/logout; all reminder CRUD and batch commands;
cron/timezones including DST boundaries; one-time and recurring delivery; endpoint
failures/retries; immutable occurrence text; pagination; 100th/101st creation;
archive/retention; cleaning; delete/restore; inactivity expiry; duplicate/tampered
IAM events; API/worker restart with pending work. Record observed results and
fixes, not just planned cases, in [the manual run log](MANUAL_ACCEPTANCE.md).
