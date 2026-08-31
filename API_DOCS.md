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

- **Bearer authentication:** IAM access token.
- **Organization context:** Requests require `X-Org-ID`.
- **Creation authority:** Only an authenticated Silicon creates schedules.
- **Visibility:** Organization Carbons and Silicons can inspect schedules under the current product rules.
- **Idempotency:** Schedule creation and updates require `Idempotency-Key`.

Remind does not create schedules for Carbons. A Carbon may view schedules if authorized but is not the schedule owner.

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

## Schedules

### `GET /schedules`

Lists schedules visible to the current actor.

- **Authentication:** Bearer token.
- **Filters:** `silicon_id` and status.
- **Pagination:** Cursor and limit.
- **Returns:** Schedules and next cursor.

Statuses are `active`, `paused`, and `completed`. A one-time schedule becomes
`completed` after its current execution is delivered or reaches terminal
failure. A recurring schedule remains active until paused or deleted.

### `POST /schedules`

Creates a one-time or recurring schedule for the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Required:** Reminder `text`, IANA `timezone`, and exactly one of `run_at` or `cron`.
- **Required header:** `Idempotency-Key`.
- **Returns:** Created schedule.

`run_at` is an absolute date-time for a one-time execution. `cron` is a
five-field Vixie/Linux expression interpreted in the supplied time zone. Sunday
may be `0`, `7`, or `SUN`; restricted day-of-month and day-of-week fields match
as a union. Quartz-only `L`, `W`, and `#` extensions are rejected. A nonexistent
DST wall time is skipped, while both real instants in a repeated interval are
eligible.

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

### `PATCH /schedules/{schedule_id}`

Updates a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Input:** Text, timezone, `run_at`, `cron`, or `active`/`paused` status.
- **Required header:** `Idempotency-Key`.
- **Returns:** Updated schedule.

Changing between one-time and recurring behavior requires clearing the old field. The resulting schedule must contain exactly one of `run_at` or `cron`.

Pausing prevents new materialization without deleting history. Resuming
recalculates the next occurrence. An already materialized execution retains its
immutable text, timezone, public destination, scheduled instant, schedule kind,
and generation. It may finish its own history, but a stale one-time generation
cannot mark a later edited or converted schedule `completed`.

### `DELETE /schedules/{schedule_id}`

Deletes a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Returns:** `204 No Content`.

Deletion immediately hides the schedule and prevents unaccepted work from being
claimed again. Its metadata and execution history remain archived for 45 days,
then a bounded worker sweep permanently removes them.

## Executions

### `GET /schedules/{schedule_id}/executions`

Lists execution history for a schedule.

- **Authentication:** Bearer token.
- **Pagination:** Cursor and limit.
- **Returns:** Scheduled time, attempt time, delivery time, status, Hook event ID, and failure reason.

Each occurrence has a stable execution UUID. That UUID is used as the Hook idempotency key so retries cannot cause multiple logical reminders.

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
Silicon creates schedule with run_at
  -> Remind stores and calculates next_run_at
  -> scheduler materializes a durable execution with a stable ID
  -> delivery worker claims that execution
  -> Remind sends signed Hook event
  -> Hook durably accepts it, or bounded retries reach terminal failure
  -> Remind records the terminal outcome and completes the matching generation
```

### Recurring reminder

```text
Silicon creates cron + timezone schedule
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
- missed recurring occurrence coalescing (D-009);
- retry classification, attempt limits, and terminal history (D-011);
- deletion, completion, and 45-day retention (D-012, D-023);
- principal/public-ID binding and encrypted Hook destination provisioning
  (D-021, D-026, D-029); and
- organization-wide schedule visibility under the current product rule (D-005).

## Deferred public operations

- OBO Access for applications is not included in this OpenAPI contract.
- Cron validation and human-readable schedule preview endpoints are absent.
- Manual trigger and execution-redelivery operations are absent.
- Bulk pause, resume, and organization-wide controls are absent.
- The contract has no finer-grained reminder-text visibility capability beyond
  the current organization access rule.
