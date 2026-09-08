# Silicon Remind build and acceptance status

Implemented and manually exercised locally on 2026-09-06 IST against hosted IAM
sandbox identities and the real local configured webhook receiver backend. Scope follows
[UNDERSTANDING.md](../UNDERSTANDING.md). The frontend and `remind report` remain
explicit later work in that document.

## Delivered

- Official `silicon-iam-client` 1.2.1 integration for `tos>remind`: SLT exchange,
  refresh, logout, live app/org/test authorization and signed lifecycle receiver.
- Reminder create/list/get/edit, one-time and recurring five-field cron, IANA
  timezones with UTC default, stored next UTC occurrence, individual and atomic
  batch pause/resume, owner archive and org-wide read permissions.
- Owner-configured webhook destinations, current signed delivery contract, durable
  execution history, retry after receiver outages and concurrent worker claims.
- One-time automatic archive, 45-day readability, eventual physical purge and
  full deletion snapshots capped to the latest 100000 text records.
- Organization-owned test environments in a dedicated shared test database,
  with isolated schemas and mandatory IAM test binding. Creation needs name and
  IAM root key; test app configuration can follow separately. Root keys are
  encrypted at rest and can be retrieved, rotated, imported and forgotten.
- Sandbox clean, delete, restore with a fresh key, 15-day inactivity retirement,
  30-day recovery and permanent purge. Logical expiry is enforced before sweep.
  All retained reminders count toward the explicit sandbox-only limit of 100.
- Stateless typed Rust client and stateful CLI covering public operations.
  CLI secrets live in permission-restricted `~/.remind/` state; refresh operation
  identities are persisted before rotating credentials to allow crash recovery.
- Default-on hourly command-triggered update maintenance with opt-out. CLI
  installation uses its existing Cargo root; source builds report availability.
  Library updates affect the consuming lockfile and require a rebuild.
- Segregated [API](api/README.md), [client](client/README.md),
  [CLI](cli/README.md), [IAM](iam.md), [sandbox](testing-environments.md),
  [webhook](webhook-delivery.md) and [internal API](internal-api.md) guides, plus OpenAPI.

## Evidence

[MANUAL_ACCEPTANCE.md](MANUAL_ACCEPTANCE.md) records actual CLI/API actions,
SQL-prepared large/aged fixtures and inspected worker/webhook outcomes. Coverage
includes real Carbon and Silicon sessions, permissions across identities and
organizations, cross-sandbox reads, all CLI command groups, rotation/recovery,
quota and byte limits, idempotency conflicts, atomic batch rollback, retention
boundaries, DST gaps/repeats, signed event replay/tampering, real IAM removal,
logout, interrupted refresh recovery and two-worker outage/retry delivery.

Development regression checks passed independently of that manual stage:
125 existing workspace tests, formatting, strict all-target Clippy and OpenAPI
validation. The final Docker image built and ran as UID/GID 10001 with a
read-only filesystem and dropped capabilities; readiness returned 200 and a
real IAM test Silicon request returned 200. The temporary verification container
was stopped and removed after inspection.

Production secrets are in git-ignored mode-0600 `.env`; protected manual fixtures
are ignored under `target/manual-secrets`. The source scan found none of those
configured secret values in tracked or newly added source files.

## Public deployment: 2026-09-06

The dedicated AWS production stack is deployed. Public TLS/readiness, healthy
API and worker containers, production Carbon login, test Silicon login and
isolated sandbox creation passed. See [release record](RELEASE_0.1.0.md).

## Public release verified

The public backend is deployed with healthy API and worker tasks. IAM reports
its webhook active; a signed event is recorded as processed in the production
receiver, and the app dead-letter list is empty. Both client and CLI are published
at 0.1.0. Registry archive checksums and Rust source match this checkout, and a
fresh crates.io installation passed version, public readiness, authentication
and current-version update checks. See [release record](RELEASE_0.1.0.md) for
exact evidence and the remaining limitation on newer-version updater testing.

webhook delivery is at least once. An ingress receipt is not proof of signature
verification or application processing; the manual test separately inspected
webhook verified history. Consumers must deduplicate the stable execution ID.
