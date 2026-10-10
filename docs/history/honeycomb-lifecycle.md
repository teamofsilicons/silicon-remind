# Honeycomb lifecycle integration

Honeycomb owns shared environment creation, cleaning, key rotation, disable/restore and permanent removal. Remind prepares and clears only its isolated schema; IAM remains responsible for test identities, application secrets, readiness and permission checks.

Configure `REMIND_HONEYCOMB_SERVICE_TOKEN` in the deployment secret store and the matching Honeycomb participant entry for the configured Remind application ID. The token must contain at least 32 visible ASCII characters. It is independent of IAM user sessions, application secrets, the environment root key and the existing internal provisioning token. Configure `REMIND_HONEYCOMB_BASE_URL` as the coordinator origin for activity reports (HTTPS in production). Run `remind-migrate` before starting the updated API and worker.

Honeycomb sends its participant request to:

```text
PUT /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}
Authorization: Bearer <dedicated service token>
```

The body contains matching `org_id`, `environment_id`, `operation_id`, `app_id`, positive `environment_revision`, `generation`, `key_version`, `action`, `testing_key`, and optional `snapshot`, `reason`, `retired_apps`. Unknown fields and mismatched paths/applications are rejected. Supported actions are `prepare`, `import`, `refresh-import`, `rotate-key`, `clean`, `disable`, `restore`, `purge`, and `retire-applications`. Retirement clears Remind only when its exact app ID is selected.

Retry with the same operation ID and identical body. Receipts report `pending`, `completed` or `failed` and echo the environment, application, operation, revision, generation, key version and retirement selection. `GET` on the same URL retrieves a receipt, including after purge. Changed replays and stale revisions are rejected. Failed or interrupted cleanup leaves the environment fenced until that operation succeeds. Completion and data cleanup commit atomically. Historical successful replays return their receipt without modifying newly created data.

Prepare creates an empty schema using production migrations. Cleaning clears reminders, schedules, subscriptions, executions and delivery attempts, history, identity projections, deletion logs, idempotency records, audit records, reports and sandbox telemetry. It resets contract usage counters. The participant link and lifecycle receipts remain outside this schema. Purge removes the schema and keeps a tombstone to prevent delayed discovery or operations from recreating it.

Runtime admission holds a shared database fence; lifecycle changes take the exclusive fence. Requests resolved before a lifecycle change are rechecked before use. IAM context is resolved again under the fence and must match the current root-key digest and version. Following cleanup, the IAM cleanup timestamp must advance. IAM rejects access while shared readiness is pending. Disabled or retired environments cannot admit requests, scheduler work or retries. Restoring re-enables admission only after IAM confirms readiness and never restores cleared records.

Each outbound test delivery rechecks live IAM state. Signed IAM events from before cleanup are acknowledged without writing their receipt or projection back into the cleared schema. Test deliveries are simulated unless the exact receiver URL is explicitly configured in `REMIND_TEST_WEBHOOK_URLS`.

Successful user activity is persisted separately from scheduler polling and sent to Honeycomb's environment/application activity endpoint with the current root key, generation, key version and a stable idempotency key. Failed reports remain pending. Cleaning discards old-generation activity. Remind does not independently retire Honeycomb-managed environments.

Local integration tests use PostgreSQL and mock IAM/Honeycomb servers to exercise cleanup fencing, altered replays, disabled sessions, key rotation, failed-operation recovery, old webhook rejection, activity retries, selected retirement, and purge tombstones. These checks do not establish deployed cross-service readiness; deployment requires matching participant configuration and credentials in Honeycomb.
