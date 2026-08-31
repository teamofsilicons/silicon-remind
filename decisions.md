# Silicon Remind engineering decisions

This file is the append-only decision log for the Silicon Remind backend.
Material architecture, security, data-model, API, scheduling, and operational
decisions are recorded here before or alongside their implementation. A changed
decision is marked as superseded; its original record is not silently rewritten.

## D-001 — PostgreSQL is authoritative

**Status:** Accepted

PostgreSQL is the system of record for schedules, executions, idempotency
records, Hook destinations, internal-event receipts, and audit records. Database
constraints and transactions enforce invariants whenever PostgreSQL can express
them. No in-memory queue or scheduler state is required for correctness.

## D-002 — Modular monolith with separate API and worker processes

**Status:** Accepted

The backend is one Rust package containing a library and three thin binaries:
`remind-api`, `remind-worker`, and `remind-migrate`. Domain, application,
infrastructure, HTTP, and worker concerns remain separate modules. API and worker
processes can scale independently without introducing a distributed transaction
between internal services.

## D-003 — Rust safety and quality baseline

**Status:** Accepted

The service uses the Rust 2024 edition and the stable 1.98 toolchain to match the
adjacent IAM service. Unsafe code is forbidden. Rustfmt, strict Clippy lints,
dependency policy checks, deterministic lockfiles, and automated tests are part
of the repository. Recoverable production paths do not use `unwrap`, `expect`,
`todo`, `unimplemented`, or deliberate panics.

## D-004 — IAM tokens are introspected and authorization fails closed

**Status:** Accepted

Public requests use the bearer token and `X-Org-ID` required by the OpenAPI
contract. Remind sends the opaque token to IAM's authenticated introspection
endpoint and requires an active Carbon or Silicon actor with current membership
in the requested organization. Missing, malformed, inactive, ambiguous, or
unrecognized IAM responses are rejected. Authorization results are not cached in
the initial release so logout, revocation, and membership removal take effect on
the next request.

IAM's published OpenAPI currently references but does not define the
`TokenIntrospection` schema. The HTTP adapter therefore accepts a deliberately
small compatibility set for organization membership (`org_id`, `org_ids`, or a
membership/organization array) while exposing one strict internal actor model.
This compatibility logic is isolated in the IAM adapter and covered by tests.

## D-005 — Organization visibility and Silicon ownership follow product intent

**Status:** Accepted

An authenticated Carbon or Silicon may read schedules and execution history for
Silicons in the actor's selected organization, matching `UNDERSTANDING.md`.
Only an authenticated Silicon may create a schedule; the authenticated Silicon
always becomes its owner. Only that owner may update or delete it. Every lookup
includes `org_id`, and unauthorized cross-organization identifiers are returned
as not found so UUID knowledge does not disclose resource existence.

Application/OBO access is deferred because the Remind contract does not define
its audience, actions, or proof-consumption flow.

## D-006 — One-time schedules use `run_at`; recurring schedules use five-field cron

**Status:** Accepted

The OpenAPI contract's absolute RFC 3339 `run_at` representation is normative
for one-time schedules. Recurring schedules use exactly five Linux cron fields.
Exactly one representation must exist after create or patch. A one-time
`run_at` must be in the future when created or changed. Reminder text is trimmed
only for emptiness checks; its original content is retained, with a maximum of
100,000 bytes.

Clients cannot set `completed`; that is a worker-owned terminal state for
one-time schedules. Clients may pause and resume. Recurring schedules do not
transition to `completed` through the public API.

## D-007 — UTC persistence with explicit IANA time-zone evaluation

**Status:** Accepted

All instants are stored and returned in UTC. Every schedule retains a validated
IANA time-zone identifier. Cron fields are evaluated as wall-clock values in
that zone. During a daylight-saving gap a nonexistent local occurrence is
skipped. During a repeated local interval both distinct instants are eligible;
the normal next-occurrence ordering fires each once. One-time `run_at` remains
an absolute instant; its timezone is descriptive context for delivery and UI.

The next recurring occurrence is always calculated strictly after a supplied
instant, which makes scheduling calculations deterministic and testable.

## D-008 — Durable occurrence materialization separates timing from delivery

**Status:** Accepted

The worker first materializes due occurrences into an `executions` table using a
unique `(schedule_id, scheduled_for)` constraint. It then delivers executions in
a separate lease-based stage. Claims use `FOR UPDATE SKIP LOCKED`, bounded
leases, and stable execution UUIDs, allowing multiple workers without duplicate
logical occurrences.

For recurring schedules, materialization advances `next_run_at` transactionally.
For one-time schedules it clears `next_run_at`; the schedule becomes `completed`
after Hook accepts the execution or after delivery reaches terminal failure.
Execution history therefore records delivery outcome even though the one-time
schedule cannot fire again.

## D-009 — Missed recurring occurrences are coalesced

**Status:** Accepted

If the service was unavailable across several recurring occurrences, the worker
creates one execution for the stored due instant, then advances to the first
future occurrence relative to worker time. It does not emit an unbounded burst
of historical reminders. One-time reminders are delivered once when service
resumes and preserve their original `scheduled_for` instant.

This policy is an implementation choice because the public contract leaves
misfires undefined.

## D-010 — Hook delivery is HTTP, signed, idempotent, and replaceable

**Status:** Accepted

Remind follows Silicon Hook's HTTP ingress contract rather than sending a
WebSocket frame directly. Hook is responsible for downstream WebSocket delivery.
Each request uses the execution UUID as `Idempotency-Key`; retries reuse that
UUID. The event type is `remind.schedule.triggered`, and an execution is marked
delivered only after Hook returns `202 Accepted` with a valid event UUID.

Because the current contracts do not define Hook destination discovery or the
signature canonicalization, these details live behind application ports. The
initial HTTP adapter uses HMAC-SHA256 over `<unix_timestamp>.<raw_body>` and a
`v1=<lowercase hex>` signature. Per-Silicon endpoint URLs and signing secrets are
registered through a service-authenticated internal endpoint and encrypted at
rest. Production startup requires the internal service credential and a
versioned 256-bit encryption key. This internal integration contract can be
changed without changing the public schedules API when Hook publishes a
normative provisioning flow.

Outbound redirects are disabled, destination URLs must be HTTPS in production,
response bodies are bounded, and credentials are never logged.

## D-011 — Delivery uses capped exponential retry and terminal history

**Status:** Accepted

Network failures, timeouts, `408`, `425`, `429`, and `5xx` responses are retried
with capped exponential delay and deterministic per-execution jitter. Other
`4xx` responses are terminal. The default maximum is eight attempts, with a
30-second base and one-hour cap; all values are configurable. Failure details
are bounded and sanitized before storage. Terminal failures remain visible in
execution history and are not silently discarded.

## D-012 — Archive and deletion retention is 45 days

**Status:** Accepted

Completed one-time schedules and explicitly deleted schedules are soft-archived
with their execution history for 45 days, as required by `UNDERSTANDING.md`.
Deleted schedules disappear from public reads immediately. Completed schedules
remain readable through the documented `completed` status during retention.
After the retention deadline a worker permanently deletes the schedule and its
cascading execution history in bounded batches. Recurring schedules remain
active or paused until deleted.

## D-013 — Idempotency is actor-, organization-, and operation-scoped

**Status:** Accepted

Create and patch reserve `Idempotency-Key` in the same database transaction as
their mutation. The scope contains organization, authenticated actor, operation,
and target where applicable. A replay with the same canonical request hash
returns the original status and body. Reusing a key with different input returns
`409 idempotency_conflict`. Concurrent identical requests serialize on the
unique key and cannot duplicate a mutation. Records are retained for 24 hours
by default and then purged in bounded worker batches.

DELETE remains naturally idempotent for an owner: deleting an already deleted
or absent identifier returns `204` only when ownership can be established in the
same organization; unknown resources remain `404` to avoid authorization leaks.

## D-014 — Pagination is opaque keyset pagination

**Status:** Accepted

Schedules are ordered by `(created_at DESC, id DESC)` and executions by
`(scheduled_for DESC, id DESC)`. Cursors encode the last tuple and the query kind
in URL-safe base64 JSON. They are treated as opaque and validated strictly.
Filters are not embedded because applying a cursor to different filters remains
safe and deterministic under the same ordering. Invalid cursors return a
validation error. Limits follow the documented default of 20 and maximum of 100.

## D-015 — Schedule updates are optimistic and occurrence-safe

**Status:** Accepted

Each schedule has an internal monotonic version incremented on mutation.
Public PATCH locks the row before merging and validating fields. Occurrences
already materialized keep an immutable snapshot of the text, timezone, owner,
and scheduled instant used for delivery; later edits affect only future
occurrences. Pausing prevents new materialization but does not cancel a durable
execution. Deleting prevents pending/retrying executions that have not been
accepted by Hook from being claimed again.

## D-016 — Public errors and request boundaries are stable

**Status:** Accepted

All failures use the documented `{ "error": { "code", "message",
"request_id" } }` envelope. Validation is `422`, absent/invalid credentials are
`401`, insufficient authority is `403`, invisible resources are `404`, state or
idempotency conflicts are `409`, provider unavailability is `503`, and
unexpected failures are `500` without leaking internal detail. The API enforces
request IDs, body limits, timeouts, sensitive-header redaction, structured
tracing, and graceful shutdown.

## D-017 — Configuration is explicit and fail-fast

**Status:** Accepted

Configuration is loaded from namespaced environment variables into typed,
validated structures. Development defaults are limited to non-secret behavior.
Production refuses plaintext Hook destination URLs, missing IAM application
credentials, missing internal credentials, or missing encryption material.
API and worker use least-privilege database credentials when deployments provide
them; migrations run through a separate binary and connection.

## D-018 — Health, readiness, metrics, and audit are operational contracts

**Status:** Accepted

Unprotected liveness and readiness endpoints are outside the versioned product
API. Readiness verifies PostgreSQL with a short deadline. Prometheus-format
metrics cover HTTP outcomes, materialized executions, Hook delivery outcomes,
retry counts, and worker loop failures without actor IDs or reminder text.
Mutations, lifecycle transitions, and internal destination changes create
append-only redacted audit rows in the same transaction as their state change.

## D-019 — Public contract and implementation evolve together

**Status:** Accepted

`openapi.yaml` remains the public schedules contract and
`API_DOCS.md` its explanation. The implementation covers its six operations and
does not add legacy Glass `/crons` aliases or Interface CLI response shapes.
Internal operational endpoints are mounted outside `/api/v1` and documented
separately so they cannot accidentally become public product surface.
Implementation interpretations and gap resolutions belong in this decision log,
not in `UNDERSTANDING.md`.

## D-020 — IAM lifecycle events stop removed Silicons fail-safe

**Status:** Accepted

IAM logout and token revocation need no local session invalidation because public
authorization is introspected on every request. Organization or Silicon removal
must also stop unattended worker delivery. Until IAM publishes a normative
application-webhook schema, a service-authenticated internal lifecycle endpoint
accepts idempotent, versioned `iam.silicon.removed` and
`iam.organization.removed` events. Processing such an event soft-deletes the
affected schedules, clears future due instants, and terminally fails unaccepted
pending executions in one transaction. Event IDs and request hashes are retained
so duplicate delivery is safe and conflicting reuse is rejected.

Unknown event types are durably recorded but rejected without changing schedule
state. This narrow temporary contract is documented separately and can be
replaced by IAM's signed application webhooks once their envelope, signature,
and event schemas are normative.

## D-021 — IAM principal UUIDs and public Silicon IDs have separate roles

**Status:** Accepted; supersedes the introspection-shape rationale in D-004

IAM's published top-level introspection result (`active`, `principal_id`,
`actor_type`, `org_id`, and `expires_at`) is the normative authorization input,
and Remind sends `X-Org-ID` so IAM binds its decision to the requested tenant.
The adapter retains a small, tested compatibility reader for the formerly
published nested actor/membership shapes, but all variants collapse to the
stable IAM principal UUID. No public handle is accepted as proof of identity.

Public schedules and Hook routing still require IAM's immutable global Silicon
ID. A trusted Hook-destination provisioning call therefore supplies the
organization, principal UUID, and exact `{local_silicon_id}:{org_id}` handle in
one request. Remind persists that binding separately, uses the principal UUID
for ownership, and uses the public ID only in API responses and Hook URLs. The
binding and organization lifecycle rows are locked during creation and become
irreversible tombstones after revocation, closing create-versus-removal races
and preventing reuse of a removed identity.

## D-022 — Cron follows Vixie/Linux day matching exactly

**Status:** Accepted; clarifies D-006 and D-007

Recurring expressions use the five Vixie/Linux fields and are evaluated with a
pinned `cronexpr` release. Sunday is accepted as `0`, `7`, or `SUN`. When both
day-of-month and day-of-week are restricted, either field matching selects the
day, as in Vixie cron. Quartz-only `L`, `W`, and `#` day extensions are rejected
even though the parser library supports them. Stored source is normalized to
single field separators so later evaluation is independent of input spacing.

The selected IANA zone controls wall-clock evaluation. Nonexistent local times
are skipped and both UTC instants in a repeated local interval are eligible.
Regression tests pin Sunday numbering, day-field union semantics, and both DST
boundaries so a dependency update cannot silently change scheduling behavior.

## D-023 — Execution generations isolate already-materialized work

**Status:** Accepted; clarifies D-008 and D-015

Each execution stores the schedule version and kind that produced it, in
addition to the immutable delivery snapshot. Materialization advances a
recurring schedule and snapshots its resulting version in one transaction. A
terminal one-time delivery changes the parent schedule to `completed` only when
that execution still belongs to the current version and the schedule is still
one-time. Thus an execution claimed before a pause, resume, timing edit, or
one-time/recurring conversion may finish its own durable history but cannot
complete a newer schedule generation.

## D-024 — Exact idempotent replay precedes mutable-state validation

**Status:** Accepted; clarifies D-013

Create and patch first look up an unexpired completed idempotency record by its
full actor, organization, operation, target, key, and canonical request hash.
An exact replay returns the original status and body before revalidating current
time, current schedule version, or current identity state. This preserves the
meaning of a committed replay after `run_at` passes or the resource changes.
The transactional reservation remains authoritative for concurrent first
requests, and a different hash under the same scope still conflicts.

## D-025 — Normative signed IAM application events replace the temporary lifecycle API

**Status:** Accepted; supersedes D-020

IAM now publishes a versioned application-webhook contract, so Remind receives
it at `POST /internal/v1/iam/events` without the internal bearer credential.
The receiver requires the four `X-Silicon-IAM-*` headers, selects a retained
positive key version, rejects duplicate security headers and timestamps more
than five minutes away, and verifies constant-time HMAC-SHA-256 over
`{timestamp}.{exact raw body}` before JSON parsing. The HMAC key is the complete
literal `whs_` credential, matching IAM's sender; this intentionally differs
from Hook's `whsec_` contract, whose suffix is decoded before signing. Header
and body event UUIDs must agree, and exact event replays are deduplicated by a
durable receipt while changed reuse conflicts.

The receiver accepts IAM's published `spec_version: "1.0"` event vocabulary.
A Silicon `organization.membership.removed.v1` event revokes its principal; an
`organization.updated.v1` event with `status: "disabled"` revokes the tenant.
Other currently published events are recorded as processed no-ops. Revocation
atomically writes an irreversible tombstone before bounded inline schedule,
execution, and destination cleanup. Every public read, materialization, claim,
and destination lookup also joins active lifecycle state, so safety is immediate
even when the worker must drain more rows in later bounded batches.

## D-026 — Hook destination secrets have an explicit cryptographic lifecycle

**Status:** Accepted; extends D-010 and D-012

Trusted provisioning validates the exact Hook origin, Silicon path identity,
canonical routing key, and one-time `whsec_` credential before encrypting both
URL and secret independently with AES-256-GCM, fresh nonces, versioned keys, and
field/tenant/identity associated data. Active rows encrypted under an older key
are opportunistically re-encrypted by bounded worker sweeps using optimistic
row versions and append-only audit records.

Disabling a destination immediately removes it from delivery and assigns the
same 45-day recovery window as other archived Remind data. The worker then
permanently removes its ciphertext in bounded, multi-worker-safe batches.
Operators retain old encryption keys until no row references them; rewrap makes
active-key retirement converge without exposing plaintext or logging secrets.

## D-027 — Worker operations are bounded, observable, and schema-aware

**Status:** Accepted; extends D-017 and D-018

API and worker readiness use a two-second PostgreSQL deadline, verify every
embedded SQLx migration version and checksum, and check critical schema fields.
Rows from a newer migration binary are tolerated for rolling deploys, while a
missing, failed, changed, or manually damaged required schema fails readiness.

The worker exposes only `/health/live`, `/health/ready`, and `/metrics` on its
separate operational listener, loopback port `9090` by default, using the same
metrics registry as its processing loops and graceful shutdown. Scheduler,
delivery, revocation cleanup, and retention batches are positive and capped at
10,000. Delivery has its own concurrency cap, never claims more work than it can
start, and cannot exceed the scheduler batch. The lease must exceed the Hook
request deadline, two database acquire-plus-statement budgets, one poll
interval, and five seconds. Delivery remains at-least-once; Hook idempotency by
execution UUID is the correctness boundary if a pathological delay outlives a
lease.

## D-028 — The unreleased baseline schema is consolidated

**Status:** Accepted

This repository is a greenfield service with no deployed Remind database, so
identity bindings, execution-generation fields, lifecycle tombstones, and
destination-retention fields are consolidated into the initial schema and its
trigger/index migration. This keeps a fresh installation deterministic and
avoids carrying superseded intermediate layouts before the first release. Once
the baseline has been deployed, these migration files become immutable and all
schema evolution uses new forward-only migrations.

## D-029 — Hook delivery accepts only documented endpoint identities

**Status:** Accepted; clarifies D-010

Provisioned URLs must share the configured Hook origin, contain no userinfo,
query, or fragment, and identify the exact global Silicon plus an uppercase
six-hex routing key. Remind accepts Hook's canonical root path and its documented
`/api/v1` compatibility alias, with optional trailing slash, but no other path.
For delivery it decodes the unpadded base64url characters after `whsec_` to the
required 32-byte HMAC key and signs the exact serialized body. The execution UUID
is the stable idempotency key, `occurred_at` equals the scheduled occurrence,
redirects are disabled, and success requires Hook's `202` receipt containing a
valid event UUID.

## D-030 — Remind does not infer authority from legacy IAM integration variants

**Status:** Accepted

IAM's current public constraints are normative: organization IDs and both
segments of a global Silicon ID allow 3–50 lowercase label characters, and the
application webhook uses the `X-Silicon-IAM-*` headers and version 1.0 envelope
described in D-025. Remind enforces those same identifier bounds across HTTP,
repository validation, and PostgreSQL constraints.

The older `X-Silicon-*` header set, `timestamp.event_id.body` signature input,
and `id`/`type`/`schema_version` envelope are deliberately not auto-detected.
That variant has no signed key version and its generic lifecycle payload lacks
the tenant/public-principal projection Remind needs to revoke safely. Accepting
it as equivalent would create ambiguous authentication and false confidence in
cleanup. Any IAM producer still emitting that variant must migrate to the
published application-webhook contract before Remind is deployed.

The same fail-closed rule applies to token introspection. Remind requires IAM's
published `principal_id`, `actor_type`, public `org_id`, and `expires_at`
projection after sending `X-Org-ID`. It does not infer an actor kind from a
generic `sub` or equate IAM's internal organization UUID with its public
organization handle. The IAM runtime must implement that documented endpoint
and response before public Remind traffic is enabled.

## D-031 — The 45-day archive period is product policy, not configuration

**Status:** Accepted; clarifies D-012 and D-017

Schedule and disabled-destination purge deadlines are assigned and constrained
by PostgreSQL to exactly 45 days. Remind therefore does not expose an archive
retention environment variable: a runtime knob could disagree with already
persisted deadlines and imply a configurability that `UNDERSTANDING.md` does
not permit. Idempotency retention and bounded sweep cadence remain configurable
because they are operational policies rather than user-visible archive rules.
