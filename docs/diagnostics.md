# Bug reports and telemetry

## Report a bug

```sh
remind report 'Steps to reproduce; expected result; actual result' --pr https://github.com/teamofsilicons/silicon-remind/pull/123
remind report-status <report-id> --json
```

Sign in with your usual Remind SLT first. The PR is optional; without it the CLI shows the repository where you can propose a fix. Include enough reproduction detail to investigate, but never include credentials. Reports go from `remind@teamofsilicons.com` through Postmark to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`, exactly as specified in the product understanding. No other application emails are sent.

A receipt starts `queued`, becomes `sending`, then `sent` or `failed`. `sent` means Postmark accepted the email, not proof of inbox delivery. The worker retries transient failures up to eight attempts, five minutes apart, and records a sanitized failure code. Client retries using the same idempotency key do not create another report. Email transport is at least once: a crash after Postmark accepts a message but before the database acknowledgement can produce a duplicate, with the same report ID and Message-ID. Permanent provider rejection ends retries.

Sandbox reports are `simulated`, remain inside that sandbox, and never reach Postmark. Both normal test identities and production identities retain their actual IAM permissions. Only the submitting actor may read a receipt.

## Control telemetry

- CLI and updater daemon: `remind config telemetry off` (or `on`). `REMIND_TELEMETRY_ENABLED=false` overrides the local setting.
- Rust integrations: `client.with_telemetry(false)`.
- Browser: Settings → Telemetry.
- Raw API: send `X-Remind-Telemetry: off` on requests.
- Service operator: `REMIND_TELEMETRY_ENABLED=false`, then restart API and worker.

Telemetry defaults to enabled. The dedicated Space Station table is [`tos.remindtelemetry`](https://spacestation.teamofsilicons.com/o/tos/tables/remindtelemetry). The backend uses the official `space-station` Rust client. API responses record route templates, method, result, duration and request correlation; workers emit operational heartbeats; client, CLI and daemon calls add source and completion events. The browser uses the official Space Station analytics/event package and reduces outgoing data to fixed event codes, result and timing. Source, step, environment, version and occurrence time accompany server records. The Space Station client adds its normal system metadata.

Event payloads exclude reminder/report text, request/response bodies, credentials, webhook URLs, command arguments and browser input contents. Client event names and fields are allowlisted. Collection never recursively instruments its own endpoint. Missing configuration and remote outages do not fail reminder work. Remote records are bounded by the official client's queue/spool policy; sandbox records are capped at the newest 10000 per schema and are deleted by sandbox cleaning. Sandbox events never enter the production Space Station queue.

## Configure the service

Inject `REMIND_POSTMARK_SERVER_TOKEN` from the secret manager. Use a Postmark server that can send from `remind@teamofsilicons.com`, with its `outbound` transactional stream. This deployment uses the existing Team of Silicons Postmark server (20347625); the account already has its maximum number of servers. The sender domain is DKIM and Return-Path verified. The implementation follows the [Postmark Email API](https://postmarkapp.com/developer/api/email-api). No token is exposed in the public client, website, documentation or error responses.

Inject the dedicated write key as `REMIND_TELEMETRY_TABLE_KEY`, and mount a private writable directory at `REMIND_TELEMETRY_HOME` (default `/var/lib/remind/telemetry`, owned by UID 10001 for the production image). The provisioned write key is backed up in AWS SSM SecureString `/silicon/remind/production/telemetry/table-key`. Keep API and worker spools separate to avoid cross-container daemon socket ownership.

Run migrations before updating API and worker; the new tables are automatically migrated in every existing sandbox through the test database migrator. No separate Space Station root/admin key is needed for normal event writes. Do not put the table key in browser or public package configuration.
