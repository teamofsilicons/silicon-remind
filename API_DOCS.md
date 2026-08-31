# Silicon Remind API documentation

This document explains every operation in the Silicon Remind OpenAPI contract. The machine-readable contract is in [`openapi.yaml`](./openapi.yaml).

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

## Schedules

### `GET /schedules`

Lists schedules visible to the current actor.

- **Authentication:** Bearer token.
- **Filters:** `silicon_id` and status.
- **Pagination:** Cursor and limit.
- **Returns:** Schedules and next cursor.

Statuses are `active`, `paused`, and `completed`. A completed one-time schedule has fired; a recurring schedule normally remains active until paused or deleted.

### `POST /schedules`

Creates a one-time or recurring schedule for the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Required:** Reminder `text`, IANA `timezone`, and exactly one of `run_at` or `cron`.
- **Required header:** `Idempotency-Key`.
- **Returns:** Created schedule.

`run_at` is an absolute date-time for a one-time execution. `cron` is a five-field Linux cron expression interpreted in the supplied time zone.

The creator becomes the schedule owner. Remind calculates and returns `next_run_at`.

### `GET /schedules/{schedule_id}`

Returns one visible schedule.

- **Authentication:** Bearer token.
- **Returns:** Owner Silicon, expression, timezone, state, next run, and timestamps.

Organization boundaries must be checked even when the caller knows the schedule UUID.

### `PATCH /schedules/{schedule_id}`

Updates a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Input:** Text, timezone, `run_at`, `cron`, or status.
- **Required header:** `Idempotency-Key`.
- **Returns:** Updated schedule.

Changing between one-time and recurring behavior requires clearing the old field. The resulting schedule must contain exactly one of `run_at` or `cron`.

Pausing prevents future execution without deleting history. Resuming recalculates the next occurrence. Updating a schedule must define how an execution already claimed by a worker behaves.

### `DELETE /schedules/{schedule_id}`

Deletes a schedule owned by the authenticated Silicon.

- **Authentication:** Silicon bearer token.
- **Returns:** `204 No Content`.

Deletion prevents future executions. The contract currently does not define whether schedule metadata and execution history are retained for audit.

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
  "subject": "schedule-id",
  "occurred_at": "2026-08-31T09:00:00Z",
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
  -> worker claims occurrence
  -> Remind creates stable execution ID
  -> Remind sends signed Hook event
  -> Hook durably accepts event
  -> Remind marks execution delivered and schedule completed
```

### Recurring reminder

```text
Silicon creates cron + timezone schedule
  -> Remind calculates next occurrence
  -> every occurrence receives a unique execution ID
  -> retries reuse that occurrence ID
  -> successful Hook acceptance advances next_run_at
```

## Contract gaps

- OBO Access for applications is described in the understanding but not included in this OpenAPI contract.
- Cron validation and human-readable schedule preview endpoints are missing.
- Daylight-saving gaps and repeated local times need explicit rules.
- Missed execution, downtime catch-up, and misfire policies are undefined.
- Retry intervals, maximum attempts, and terminal failure notifications are undefined.
- Manual trigger and execution retry operations are missing.
- Schedule deletion and execution-history retention are unspecified.
- There is no explicit Hook destination reference in the schedule schema.
- Bulk pause, resume, and organization-wide schedule controls are missing.
- Viewing every Silicon's schedules may expose sensitive reminder text and needs an authorization decision.
- Exactly when `completed` applies to one-time versus recurring schedules should be normative.
