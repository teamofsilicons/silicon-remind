# Silicon Remind

Silicon Remind is the durable scheduling backend for one-time and recurring
Silicon reminders. It authenticates callers through Silicon IAM, stores schedule
and execution state in PostgreSQL, and submits signed, idempotent events to
Silicon Hook when occurrences become due.

Both reminder kinds use five-field Linux cron syntax. Clients select
`one_time` or `recurring` explicitly and may omit the IANA timezone, in which
case Remind canonicalizes it to UTC before calculating and storing the next
occurrence.

The default schedule view contains current reminders. Owner-archived reminders
and one-time reminders whose cron trigger has materialized remain readable in
the archived section, with execution history, for exactly 45 days before the
retention worker permanently removes them. Read and delivery queries enforce
the deadline independently, so a delayed sweep cannot extend access or send an
expired reminder.

The service is a Rust modular monolith with three independently runnable
processes:

- `remind-api` serves the public and internal HTTP APIs.
- `remind-worker` materializes due occurrences, delivers Hook events, retries
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

The API is available at `http://127.0.0.1:8080`, the worker's operational server
at `http://127.0.0.1:9090`, and PostgreSQL only on `127.0.0.1:5432`. The Compose
file overrides database hostnames for its network. IAM and Hook URLs in `.env`
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

## Internal integration bootstrap

Before a Silicon can create a reminder, a trusted control-plane caller must
register the Hook destination through `PUT /internal/v1/hook-destinations` with
the IAM principal UUID, immutable public global Silicon ID, exact Hook endpoint,
and one-time `whsec_` credential. This bearer-protected operation establishes
the authorization-to-routing identity binding and encrypts the destination
material. Until it succeeds, schedule creation fails closed with
`409 webhook_not_configured` and `Set the webhook url first.`

Destination rotation uses the same `PUT`; explicit disable uses
`DELETE /internal/v1/hook-destinations/{org_id}/{silicon_id}`. IAM revocation
events create irreversible principal or organization tombstones, so a later
provisioning call cannot silently reactivate removed authority. The complete
wire formats, HMAC rules, responses, and lifecycle behavior are documented in
[INTERNAL_API.md](INTERNAL_API.md).

The IAM producer must emit its published `X-Silicon-IAM-*` application-webhook
contract before deployment. Remind intentionally rejects the older
`X-Silicon-*`/`timestamp.event_id.body` variant because it lacks the key version
and lifecycle identity projection needed for fail-safe revocation; see D-030 in
[decisions.md](decisions.md). IAM must likewise serve the published authenticated
introspection response containing `principal_id`, `actor_type`, public `org_id`,
`membership_id`, `authorization_epoch`, and `expires_at`. Carbon responses must
also contain the explicit `remind_permitted_silicon_principal_ids` array. Remind
applies that owner UUID projection inside PostgreSQL before pagination; Silicons
receive organization-wide reads, Carbons receive read-only projected access,
and only an owner Silicon can mutate its reminder.

The checked-in IAM runtime must add a token-exchange/issuance path for
Remind-audience application tokens and populate this authoritative projection
before public Remind traffic is enabled. Its current native token audience and
Carbon-only OAuth path cannot satisfy Remind's authenticated introspection
contract. Remind fails closed rather than deriving Carbon access from an
organization directory response.

## Configuration

[.env.example](.env.example) is the exhaustive `REMIND_*` configuration
reference and matches [src/config.rs](src/config.rs). Values are grouped by
server, runtime and migrator database pools, IAM, internal authentication,
encryption, Hook, workers, retries, and retention.

Important invariants include:

- IAM application credentials, retained `whs_` webhook signing versions, the
  internal bearer token, and every encryption key are secret values and must
  come from a secret manager in production.
- IAM delivers application events to `POST /internal/v1/iam/events`. This route
  does not accept the internal bearer token as authentication: it verifies
  `X-Silicon-IAM-*` headers over the exact raw body, enforces the five-minute
  replay window, and deduplicates the signed event ID durably. Hook-destination
  provisioning routes remain protected by the internal bearer credential.
- Encryption keys are unpadded base64url encodings of exactly 32 random bytes.
  Increment the current version for new writes. Worker sweeps automatically
  rewrap active destination ciphertext with it; retain old keys until no row
  references them. Disabled destination ciphertext is purged after 45 days.
- Production HTTP dependency URLs must use HTTPS. PostgreSQL URLs must include
  `sslmode=verify-full`.
- The Hook base URL restricts persisted delivery destinations; arbitrary
  destination hosts are not accepted.
- Hook delivery concurrency is bounded independently from scheduler batch size
  and cannot exceed that batch size. A worker claims no more deliveries than it
  can start concurrently, so leased work does not wait behind an in-process
  queue.
- The worker lease must exceed the Hook request timeout, two configured database
  operation budgets, one poll interval, and a five-second safety margin. Retry
  maximum delay must not be shorter than the base delay.
- Schedule history and disabled Hook destinations have a fixed 45-day retention
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
src/infrastructure/   PostgreSQL, IAM, Hook, and encryption adapters
src/worker/           Scheduler, delivery, and retention loops
src/bin/              API, worker, and migration composition roots
migrations/           Ordered PostgreSQL schema migrations
```

This repository is proprietary and is not published as a crate.
