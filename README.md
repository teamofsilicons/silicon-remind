# Silicon Remind

Silicon Remind is the durable scheduling backend for one-time and recurring
Silicon reminders. It authenticates callers through Silicon IAM, stores schedule
and execution state in PostgreSQL, and submits signed events with stable execution IDs to
configured webhook receiver when occurrences become due.

Both reminder kinds use five-field Linux cron syntax. Clients select
`one_time` or `recurring` explicitly and may omit the IANA timezone, in which
case Remind canonicalizes it to UTC before calculating and storing the next
occurrence.

An owner Silicon can pause or resume one reminder through its individual PATCH
operation, or atomically apply the same status to a batch of up to 100 owned
reminders. Paused reminders remain in the current section and retain their
history; resuming recalculates their next future cron occurrence.

The default schedule view contains current reminders. Owner-archived reminders
and one-time reminders whose cron trigger has materialized remain readable in
the archived section, with execution history, for exactly 45 days before the
retention worker permanently removes them. Read and delivery queries enforce
the deadline independently, so a delayed sweep cannot extend access or send an
expired reminder.

The service is a Rust modular monolith with three independently runnable
processes:

- `remind-api` serves the public and internal HTTP APIs.
- `remind-worker` materializes due occurrences, delivers webhook events, retries
  transient failures, and applies retention.
- `remind-migrate` applies embedded forward-only PostgreSQL migrations once.

Product intent lives in [UNDERSTANDING.md](UNDERSTANDING.md), engineering choices
in [decisions.md](decisions.md), and the public HTTP contract in
[openapi.yaml](openapi.yaml) with prose guidance in [API_DOCS.md](API_DOCS.md).
The non-public provisioning and IAM receiver contract is in
[INTERNAL_API.md](INTERNAL_API.md).

## Prerequisites

- Rust 1.98.0; `rust-toolchain.toml` installs `rustfmt` and Clippy.
- PostgreSQL 16 or newer.
- Docker with Compose v2 for the container workflow.
- Python 3 plus `openapi-spec-validator==0.7.2` only for local OpenAPI validation.

## Local setup

Create the local environment file:

```sh
cp .env.example .env
```

Replace every `REPLACE_*` value. The IAM application and `whs_` webhook signing
secrets must come from the IAM application registration. Generate independent
internal and encryption secrets; for example:

```sh
openssl rand -hex 32
openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n'
```

Put the second value in `REMIND_ENCRYPTION_KEYRING` under the version selected
by `REMIND_ENCRYPTION_CURRENT_VERSION`. Never reuse these examples or local
database credentials in a deployed environment.

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

A new Compose volume initializes both `remind` and `remind_testing`. For an
existing volume, create the latter database explicitly before running migrations:
`docker compose exec postgres createdb -U remind remind_testing`.

The API is available at `http://127.0.0.1:8080`, the worker's operational server
at `http://127.0.0.1:9090`, and PostgreSQL only on `127.0.0.1:5432`. The Compose
file overrides database hostnames for its network. IAM and webhook URLs in `.env`
must also be reachable from the containers. On Docker Desktop, a host-run
dependency can normally be addressed through `host.docker.internal` rather than
`127.0.0.1`.

Migrations never run implicitly when an API or worker replica starts. Run the
migrator once for each release before starting code that requires its schema.

### Run native Rust processes

Start only the local database, then run each process in a separate terminal:

```sh
make db-up
make migrate
make run-api
make run-worker
```

Each binary loads `.env` when present. Both the API listener and worker
operational listener expose `/health/live`, `/health/ready`, and `/metrics` on
their respective ports. Readiness has a two-second deadline and verifies the
embedded migration ledger plus critical Remind schema fields; a reachable but
unmigrated or drifted PostgreSQL database is not reported ready. The worker
metrics endpoint is the authoritative source for scheduler, delivery, retry,
and worker-error counters.

## Development checks

```sh
make fmt
make check
make clippy
make test
```

To validate the OpenAPI contract locally:

```sh
python3 -m pip install openapi-spec-validator==0.7.2
make openapi
```

`make ci` runs formatting, type checking, strict Clippy, tests, and OpenAPI
validation. `make help` lists all supported commands. Tests that exercise real
PostgreSQL concurrency require a running Docker daemon for Testcontainers.

## CLI, client and IAM setup

Build the public client and CLI with `cargo build --workspace`. The executable is
`target/debug/remind`; run `remind -h` for commands. Obtain an organization-bound
IAM SLT for `tos>remind`, then use:

```sh
remind login <slt>
remind webhook subscribe https://example.com/reminders --secret-stdin < /private/webhook-secret
remind create --text 'Check the build' --cron '*/5 * * * *'
```

Only Silicons configure destinations or mutate their own reminders. Both Carbon
and Silicon members can read reminders throughout their organization. All IAM
calls use the official published Rust client and live Application authorization
snapshots; no Remind-specific permission projection is required from IAM.

The registered IAM receiver is `POST /webhook/`. Configure its exact signing
version and secret in the backend keyring. Registering the URL does not deploy
it or complete IAM review. Test requests require a Remind root key and the
linked IAM sandbox's own identity/Application credential. Test reminders only
accept webhook test ingress; production reminders only accept production ingress.

Detailed references live in [docs/](docs/README.md): [API](docs/api/README.md),
[Rust client](docs/client/README.md), [CLI](docs/cli/README.md),
[IAM](docs/iam.md), [testing environments](docs/testing-environments.md), and
[webhook delivery](docs/webhook-delivery.md). The
[manual acceptance record](docs/MANUAL_ACCEPTANCE.md) identifies verified work
and outstanding evidence. Internal provisioning is documented separately in
[docs/internal-api.md](docs/internal-api.md) and is not exposed by the client.

## Configuration

[.env.example](.env.example) is the exhaustive `REMIND_*` configuration
reference and matches [src/config.rs](src/config.rs). Values are grouped by
server, runtime and migrator database pools, IAM, internal authentication,
encryption, webhook, workers, retries, and retention.

Important invariants include:

- IAM application credentials, retained `whs_` webhook signing versions, the
  internal bearer token, and every encryption key are secret values and must
  come from a secret manager in production.
- IAM delivers application events to `POST /webhook/` (the internal alias remains). This route
  does not accept the internal bearer token as authentication: it verifies
  `X-Silicon-IAM-*` headers over the exact raw body, enforces the five-minute
  replay window, and deduplicates the signed event ID durably. webhook-destination
  provisioning routes remain protected by the internal bearer credential.
- Encryption keys are unpadded base64url encodings of exactly 32 random bytes.
  Increment the current version for new writes. Worker sweeps automatically
  rewrap active destination ciphertext with it; retain old keys until no row
  references them. Disabled destination ciphertext is purged after 45 days.
- Production HTTP dependency URLs must use HTTPS. PostgreSQL URLs must include
  `sslmode=verify-full`.
- Each Silicon may have zero or more persisted webhook subscriptions; arbitrary
  absolute HTTP(S) destination hosts and paths are accepted subject to transport
  validation.
- webhook delivery concurrency is bounded independently from scheduler batch size
  and cannot exceed that batch size. A worker claims no more deliveries than it
  can start concurrently, so leased work does not wait behind an in-process
  queue.
- The worker lease must exceed the webhook request timeout, two configured database
  operation budgets, one poll interval, and a five-second safety margin. Retry
  maximum delay must not be shorter than the base delay.
- Schedule history and disabled webhook destinations have a fixed 45-day retention
  policy. Before a retained schedule is permanently removed, the worker writes
  an internal, one-line JSON deleted-reminder record containing its reminder,
  trigger, and creator snapshots. The ledger keeps the newest 100,000 records
  globally. IAM lifecycle tombstones remain authoritative while worker cycles
  drain affected resources in bounded batches.
- Runtime and migrator database URLs should use separate least-privilege roles
  in deployed environments.

Configuration errors are redacted and terminate startup before a process begins
serving work.

## Container deployment

The multi-stage [Dockerfile](Dockerfile) produces one non-root, read-only-capable
image containing all three binaries. Its default entrypoint is `remind-api`.
Override the entrypoint with `/usr/local/bin/remind-worker` or
`/usr/local/bin/remind-migrate` for the other process roles.

The included Compose stack is for local development, not production. A deployed
release should inject secrets through the platform secret manager, run the
migrator as a one-shot release job, expose only the API publicly, make worker
port `9090` reachable only from health probes and the metrics collector, scale
API and worker roles independently, and preserve graceful `SIGTERM` windows.

## Repository layout

```text
src/api/              HTTP routing, middleware, and wire models
src/application/      Use cases and infrastructure-independent ports
src/domain/           Scheduling, execution, actor, and cursor policy
src/infrastructure/   PostgreSQL, IAM, webhook, and encryption adapters
src/worker/           Scheduler, delivery, and retention loops
src/bin/              API, worker, and migration composition roots
migrations/           Ordered PostgreSQL schema migrations
```

The backend crate is proprietary and is not published. The public client and CLI
under `crates/` are separately licensed Apache-2.0 packages.
