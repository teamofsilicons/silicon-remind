# Public HTTP API

The production origin is `https://backend.remind.teamofsilicons.com`. Routes begin with
`/api/v2` (API contract 2); every request names it with `X-Remind-API-Version: 2`. JSON requests
use `Content-Type: application/json`. Errors look like
`{"error":{"code":"…","message":"…","hint":"…","request_id":"…"}}` (`hint` when there is a
next step); quote the request id when reporting a problem. The OpenAPI document is
[`openapi.yaml`](/openapi.yaml).

Contract 1 (`/api/v1`) is retired: every `/api/v1` path answers `410 api_version_retired` with a
pointer to v2.

## Authentication

Send a Silicon Accounts access token issued to Remind:

```http
Authorization: Bearer <access token>
X-Remind-API-Version: 2
```

The token is an EdDSA JWT with `aud` = `remind`, checked against Silicon Accounts' published
keys (`iss`, `aud`, expiry). Before actions that cannot be undone or that reveal a secret
(archiving a reminder, adding or ending webhook subscriptions, sharing, allow-lists, test
environment keys, retiring an environment), Remind also asks Silicon Accounts whether the token
is still active. Sign-in happens between your client and Silicon Accounts; Remind never sees a
refresh token:

- Carbons: the device flow (`POST {ACCOUNTS}/v1/device/authorize` with `client_id=remind`, then
  poll `POST {ACCOUNTS}/v1/oauth/token`), as `remind login` does.
- Silicons: a short-lived token from `silicon-accounts login --app remind -q`, exchanged at
  `POST {ACCOUNTS}/v1/oauth/token` with `grant_type=urn:silicon:params:oauth:grant-type:slt`,
  `slt` and `client_id=remind`.
- Websites: the hosted sign-in pages and a code exchange with Remind's app secret, on a server.

[Signing in and who sees what](../accounts.md) explains both flows and the rules below.

| 401 code | meaning |
|---|---|
| `unauthenticated` | no credential, or one Remind cannot read |
| `token_expired` | the access token expired; refresh it |
| `token_revoked` | issued before the account signed out, was removed or deleted; sign in again |
| `token_wrong_audience` | issued to another app; another app reads with a verification proof instead |
| `token_kind_mismatch` | the token's `kind` differs from what Remind knows for that account |
| `legacy_token_rejected` | a token from contract 1 (`oat_`/`ort_`); sign in with Silicon Accounts |
| `proof_as_bearer` | a `sap_` proof sent as `Bearer`; send it as `Proof` |
| `account_deleted` | the account was deleted |

Every 401 carries `WWW-Authenticate: Bearer realm="remind"`.

### Other apps: verification proofs

Another app reads on an account's behalf with a Silicon Accounts User verification proof:
`Authorization: Proof sap_…`. Remind verifies it with Silicon Accounts and requires that it is
valid, a User verification proof for receiving app `remind`, carries the scope
`remind.schedules.read`, and comes from an app the deployment allows for that scope
(`REMIND_PROOF_ISSUERS`). It is accepted only on `GET /schedules`, `GET /schedules/{id}`,
`GET /schedules/{id}/executions`, `GET /silicons` and `GET /auth/me` (elsewhere
`401 proof_not_accepted`), and reads exactly what that account may read.

## Who may do what

Every reminder belongs to the Silicon that created it. Accounts are identified by their Silicon
Accounts `uuid` (short, case-sensitive text) and shown by their current `c:`/`si:` id.

- A Silicon reads its own reminders and those of its custodian's other Silicons, and changes only
  its own.
- A Carbon reads the reminders of the Silicons it looks after (it is their custodian). Carbons
  never create, change, pause or archive reminders, and never subscribe webhooks.
- Anyone reads what a Silicon (or its custodian) shared with them (`/viewers`).
- Sharing with a Silicon outside the owner's custodian and its other Silicons needs that Silicon
  (or its custodian) to have allowed the owner first (`/allowed-accounts`).

A reminder you cannot read answers 404, as if it did not exist.

## The calling account

| Method and path | Result |
| --- | --- |
| `GET /auth/me` | `{uuid, kind, id, display_name, pfp_url, custodian, can_manage_reminders, credential, issuing_app, visible_silicons}` |
| `GET /silicons?after=<uuid>&limit=50` | The Silicons whose reminders you can read: `{uuid, silicon_id, display_name, pfp_url, relation, reminder_count}`; `relation` is `self`, `custodian`, `sibling` or `shared` |

`credential` is `access_token` or `proof` (then `issuing_app` names the app). `/silicons` pages
by uuid: pass `next_cursor` as `after`.

## Reminders

| Method and path | Required input | Result |
| --- | --- | --- |
| `POST /schedules` | `text`, `kind`, `cron`, `timezone`; `Idempotency-Key` | `201` reminder |
| `GET /schedules` | optional filters below | page of reminders |
| `GET /schedules/{id}` | reminder id | the reminder |
| `PATCH /schedules/{id}` | changed fields; `Idempotency-Key` | updated reminder |
| `PATCH /schedules` | `schedule_ids`, `status`; `Idempotency-Key` | results in the order given |
| `DELETE /schedules/{id}` | owner Silicon | `204`; archives it |
| `GET /schedules/{id}/executions` | optional `cursor`, `limit` | delivery history |

```json
{
  "text": "Check the build results",
  "kind": "recurring",
  "cron": "*/15 * * * *",
  "timezone": "Asia/Kolkata"
}
```

`kind` is `recurring` or `one_time`. Both use five-field Linux cron: minute, hour, day of month,
month, day of week. One-time means the first future match; the reminder then moves to the
archive. An IANA timezone is mandatory on creation (`"timezone": "UTC"` for UTC); a missing,
null or blank one answers `422 timezone_required`, and unknown identifiers, display names and
fixed offsets are refused.

Cron supports Linux/Vixie lists, ranges, steps and named months and weekdays. Sunday is 0 or 7.
When both day of month and day of week are restricted, either may match. Quartz extensions (`L`,
`W`, `#`) are refused. A wall-clock time skipped by a DST change is skipped; both real instants in
a repeated hour are eligible. The next occurrence is recalculated in UTC after each trigger.

Text must be non-blank and at most 100,000 UTF-8 bytes. A reminder carries `id`, `owner`
(`{uuid, id, kind}`), `silicon_id` (the owner's current id), `text`, `kind`, `cron`,
`timezone`, `status`, `section`, `next_run_at`, `archived_at`, `purge_after`, `created_at` and
`updated_at`.

Listing filters: `silicon_id` (`si:` id or uuid), `section=current|archived`,
`status=active|paused|completed`, `cursor`, `limit` (1 to 100). `current` is the default. Pass
`next_cursor` unchanged with the same filters for the next page.

PATCH accepts `text`, `timezone`, `kind`, `cron` and `status`; omitted fields are unchanged and
cron cannot be cleared. Timing changes recalculate the next occurrence; a text-only change keeps
it. Only `active` and `paused` can be written; `completed` is set when a one-time reminder fires.
Archived reminders cannot change.

Pause or resume 1 to 100 distinct reminders at once:

```json
{"schedule_ids":["0198f74d-7ef7-7c9f-95bf-7d403a61e5ca"],"status":"paused"}
```

Every id must be a current reminder of the caller, or the whole batch fails. Pausing stops
future occurrences; resuming computes the next future match; repeating the same state changes
nothing. Reuse an `Idempotency-Key` (16 to 255 visible ASCII characters) only with the exact same
request; a different body answers `409 idempotency_conflict`.

## Sharing and allow-lists

| Method and path | Who | Result |
| --- | --- | --- |
| `GET /viewers` | anyone | `{granted, received}`: grants on your (or your Silicons') reminders, and grants you hold |
| `POST /viewers` | the Silicon, or its custodian | `{id, silicon_id?}` → `201` grant (`200` when it already existed) |
| `DELETE /viewers/{viewer}?silicon_id=` | the Silicon, or its custodian | `204` |
| `GET /allowed-accounts?silicon_id=` | the Silicon, or its custodian | `{items}` |
| `POST /allowed-accounts` | the Silicon, or its custodian | `{id, silicon_id?}` → `201` entry (`200` when it already existed) |
| `DELETE /allowed-accounts/{account}?silicon_id=` | the Silicon, or its custodian | `204`; also ends the grants the entry made possible |

`id` names the other account by `c:`/`si:` id or uuid (Remind looks it up at Silicon Accounts).
A Carbon names which of its Silicons the request is about with `silicon_id`; a Silicon may omit
it. A grant is `{id, owner, viewer, granted_by, created_at}` and an allow-list entry
`{id, silicon, allowed, created_by, created_at}`, accounts as `{uuid, id, kind}`. Granting to a
Silicon outside the owner's custodian and its other Silicons answers `403 silicon_not_open` until
that Silicon allows the owner (or the granting custodian). Both are refused inside a test
environment (`403 not_in_test_environment`).

## Webhook subscriptions

| Method and path | Behavior |
| --- | --- |
| `POST /webhooks` | Add a subscription for the calling Silicon |
| `GET /webhooks?silicon_id=` | A Silicon's own subscriptions, or (read-only, for a Carbon) those of the Silicons it looks after |
| `DELETE /webhooks/{subscription_id}` | End one subscription; `204` |
| `PUT /webhook` | Older single-endpoint form of `POST /webhooks` |
| `GET /webhook` | The first active subscription, without the signing secret |
| `DELETE /webhook` | End every subscription of the calling Silicon; `204` |

Input is `{"endpoint_url":"…","signing_secret":"…"}`; leave out `signing_secret` for unsigned
delivery. The endpoint must be an absolute https URL in production (credentials and fragments
are refused). Endpoint and secret are encrypted at rest and the secret is never returned. A
receipt is `{id, silicon_id, silicon_uuid, version, updated_at}`. Subscriptions are optional:
reminders created without any simply have nowhere to go until one is added. See the
[delivery contract](../webhook-delivery.md).

## Delivery, archive and retention

When a reminder comes due, the worker stores an immutable occurrence, then posts it to every
subscription. The execution id is stable across retries and is the delivery's idempotency key.
The snapshot holds the text at trigger time, the reminder and owner identity, the timezone and
the intended instant. Passing failures retry with bounded backoff; final failures stay in the
execution history. Reminders of an account that removed Remind from its apps stop firing until
it signs in again.

The owner can archive a reminder at any time; a one-time reminder enters the archive when it
fires. Archived reminders and their history stay readable for 45 days. Before a reminder is
removed for good, one JSON line with the reminder, trigger and creator snapshots is written to
the deleted-reminders log, which keeps the newest 100,000 records. The log is internal and not
exposed through the API.

## Test environments

See the [testing guide](../testing-environments.md). Management is a production action with your
access token (a request carrying `X-Remind-Test-Key` here answers `403 test_key_not_allowed_here`):

| Method and path | Result |
| --- | --- |
| `POST /test-environments` | `{name, description?}` → `201 {environment, key}` |
| `GET /test-environments` | page; `include_deleted`, `after` (environment id), `limit` 1 to 100 |
| `GET /test-environments/{id}` | metadata: `{id, owner_uuid, owner, name, description, version, created_at, last_activity_at, deleted_at, purge_after}` |
| `GET /test-environments/{id}/key` | `{environment_id, key}` |
| `POST /test-environments/{id}/key-rotations` | a new key; the old one stops working |
| `DELETE /test-environments/{id}` | `204`; retired, restorable for 30 days |
| `POST /test-environments/{id}/restorations` | restored, with a new key |

Use an environment by adding `X-Remind-Test-Key: <key>` to ordinary requests, with your normal
access token. `GET /testing-environment` and `POST /testing-environment/cleanings` (`204`) work
with the key alone; without a key they answer `400 test_environment_required`. Owners and, for a
Silicon owner, its custodian manage an environment; the owner's custodian and its other Silicons
(or a Carbon owner's Silicons) see it and read its key; anyone with the key uses and cleans it.
An environment holds at most 100 reminders (`409 test_reminder_limit`), retires after 15 days
without activity, and is restorable for 30 days. Sending the retired `iam_test_key` or
`iam_app_secret` fields answers `422 test_key_field_retired`.

## Errors and operational endpoints

`401` means no usable credential (codes above). `403` means a known account lacks the
permission (`not_reminder_owner`, `silicon_only`, `not_custodian`, `not_environment_manager`,
`proof_cannot_write`…). `404` also hides what you cannot read. `409` reports lifecycle or
idempotency conflicts, duplicate active environment names and the test environment reminder
limit. `410 api_version_retired` answers contract 1 paths. `422` reports invalid data. `429` may
include `Retry-After`. `503` means Silicon Accounts or the database could not answer safely; it
never becomes a guessed identity.

`/health/live` reports process liveness and `/health/ready` database readiness (no credential).
`/metrics` is operational; restrict it by deployment networking. `POST /webhook/` (also
`/webhook`) receives Silicon Accounts' signed account events (id and profile changes, custodian
changes, sign-outs, access removal, deletion); it is not the subscription route `/api/v2/webhook`.

## Contract negotiation

`GET /api/versions` lists the contracts this server serves and their state. Send
`X-Remind-API-Version: 2` with `/api/v2` requests; an unsupported or conflicting selection answers
`406` before anything runs, and the selected version comes back on the response. See the
[version policy](../version-policy.md).

## Reports and telemetry

`POST /api/v2/reports` takes `{"message":"steps, expected result, actual result","pr":null}` with
your access token and an `Idempotency-Key`. Messages are at most 16,384 UTF-8 bytes, at most 10
new reports per account per hour, and `pr` must link a pull request of this repository. It
answers `202 {id, status, failure_reason}`; replaying the same key and body answers `200` with the
same receipt, a different body `409`. `GET /api/v2/reports/{id}` is visible only to the account
that sent it. Without the email transport configured, production answers `503`; inside a test
environment reports are `simulated`.

`POST /api/v2/telemetry/events` takes only the bounded `TelemetryEvent` schema, with your access
token. `X-Remind-Telemetry: off` turns observations off for any request. See
[diagnostics](../diagnostics.md).
