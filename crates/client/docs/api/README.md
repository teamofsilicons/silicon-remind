# Public HTTP API

The production service origin is `https://backend.remind.teamofsilicons.com`.
Ordinary routes begin with `/api/v1`. JSON requests use
`Content-Type: application/json`; JSON errors contain `error.code`,
`error.message`, and normally `error.request_id`. Quote the request ID when
investigating a failed call. Bodies are bounded by the deployment's request limit.

## Authentication and organization selection

Authenticate with `Authorization: Bearer <application-access-token>` and
`X-Org-ID: <org-handle>`. These are IAM Application tokens for `tos>remind`.
Remind introspects each request through the official `silicon-iam-client` and
requires matching application audience, organization, principal, membership,
expiry, and testing plane. An IAM refresh token cannot authorize reminder actions.
An unavailable IAM dependency fails closed; it does not become a fabricated user.

Every current member can read their organization's reminders. Only the owner
Silicon can create, edit, pause, resume, archive, or configure its webhook.
Carbon access is read-only for reminders, even if that Carbon is an org owner.
An org owner/admin can administer test environments; their reminder permissions
are still those of a Carbon.

A request carrying `X-Remind-Test-Key: <32-character-key>` uses an isolated Remind
sandbox. Its bearer must be from the linked IAM sandbox. Invalid keys never fall
through to production. Environment lifecycle management uses production identity
and rejects this test header. See [testing environments](../testing-environments.md).

## Application sessions

| Method and path | Request | Result |
| --- | --- | --- |
| `POST /auth/login` | `{"slt":"…"}` | Access/refresh tokens, expiry seconds, actor and optional org |
| `POST /auth/refresh` | `{"refresh_token":"…"}` | Successor access and rotating refresh tokens |
| `POST /auth/logout` | `{"token":"…"}` | `204`; refresh token revokes the whole family |
| `GET /auth/me` | Bearer and org headers | Current identity, disclosed org role and reminder-write capability |

The three POST endpoints accept a token in their JSON body. Remind's Application
secret stays on the server. They do not ask for an IAM password, email, phone,
OTP, or a browser redirect. Pass an `Idempotency-Key` on login and refresh when a
retry must replay the same logical exchange. It must be 16–255 visible ASCII
characters. Session responses use `Cache-Control: no-store`.

Example, with a short-lived token supplied from a protected file:

```sh
curl --request POST "$REMIND_URL/api/v1/auth/login" \
  --header 'Content-Type: application/json' \
  --header "Idempotency-Key: $MUTATION_KEY" \
  --data-binary @login.json
```

`login.json` contains `{"slt":"the-token-from-IAM"}`. Keep tokens out of application
logs. Access and refresh tokens are separate: do not retry a spent refresh token
with a different idempotency key after a successful rotation.

## Webhook configuration and Silicon discovery

| Method and path | Behavior |
| --- | --- |
| `PUT /webhook` | Set the webhook endpoint for the authenticated Silicon |
| `GET /webhook` | Read its endpoint URL and version, without the signing secret |
| `DELETE /webhook` | Disable the endpoint; returns `204` |
| `POST /webhooks` | Add another independent webhook subscription |
| `GET /webhooks` | List active subscriptions, possibly empty |
| `DELETE /webhooks/{subscription_id}` | Disable one subscription; returns `204` |
| `GET /silicons?after=<uuid>&limit=50` | List registered Silicons in this org with reminder counts |

Configuration input is `{"endpoint_url":"…","signing_secret":"…"}`; omit `signing_secret` for unsigned delivery. Use any URL
and an optional textual `signing_secret` chosen for the receiver. The backend validates the endpoint
as an absolute HTTP(S) URL and verifies the Silicon's canonical public ID.
See [the exact sender and receipt contract](../webhook-delivery.md).
Ownership and public routing identity are derived from IAM, never supplied by a
public caller. Endpoint URL and signing secret are encrypted at rest.

Webhook subscriptions are optional. Reminders can be created with no configured
receiver and will begin fan-out delivery when subscriptions are added.

## Reminders

| Method and path | Required input | Result |
| --- | --- | --- |
| `POST /schedules` | `text`, `kind`, `cron`; `Idempotency-Key` | `201` reminder |
| `GET /schedules` | Optional filters below | Page of reminders |
| `GET /schedules/{id}` | Reminder UUID | Visible reminder |
| `PATCH /schedules/{id}` | Changed fields; `Idempotency-Key` | Updated reminder |
| `PATCH /schedules` | `schedule_ids`, `status`; `Idempotency-Key` | Atomic results in supplied ID order |
| `DELETE /schedules/{id}` | Owner identity | `204`; archives the reminder |
| `GET /schedules/{id}/executions` | Optional cursor and limit | Delivery history |

Creation example:

```json
{
  "text": "Check the build results",
  "kind": "recurring",
  "cron": "*/15 * * * *",
  "timezone": "Asia/Kolkata"
}
```

`kind` is `recurring` or `one_time`. Both use five-field Linux cron in the order
minute, hour, day of month, month, day of week. One-time means the first future
matching occurrence, not a separate timestamp format. Omitted timezone means UTC.
Use an IANA identifier such as `Asia/Kolkata`, not a display name or fixed offset.

Cron supports Linux/Vixie lists, ranges, steps and named months/weekdays. Sunday
is 0 or 7. When both day-of-month and day-of-week are restricted, either may
match. Quartz extensions such as `L`, `W` and `#` are rejected. A nonexistent DST
wall-clock time is skipped; both real instants in a repeated interval are
eligible. The next occurrence is recalculated in UTC after each trigger.

Text must be nonblank and no more than 100,000 UTF-8 bytes. Responses include the
UUID, org, public Silicon ID, text, kind, cron, timezone, status, section, next UTC
occurrence, archive/deletion deadline, and creation/update timestamps.

Listing filters are `silicon_id`, `section=current|archived`,
`status=active|paused|completed`, `cursor`, and `limit` (1–100). Current is the
default section. Pass `next_cursor` unchanged into the next request, keeping the
same filters and organization. An empty result has `items: []`.

PATCH accepts text, timezone, kind, cron and status. Omitted fields are unchanged.
Cron cannot be cleared. Timing changes recalculate the next future occurrence;
a text-only change preserves it. Only `active` and `paused` are client-writable
statuses. `completed` is assigned by the backend when a one-time occurrence is
materialized. Archived reminders are immutable.

For atomic pause/resume, send 1–100 distinct UUIDs:

```json
{"schedule_ids":["0198f74d-7ef7-7c9f-95bf-7d403a61e5ca"],"status":"paused"}
```

All IDs must identify current reminders owned by the caller. Any invalid owner,
missing reminder, or invalid lifecycle state fails the entire batch. Pausing
suppresses future materialization; already-materialized occurrences retain their
delivery lifecycle. Resuming calculates the next future cron match. Reapplying
the same state is a no-op. Reuse the same idempotency key only with the exact same
operation and input; changing input returns `409 idempotency_conflict`.

## Delivery, archive and retention

The worker stores an immutable occurrence before sending it to the configured webhook endpoint.
Execution ID is stable across retries; it is also webhook's idempotency identifier.
The snapshot contains the reminder text at trigger time, schedule identity,
Silicon ID, timezone, and intended trigger instant. Transient/ambiguous failures
retry with bounded backoff; terminal errors are retained in execution history.

The owner can archive a reminder at any time. One-time reminders automatically
enter the archive when their occurrence is materialized. Archived reminders and
history remain readable for 45 days. Read and delivery queries enforce that
expiry even if the cleanup worker is delayed. Before permanent removal, the
worker writes one JSON text line with reminder, trigger and creator snapshots;
the ledger retains the newest 100,000 records within that database/schema.
The deletion ledger is backend-internal and is not exposed by the client or CLI.

## Environment lifecycle

See the [dedicated sandbox guide](../testing-environments.md) for setup and
permissions. Public control operations are:

| Method and path | Result |
| --- | --- |
| `POST /test-environments` | `{environment, key}` |
| `GET /test-environments` | Page; `include_deleted`, UUID `after`, and 1–100 `limit` |
| `GET /test-environments/{id}` | Metadata |
| `GET /test-environments/{id}/key` | `{environment_id, key}` |
| `POST /test-environments/{id}/key-rotations` | New key; previous key revoked |
| `DELETE /test-environments/{id}` | `204`; retires for 30-day recovery |
| `POST /test-environments/{id}/restorations` | Fresh key and restored environment |
| `GET /testing-environment` | Root-key-only selected sandbox metadata |
| `POST /testing-environment/cleanings` | Root-key-only atomic clear; `204` |

## Errors and operational endpoints

`401` means missing, expired, revoked or mismatched authority. `403` means a
recognized actor lacks the action's permission. `404` also hides resources in
other organizations. `409` reports lifecycle/idempotency conflicts, absent
webhook configuration, duplicate active environment names, or the sandbox's
100-reminder limit. `422` reports invalid data. `429` may include `Retry-After`.
`503` means an authority or storage dependency could not answer safely.

Origin-relative `/health/live` reports process liveness. `/health/ready` verifies
production schema readiness. `/metrics` is an operational endpoint and should be
restricted by deployment networking. The IAM receiver is the origin-relative
`POST /webhook/`; it verifies signed raw bodies and is not the user configuration
route `/api/v1/webhook`.

Sandbox creation accepts an optional `iam_app_secret`. If omitted, root metadata,
cleaning and configuration are available immediately; authenticated actions wait
for `PUT /testing-environment/iam` with `{"iam_app_secret":"<test-only-secret>"}`
and the Remind root header. That route returns 204 and requires no actor bearer.
See [the sandbox setup guide](../testing-environments.md).
