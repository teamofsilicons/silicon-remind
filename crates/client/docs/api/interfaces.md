# Build an interface with Silicon Remind

Add `remind` to the interface’s app IDs in Silicon Accounts. After sign-in, take `apps["remind"].access_token` from the Accounts result. If access was declined or that entry is missing, offer sign-in again before opening Remind. Use the following API from a browser or your own backend. It serves the same reminder data and permissions as the Remind website.

| Connection | Value |
| --- | --- |
| Production base | `https://api.remind.teamofsilicons.com/api/v2` |
| Authentication | `Authorization: Bearer <Remind access token>` |
| Contract | `X-Remind-API-Version: 2`. Discover versions at `GET https://api.remind.teamofsilicons.com/api/versions`. |
| JSON and browser access | Set `Content-Type: application/json` when sending JSON. Cross-origin requests support preflight and explicit bearer authentication with `credentials: "omit"`. |
| Full request and response schemas | [OpenAPI 3.1](/openapi.yaml) and the [HTTP reference](README.md) |

## From sign-in to the first reminder

This browser example uses the sign-in result already held by the interface. Keep tokens out of URLs, logs and storage shared with other origins. The example schedules a single reminder at the next 09:00 in Kolkata. Keep the generated creation key with the submitted form until the outcome is known.

```js
const base = 'https://api.remind.teamofsilicons.com/api/v2';
const token = signIn.apps.remind.access_token;
async function remind(path, { method = 'GET', body, key } = {}) {
  const headers = { Authorization: `Bearer ${token}`, 'X-Remind-API-Version': '2' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (key) headers['Idempotency-Key'] = key;
  const res = await fetch(base + path, {
    method, headers, credentials: 'omit', body: JSON.stringify(body),
  });
  if (res.status === 204) return;
  const result = await res.json();
  if (!res.ok) throw Object.assign(new Error(result.error.message), {
    status: res.status, code: result.error.code, requestId: res.headers.get('X-Request-ID'),
  });
  return result;
}
const me = await remind('/auth/me');
if (me.can_manage_reminders) {
  const key = crypto.randomUUID();
  const schedule = await remind('/schedules', { method: 'POST', key, body: {
    text: 'Review the overnight build', kind: 'one_time', cron: '0 9 * * *', timezone: 'Asia/Kolkata',
  }});
  const history = await remind(`/schedules/${schedule.id}/executions`);
}
```

Creation returns `201` with `id`, `text`, `kind`, `cron`, `timezone`, `status`, `next_run_at` and owner metadata. Before the first occurrence, history returns `{"items":[],"next_cursor":null}`. After the worker delivers it, an execution can look like this.

```json
{"items":[{"id":"019a0000-0000-7000-8000-000000000002","schedule_id":"019a0000-0000-7000-8000-000000000001","scheduled_for":"2026-11-19T03:30:00Z","attempted_at":"2026-11-19T03:30:02Z","delivered_at":"2026-11-19T03:30:03Z","status":"delivered","ting_status":"accepted","webhook_status":"none","hook_event_id":null,"failure_reason":null}],"next_cursor":null}
```

## Operations and permissions

Paths below are relative to the production base. Use `GET /auth/me` for capabilities, `GET /silicons` for visible owners, and their immutable Accounts UUIDs for selection. An owning Silicon can create and change reminders. Its custodian, sibling Silicons, and explicitly granted viewers can read them. Use the viewer and allow-list endpoints to manage explicit sharing.

| Frontend feature | API operations | Who can use it |
| --- | --- | --- |
| Account and visible Silicons | `GET /auth/me`, `GET /silicons` | Signed-in Carbon or Silicon |
| Current or archived reminders | `GET /schedules?section=current` or `archived`, `GET /schedules/{id}`, `GET /schedules/{id}/executions` | Caller with read access to the owner |
| Create or edit, pause or resume, archive | `POST /schedules`, `PATCH /schedules/{id}`, `PATCH /schedules` for a batch, `DELETE /schedules/{id}` | Owning Silicon |
| Ting setup | `GET /ting`, `PUT /ting` | Calling Silicon |
| Optional webhook copies | `GET` or `POST /webhooks`, `DELETE /webhooks/{subscription_id}`. Compatibility routes support `GET`, `PUT` and `DELETE /webhook`. | Silicon manages its own endpoints. A custodian can list their Silicons’ subscriptions. |
| Viewers and incoming grants | `GET` or `POST /viewers`, `DELETE /viewers/{viewer}` | Signed-in accounts can inspect their grants. Owner or custodian grants or revokes access. |
| Allow sharing from another account | `GET` or `POST /allowed-accounts`, `DELETE /allowed-accounts/{account}` | Owner or custodian. For a custodian, supply the target `silicon_id`. |
| Manage test environments | `GET` or `POST /test-environments`, `GET` or `DELETE /test-environments/{id}`, `GET /test-environments/{id}/key`, `POST /test-environments/{id}/key-rotations` or `/restorations` | Signed-in account creates its own environment. Owner manages lifecycle. Its custodian circle can inspect the environment and key. |
| Use an isolated environment | Send `X-Remind-Test-Key` on data calls. `GET /testing-environment` and `POST /testing-environment/cleanings` take the key alone. | Holder of the environment key. Normal data routes also require the access token. |
| Report an integration problem | `POST /reports` with `{message,pr}` and an idempotency key, `GET /reports/{id}` | Reporting account. Include the request ID. A test environment simulates reports. |

Production has retired test environments. Hide their controls when `GET /api/versions` returns `features.test_environments: false`. The test routes in the table are available on deployments with a testing database. Discovery also reports `features.ting` and `features.reports`.

See the [HTTP reference](README.md) for query parameters, examples and payloads for each feature, and the [testing guide](../testing-environments.md) for lifecycle and limits.

## Receiving due reminders

Production enrolls the Silicon when it first opens `GET /auth/me` or creates a reminder. A successful enrollment is recorded once. `GET /ting` returns `configured`, `simulated` and `registered_at` (the last successful registration). If registration was unavailable, retry with `PUT /ting`. Use that same operation when the Silicon chooses to reconnect after revoking its subscription in Ting. The recipient controls silent delivery, blocking and subscriptions in [Ting](https://ting.teamofsilicons.com/).

At the due time, Remind sends `remind.reminderDue` to the owner’s immutable UUID. The Ting’s `data` contains the execution ID, schedule ID, reminder text, scheduled UTC instant and IANA timezone. Adding a webhook is optional. Each configured endpoint receives a copy with the same execution ID. Ting and webhook results are recorded independently. `ting_status: accepted` means Ting accepted the notification. `blocked` records a recipient preference. `unsubscribed` indicates an absent grant. `webhook_status: none` means there were no endpoints. `untracked` identifies earlier history. Test environments simulate Ting without contacting the recipient. Read execution history for the durable outcome. A frontend can poll it using the page cursor when traversing older results, and refresh the first page for new occurrences. [Webhook format and signature verification](../webhook-delivery.md)

## Pagination, retries and errors

- Lists return `items` and `next_cursor`. For reminders and history pass that cursor as `cursor`, preserving all filters. For visible Silicons and test environments pass it as `after`. A null cursor ends the list. Page sizes are 1–100 where `limit` is accepted.
- Create, edit and bulk-update reminders and submit reports with an `Idempotency-Key` of 16–255 visible ASCII characters. Reuse the same key and body after a timeout. A different body with that key returns `409`. For a webhook creation or an environment mutation with an uncertain result, read the current state before repeating it.
- The API admits up to 300 authenticated requests per account per minute per API process. Directory resolution has a separate budget of 30 per account per minute. Reports allow 10 per hour. On `429`, honor `Retry-After` when present. On `503`, back off and retry using the original mutation key.
- Every bearer request checks the signature, audience, issuer and validity interval and requires live Accounts introspection for an active `access_token` belonging to `remind`. On `401`, reacquire the app token through the interface’s Accounts session or offer sign-in. On `403`, keep the operation unavailable. A private reminder outside the caller’s view returns `404`. `422` indicates invalid input. Read `error.code`, `error.message`, optional `error.hint` and `error.request_id` when displaying or reporting failures.
