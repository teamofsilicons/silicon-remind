# Service-only routes and operator commands

These are the parts of Remind that only Silicon Accounts and the people operating Remind use. Nothing here is in
the public Rust client or the CLI, and none of it needs a Carbon's or a Silicon's sign-in. Everything a Carbon, a
Silicon or another app calls is in the [API guide](api/README.md).

## Silicon Accounts app webhook

`POST /webhook/` (also `POST /webhook`) receives Silicon Accounts' signed account events for the app `remind`. Set
it as the app's webhook at Silicon Accounts (`PUT /v1/apps/remind/webhook`, or the developer platform) with the URL
`https://api.remind.teamofsilicons.com/webhook/`, and store the `whsec_…` secret shown once in
`REMIND_ACCOUNTS_WEBHOOK_SECRET`. Pick `custodian_change` besides the default events (or every update): who looks
after a Silicon decides who reads its reminders.

Remind checks `X-Accounts-Signature` over the exact raw body with each configured secret (two, comma-separated, while
a rotation overlaps) and refuses a delivery whose `X-Accounts-Timestamp` is more than five minutes away. Each
`event_id` is recorded and applied once.

| answer | when |
| --- | --- |
| `200 {"event_id":…,"status":"processed"}` | the event was applied |
| `200 … "status":"duplicate"` | this `event_id` was already applied |
| `200 … "status":"ignored"` | a `ping`, or an event type Remind does not act on |
| `401 webhook_signature_invalid` | no signature, a wrong one, or a stale timestamp |
| `400 webhook_body_invalid` | correctly signed, but not a Silicon Accounts event |
| `5xx` | applying it failed; Silicon Accounts retries, and the `event_id` keeps it from applying twice |

What each event changes (a new id, a new custodian, a sign-out, removed access, a deleted account) is described in
[signing in and who sees what](accounts.md#when-an-account-changes). Send a test with
`POST /v1/apps/remind/webhook/test` at Silicon Accounts; Remind answers `ignored`.

## Health and metrics

The API (`:8080` in the container) and the worker's operational listener (`REMIND_WORKER_OPERATIONAL_BIND_ADDR`,
`:9090`) both serve:

- `GET /health/live`: the process is up.
- `GET /health/ready`: the database answers within two seconds and its migration ledger and critical schema fields
  match this build. A reachable but unmigrated database is not ready.
- `GET /metrics`: Prometheus metrics. The worker's are the source for scheduler, delivery, retry and worker-error
  counters.

Only `/health/*` is public on the API host; keep `/metrics` and the worker's listener private (health probes and the
metrics collector only). `GET /api/versions` is part of the public contract.

## Operator commands

Run these with the migration owner's database credentials (`REMIND_MIGRATOR_DATABASE_URL`, and
`REMIND_TEST_MIGRATOR_DATABASE_URL` for the testing database), never from a Carbon's or a Silicon's machine.

```sh
remind-migrate                 # apply the embedded migrations: production, testing, every test-environment schema
remind-migrate link-identities --file mapping.csv --dry-run
remind-migrate link-identities --file mapping.csv [--source <label>]
```

`remind-migrate` with no command applies the migrations. It never runs by itself when the API or worker starts: run
it once per release, before the new API and worker.

`link-identities` re-keys data that belonged to an account of the previous identity service to the Silicon Accounts
account it became. Each line of the file is `iam_principal_id,accounts_uuid`, where the left side is either the
principal's UUID or the `si:`/`c:` id it had; a header line, blank lines and `#` comments are skipped, and `-` (or
nothing) after the comma removes a link. With `REMIND_APP_SECRET` set, every uuid is checked with Silicon Accounts
first (it must exist, not be deleted, and be the same kind); without it, the run links offline. One refused line rolls
back the whole run. Nothing is rewritten: a wrong link is corrected by running again with the right uuid.

It prints a JSON report: `linked` (with each account's reminder and subscription counts), `unchanged`, `unlinked`,
`refused`, `unmatched` (principals that still own reminders or subscriptions and have no link) and
`test_environments_linked`. Until it is linked, an old Silicon's reminders keep firing but nobody can see or change
them. `scripts/suggest-identity-links.py` turns a dry run's `unmatched` list into a mapping file for review; the
production steps are in [the cutover guide](migration/cutover.md).
