# Bug reports

Remind records no telemetry.

## Report a bug

```sh
remind report 'Steps to reproduce; expected result; actual result' --pr https://github.com/teamofsilicons/silicon-remind/pull/123
remind report-status <report-id> --json
```

Sign in first (`remind login`, or a Silicon's short-lived token as in the [CLI guide](cli/README.md)). The PR is optional; without it the CLI shows the repository where you can propose a fix. Include enough reproduction detail to investigate, but never include credentials. Reports go from `remind@teamofsilicons.com` through Postmark to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`, exactly as specified in the product understanding. No other application emails are sent.

A receipt starts `queued`, becomes `sending`, then `sent` or `failed`. `sent` means Postmark accepted the email, not proof of inbox delivery. The worker retries transient failures up to eight attempts, five minutes apart, and records a sanitized failure code. Client retries using the same idempotency key do not create another report. Email transport is at least once: a crash after Postmark accepts a message but before the database acknowledgement can produce a duplicate, with the same report ID and Message-ID. Permanent provider rejection ends retries.

Reports sent inside a test environment are `simulated`, stay inside it, and never reach Postmark. Inside a test environment you keep your own account and permissions. Only the account that sent a report may read its receipt.

## Configure the service

Inject `REMIND_POSTMARK_SERVER_TOKEN` from the secret manager. Use a Postmark server that can send from `remind@teamofsilicons.com`, with its `outbound` transactional stream. This deployment uses the existing Team of Silicons Postmark server (20347625); the account already has its maximum number of servers. The sender domain is DKIM and Return-Path verified. The implementation follows the [Postmark Email API](https://postmarkapp.com/developer/api/email-api). No token is exposed in the public client, website, documentation or error responses.

Run migrations before updating API and worker; the migrator also brings every existing test-environment schema up to date through the test database migrator.
