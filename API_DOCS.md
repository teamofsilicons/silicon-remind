# Silicon Remind API documentation

This document explains every operation in the Silicon Remind OpenAPI contract.
The machine-readable contract is in [`openapi.yaml`](./openapi.yaml). Internal
provisioning and IAM webhooks are documented in
[`INTERNAL_API.md`](./INTERNAL_API.md).

## API conventions

### Base URL

```text
https://remind.teamofsilicons.com/api/v1
```

Remind manages one-time and recurring schedules for Silicons. When a schedule becomes due, Remind emits an idempotent event through Silicon Hook.

### Authentication

- **Bearer authentication:** IAM application token issued for the Remind audience.
- **Organization context:** Requests require `X-Org-ID`.
- **Creation authority:** Only an authenticated Silicon creates schedules.
- **Visibility:** A Silicon can inspect every visible reminder in the selected
  organization. A Carbon can inspect only reminders owned by Silicons in IAM's
  request-scoped `remind_permitted_silicon_principal_ids` projection.
- **Idempotency:** Schedule creation and updates require `Idempotency-Key`.

Remind does not create schedules for Carbons. Carbon access is read-only. IAM
derives the projection from its authoritative access policy (shared tags plus
explicit extra-Silicon grants); Remind never treats same-organization
membership or a client-supplied `silicon_id` filter as authority. An empty
projection returns an empty list, and an inaccessible schedule or execution
history returns `404` without disclosing whether it exists. Only the owner
Silicon can update or archive a reminder.

### IAM application events

IAM sends reviewed application events to
`POST /internal/v1/iam/events`. This receiver is not bearer-authenticated. It
requires `X-Silicon-IAM-Event-ID`, `X-Silicon-IAM-Timestamp`,
`X-Silicon-IAM-Key-Version`, and `X-Silicon-IAM-Signature`. The signature is
`v1=` plus the lowercase hexadecimal HMAC-SHA-256 of
`{timestamp}.{exact raw request body bytes}`, using the complete literal `whs_`
credential at the indicated retained version. Requests outside the five-minute timestamp window,
unknown versions, duplicate security headers, or mismatched header/body event
IDs are rejected before state changes.

The body uses IAM's `spec_version: "1.0"` envelope with a UUID `event_id`,
versioned `event_type`, `occurred_at`, aggregate identity/version, and an
event-specific `data` object. A Silicon
`organization.membership.removed.v1` event atomically revokes that principal's
Remind state. An `organization.updated.v1` event whose data reports
`status: "disabled"` revokes the organization. Other published IAM events are
accepted idempotently as durable no-ops until Remind assigns event-specific behavior. Hook-destination
provisioning endpoints remain protected by the internal bearer credential.

Event names are validated as lowercase dotted identifiers ending in a positive
`vN` schema version rather than deserialized through a closed enum. This keeps
IAM's additive event vocabulary forward-compatible while the two lifecycle
events above retain strict event-specific payload validation.

## Schedules

### `GET /schedules`

Lists schedules visible to the current actor.

- **Authentication:** Bearer token.
- **Filters:** `silicon_id`, status, and `section` (`current` by default or
  `archived`).
- **Pagination:** Cursor and limit.
- **Returns:** Schedules and next cursor.

The IAM owner-principal projection is applied by PostgreSQL before the cursor,
ordering, and limit. A page therefore contains up to the requested number of
authorized rows even when newer reminders belong to inaccessible Silicons.

Statuses are `active`, `paused`, and `completed`. Current results contain active
and paused reminders. Archived results contain owner-archived reminders plus
one-time reminders, which become `completed` as soon as their cron occurrence
is durably materialized. Every response includes `section`, `archived_at`, and
the fixed 45-day `purge_after` deadline when archived.

### `POST /schedules`

Creates a one-time or recurring schedule for the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Required:** Reminder `text`, `kind` (`one_time` or `recurring`), and `cron`.
- **Optional:** IANA `timezone`; omission canonicalizes to `UTC`.
- **Required header:** `Idempotency-Key`.
- **Returns:** Created schedule.

Both kinds use the same five-field Vixie/Linux cron expression interpreted in
the selected time zone. A `one_time` schedule materializes only its first future
match; a `recurring` schedule continues calculating matches until paused or
deleted. Sunday may be `0`, `7`, or `SUN`; restricted day-of-month and
day-of-week fields match as a union. Quartz-only `L`, `W`, and `#` extensions
are rejected. A nonexistent DST wall time is skipped, while both real instants
in a repeated interval are eligible.

The creator's stable IAM principal becomes the owner, while responses expose its
public global Silicon ID. Trusted Hook provisioning must establish that binding
and leave the destination enabled first. Otherwise creation returns HTTP `409`
with code `webhook_not_configured` and the exact message
`Set the webhook url first.` Remind recalculates this prerequisite inside the
creation transaction, so a concurrent destination disable cannot race a
successful first request. An exact committed idempotency replay still returns
its original response. Remind calculates and returns `next_run_at`.

### `GET /schedules/{schedule_id}`

Returns one visible schedule.

- **Authentication:** Bearer token.
- **Returns:** Owner Silicon, expression, timezone, state, next run, and timestamps.

Organization boundaries must be checked even when the caller knows the schedule UUID.
Carbon owner visibility is checked in the same database lookup; an inaccessible
UUID is indistinguishable from an absent one.

### `PATCH /schedules/{schedule_id}`

Updates a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Input:** Text, timezone, cron, kind, or `active`/`paused` status.
- **Required header:** `Idempotency-Key`.
- **Returns:** Updated schedule.

Changing cron, kind, or timezone recalculates the next UTC occurrence from the
update time. A text-only patch preserves the stored next occurrence. Cron is
required and cannot be cleared.

Pausing prevents new materialization without deleting history. Resuming
recalculates the next occurrence. An already materialized execution retains its
immutable text, timezone, public destination, scheduled instant, schedule kind,
and generation. Archived reminders are immutable.

### `DELETE /schedules/{schedule_id}`

Archives a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Returns:** `204 No Content`.

Archival immediately removes the reminder from the default current section and
atomically marks pending or retrying executions failed with their leases
cleared. It remains readable through `section=archived`, together with its
execution history, for exactly 45 days. Repeating the operation is idempotent
and never extends the original retention deadline. Public reads enforce that
deadline even if the bounded permanent-deletion sweep is delayed.

## Executions

### `GET /schedules/{schedule_id}/executions`

Lists execution history for a schedule.

- **Authentication:** Bearer token.
- **Pagination:** Cursor and limit.
- **Returns:** Scheduled time, attempt time, delivery time, status, Hook event ID, and failure reason.

Execution history remains readable while its parent reminder is retained in the
archived section. Automatically archived one-time executions continue normal
Hook delivery and retry processing; archival records that the cron trigger has
occurred, not that delivery has already succeeded.

Each occurrence has a stable execution UUID. That UUID is used as the Hook idempotency key so retries cannot cause multiple logical reminders.
Execution history inherits the parent reminder's IAM-projected owner scope.

Execution statuses are `pending`, `delivered`, `retrying`, and `failed`.

## Delivery event

When a schedule fires, Remind sends a signed event to the Silicon's configured Hook endpoint. A conceptual event is:

```json
{
  "type": "remind.schedule.triggered",
  "source": "silicon-remind",
  "subject": "schedule-id",
  "occurred_at": "2026-08-31T09:00:00Z",
  "schema_version": "1.0",
  "payload": {
    "execution_id": "execution-uuid",
    "schedule_id": "schedule-uuid",
    "silicon_id": "assistant:tos",
    "text": "Prepare the daily report",
    "scheduled_for": "2026-08-31T09:00:00Z",
    "timezone": "Asia/Kolkata"
  }
}
```

Remind marks the execution delivered after Hook durably accepts it, not after the Silicon processes it.

## Complete flows

### One-time reminder

```text
Silicon creates one_time + cron, optionally with timezone
  -> Remind defaults an omitted timezone to UTC and stores the first match
  -> scheduler materializes a durable execution with a stable ID and archives the reminder
  -> delivery worker claims that execution
  -> Remind sends signed Hook event
  -> Hook durably accepts it, or bounded retries reach terminal failure
  -> Remind records the terminal outcome without changing the archive deadline
```

### Recurring reminder

```text
Silicon creates recurring + cron, optionally with timezone
  -> Remind calculates next occurrence
  -> scheduler materializes the due occurrence and atomically advances next_run_at
  -> downtime coalesces missed recurring times into one due execution
  -> every materialized occurrence receives a unique execution ID
  -> retries reuse that occurrence ID
  -> delivery outcome does not roll back the already scheduled next occurrence
```

## OpenAPI omissions resolved by server policy

The compact public schema does not encode every operational rule. The server
policies above are stable and recorded in [`decisions.md`](./decisions.md):

- cron day matching, DST behavior, and strict-next evaluation (D-007, D-022);
- shared one-time/recurring cron timing and UTC defaulting (D-034);
- missed recurring occurrence coalescing (D-009);
- retry classification, attempt limits, and terminal history (D-011);
- deletion, completion, and 45-day retention (D-012, D-023);
- principal/public-ID binding and encrypted Hook destination provisioning
  (D-021, D-026, D-029); and
- IAM-projected Carbon visibility, organization-wide Silicon reads, and
  owner-only mutation (D-035).

## Deferred public operations

- OBO Access for applications is not included in this OpenAPI contract.
- Cron validation and human-readable schedule preview endpoints are absent.
- Manual trigger and execution-redelivery operations are absent.
- Bulk pause, resume, and organization-wide controls are absent.
- IAM must publish the Remind-specific permitted-Silicon projection in token
  introspection; clients cannot supply or broaden it through this API.
