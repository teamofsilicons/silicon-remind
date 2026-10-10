# Silicon Remind

Start with the [hosted usage and developer docs](https://docs.remind.teamofsilicons.com).

Silicon Remind keeps a Silicon's reminders. A Silicon sets one-time or recurring reminders in five-field Linux cron
syntax with an IANA timezone; when one comes due, Remind posts a signed event with a stable execution id to every
webhook subscription that Silicon set up, and keeps the execution history. Subscriptions are optional.

Everyone signs in with [Silicon Accounts](https://accounts.teamofsilicons.com); Remind is the app `remind` there.
A reminder belongs to the Silicon that set it, and only that Silicon changes it. Its custodian (the Carbon who looks
after it) and the custodian's other Silicons can read it, and so can any account the Silicon or its custodian shares
the reminders with. Silicons install the CLI from [Silicon Apps](https://apps.teamofsilicons.com):

```sh
silicon-apps install remind
silicon-accounts login --app remind -q | remind login --slt-stdin
remind create --text 'Check the build' --cron '*/30 * * * *' --timezone Asia/Kolkata
remind list
```

Carbons use the [website](https://remind.teamofsilicons.com), or `remind login`, which shows a code to approve on
the account site.

Both reminder kinds use five-field Linux cron syntax. Clients choose `one_time` or `recurring` and must give an IANA
timezone such as `Asia/Kolkata` or `UTC`; Remind refuses a missing or blank timezone and never picks one. The next
occurrence is calculated in that timezone and stored in UTC.

The owner Silicon can pause or resume one reminder, or apply the same status to up to 100 of its reminders at
once. Paused reminders stay in the current section with their history; resuming recalculates the next occurrence.
Archived reminders, and one-time reminders that have fired, stay readable in the archived section with their history
for exactly 45 days; then the retention worker removes them and records each in the deleted-reminders ledger (the
newest 100,000). Reads and deliveries enforce the deadline themselves, so a late sweep never extends access or sends
an expired reminder.

The service is a Rust modular monolith with three processes:

- `remind-api` serves the HTTP API (contract 2, under `/api/v2`) and receives Silicon Accounts' app webhook.
- `remind-worker` materialises due occurrences, delivers webhook events, retries transient failures and applies
  retention.
- `remind-migrate` applies the embedded, forward-only PostgreSQL migrations once, and re-keys data from before the
  move to Silicon Accounts (`remind-migrate link-identities`).

Product intent lives in [UNDERSTANDING.md](UNDERSTANDING.md), engineering choices in [decisions.md](decisions.md)
and [docs/migration/decisions.md](docs/migration/decisions.md), and the HTTP contract in [openapi.yaml](openapi.yaml)
with the [API guide](docs/api/README.md).

## Prerequisites

- Rust 1.98.0; `rust-toolchain.toml` installs `rustfmt` and Clippy.
- PostgreSQL 16 or newer.
- Docker with Compose v2 for the container workflow (optional).
- Python 3 plus `openapi-spec-validator==0.7.2` only for local OpenAPI validation.

## Local setup

Create the local environment file:

```sh
cp .env.example .env
```

Replace every `REPLACE_*` value. `REMIND_APP_SECRET` is Remind's app secret at Silicon Accounts, and
`REMIND_ACCOUNTS_WEBHOOK_SECRET` the `whsec_…` secret shown once when its app webhook is set; for a local Silicon
Accounts stack, set `ACCOUNTS_URL` (and `ACCOUNTS_API_URL` when the API listens elsewhere) to it. Generate the
encryption key yourself, for example:

```sh
openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n'
```

Put it in `REMIND_ENCRYPTION_KEYRING` under the version selected by `REMIND_ENCRYPTION_CURRENT_VERSION`. Never reuse
these values or the local database credentials in a deployed environment.

### Run with Docker Compose

Start PostgreSQL and apply migrations explicitly:

```sh
docker compose up -d postgres
docker compose --profile tools run --build --rm migrate
```

Then start the API and worker profiles:

```sh
docker compose --profile api --profile worker up --build
```

A new Compose volume initialises both `remind` and `remind_testing`. For an existing volume, create the latter first:
`docker compose exec postgres createdb -U remind remind_testing`.

The API listens on `http://127.0.0.1:8080`, the worker's operational server on `http://127.0.0.1:9090`, and
PostgreSQL only on `127.0.0.1:5432`. The Compose file overrides database hostnames for its network; Silicon Accounts
and webhook URLs in `.env` must also be reachable from the containers (on Docker Desktop, use
`host.docker.internal` for a service on your machine).

Migrations never run when an API or worker starts. Run the migrator once per release before starting code that needs
its schema.

### Run native Rust processes

Start only the local database, then run each process in its own terminal:

```sh
make db-up
make migrate
make run-api
make run-worker
```

Each binary loads `.env` from its working directory and every parent. The API and the worker's operational listener
both expose `/health/live`, `/health/ready` and `/metrics`. Readiness has a two-second deadline and checks the
embedded migration ledger and critical schema fields, so a reachable but unmigrated or drifted database is not
ready. The worker's metrics are the source for scheduler, delivery, retry and worker-error counters.

## Development checks

```sh
make fmt
make check
make clippy
make test
```

`make ci` runs formatting, type checking, strict Clippy, tests and OpenAPI validation (`make openapi` needs
`openapi-spec-validator==0.7.2`). The database tests need PostgreSQL: point them at a server you can create databases
on, and each test gets its own database, dropped afterwards:

```sh
REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5432/postgres make test
```

Without the variable they start a disposable PostgreSQL container through Testcontainers, which needs Docker.

## CLI and Rust client

The public client (`silicon-remind-client`) and the CLI (`silicon-remind-cli`, command `remind`) are in `crates/`.
`cargo build --workspace` builds `target/debug/remind`; `remind --help` lists every command, and `remind docs <topic>`
prints the bundled manuals. Signing in, sharing and who sees what are described in [docs/accounts.md](docs/accounts.md).

Releases ship through Silicon Apps, one archive per target: [docs/releases.md](docs/releases.md). Neither the CLI
nor the client updates itself; Silicon Apps keeps installed copies current.

Detailed references live in [docs/](docs/README.md): [API](docs/api/README.md), [Rust client](docs/client/README.md),
[CLI](docs/cli/README.md), [signing in and sharing](docs/accounts.md), [testing environments](docs/testing-environments.md),
[webhook delivery](docs/webhook-delivery.md) and the [service-only routes](docs/internal-api.md). Records from before
the move to Silicon Accounts are in [docs/history/](docs/history/README.md).

## Configuration

[.env.example](.env.example) is the complete configuration reference and matches [src/config.rs](src/config.rs).
Values are grouped by server, runtime and migrator database pools, Silicon Accounts, encryption, webhook delivery,
workers, retries and retention.

Important invariants:

- Remind's app secret, its webhook secrets and every encryption key are secrets; in production they come from a
  secret manager. The app secret never reaches the CLI or a browser.
- `ACCOUNTS_URL` is the Silicon Accounts public origin and must equal the issuer of access tokens. Remind verifies
  access tokens against Silicon Accounts' published keys and asks Silicon Accounts directly before anything that
  cannot be undone or reveals a secret.
- Silicon Accounts delivers account changes to `POST /webhook/`. Remind checks the `X-Accounts-Signature` over the
  exact raw body with each configured secret, refuses deliveries more than five minutes old, and applies each
  `event_id` once.
- Encryption keys are unpadded base64url encodings of exactly 32 random bytes. Increment the current version for new
  writes; worker sweeps rewrap active subscription secrets with it. Keep old keys until no row uses them. Disabled
  subscription ciphertext is purged after 45 days.
- Production HTTP dependency URLs must use HTTPS, and PostgreSQL URLs must include `sslmode=verify-full`.
- A Silicon may have any number of webhook subscriptions; any absolute HTTP(S) destination is accepted subject to
  transport validation.
- Delivery concurrency is bounded separately from the scheduler batch size and cannot exceed it. A worker claims no
  more deliveries than it can start at once, so leased work never waits behind an in-process queue.
- The worker lease must exceed the webhook request timeout, two database operation budgets, one poll interval and a
  five-second margin. The retry maximum delay must not be shorter than the base delay.
- Before a retained reminder is removed, the worker writes a one-line JSON record (reminder, trigger and owner) to
  the deleted-reminders ledger, which keeps the newest 100,000 records.
- Runtime and migrator database URLs should use separate least-privilege roles in deployed environments.

Configuration errors are redacted and stop startup before a process serves any work. Settings of the previous
identity service still present in an environment are ignored, with one warning each.

## Container deployment

The multi-stage [Dockerfile](Dockerfile) builds one non-root image with all three binaries; its default entrypoint is
`remind-api`. Use `/usr/local/bin/remind-worker` or `/usr/local/bin/remind-migrate` as the entrypoint for the other
roles.

The Compose stack is for local development. A deployment injects secrets from the platform's secret manager, runs the
migrator as a one-shot release step, exposes only the API publicly, keeps worker port `9090` reachable only from
health probes and the metrics collector, and preserves graceful `SIGTERM` windows. Production runs on one EC2 host:
[deploy/aws/README-standalone.md](deploy/aws/README-standalone.md) and [docs/deployment.md](docs/deployment.md).

## Repository layout

```text
src/api/              HTTP routing, middleware, and wire models
src/application/      Use cases and infrastructure-independent ports
src/domain/           Scheduling, execution, actor, and cursor policy
src/infrastructure/   PostgreSQL, Silicon Accounts, webhook, and encryption adapters
src/worker/           Scheduler, delivery, and retention loops
src/bin/              API, worker, and migration composition roots
migrations/           Ordered PostgreSQL schema migrations
crates/               The public Rust client and the remind CLI
packaging/, scripts/  Silicon Apps packaging and release scripts
deploy/               Production templates and installers
```

The backend crate is proprietary and is not published. The public client and CLI under `crates/` are separately
licensed Apache-2.0 packages.
