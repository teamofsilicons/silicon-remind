# Mandatory timezone end-to-end verification

Run the native API and CLI checks with:

```sh
python3 scripts/test-timezone-e2e.py --worker
```

The harness builds the local binaries, creates a disposable native PostgreSQL
instance, applies the actual migrations, and runs Remind on loopback. Set `PG_BIN`
to the directory containing `initdb`, `pg_ctl`, and `psql` when PostgreSQL is not
installed at `/opt/homebrew/opt/postgresql@16/bin`.

The harness honors `CARGO_TARGET_DIR`. To use binaries already built from this
checkout, pass `--skip-build --bin-dir /absolute/path/to/target/debug`. Use
`--temp-dir /tmp` to choose a short parent path for the disposable database and
its Unix socket; the harness still creates and removes its own unique directory.

IAM is a local fixture with invented credentials. Remind's authentication
middleware, API, Rust client, CLI, and database are real. No production service or
credential is used. Temporary processes and data are removed when the harness
exits. `--serve` keeps the fixture available for browser checks until interrupted
or its printed stop file is created. `--worker` waits for two upcoming one-time
reminders to reach a local webhook receiver; omit it for creation-only checks.

The fixture models an IAM 5 ordinary session for `si:clock` in `timezone-e2e`,
with the bare application handle `remind` and canonical membership
`si:clock[timezone-e2e]`. Scoped and unscoped introspection both return that one
authorization. This path does not select an IAM testing environment, call
`/application/testing-context`, or verify hosted IAM sign-in.

## Reverified on 2026-10-03 for IAM 5

The current API, migrator, CLI, and worker were built from the isolated migration
checkout with `cargo build --locked --workspace` and the four explicit binaries.
The harness passed against a task-owned PostgreSQL **16.11** cluster with the
canonical IAM fixture:

- CLI login/status and API organization discovery retained `si:clock` and
  `timezone-e2e`.
- All existing CLI/API timezone rejection, no-write, explicit-timezone,
  idempotency, GET, and PATCH checks passed.
- The real worker delivered Kolkata and UTC reminders at
  `2026-10-03T04:08:00Z` and archived both one-time reminders.
- The API, worker, fixture servers, and temporary cluster were stopped, and the
  temporary data directory was removed.

This is separate from the full PostgreSQL **17** Testcontainers suite, which
remains unverified because its Docker-only fixtures cannot start on this host.
Browser checks, live IAM 5, and a deployed release were not tested in this run.

## Verified on 2026-09-20

- The actual CLI rejects missing, valueless, empty, and whitespace-only timezone
  input for both reminder kinds before contacting IAM, with exit code 2 and
  actionable IANA / `--timezone` guidance.
- Authenticated API creation rejects omitted, null, empty, and whitespace-only
  timezone with HTTP 422 and `timezone_required`. Rejections create no reminder,
  execution, idempotency, or audit records. Invalid IANA identifiers are rejected.
- Explicit `Asia/Kolkata` and `UTC` work for recurring and one-time reminders.
  A `0 9 * * *` cron resolves to 09:00 in the selected timezone. GET preserves
  the submitted timezone, retries are idempotent, and a text-only PATCH preserves
  the timezone.
- Actual CLI login, create, and get pass through the API and PostgreSQL for both
  reminder kinds and both timezones.
- The real worker delivered two one-time reminders, one in each timezone, for
  `2026-09-20T15:33:00Z`. The webhook payloads retained the correct timezone and
  intended trigger instant. Both reminders became completed and archived.
- The built web app was exercised in the browser against this API. Authentication
  was bootstrapped through the real frontend `/ui/login` route using the fixture
  SLT; hosted IAM sign-in was not tested.
- Each new browser form starts with an empty timezone. Empty and whitespace-only
  submissions show mandatory-IANA guidance and create no reminder. Explicit
  Kolkata recurring and UTC one-time reminders save successfully; a text-only
  browser edit retains Kolkata. Both reminders remain visible after reopening
  the app, and their timezone and next-trigger values were verified in PostgreSQL.

The delivery check exposed and fixed a development-mode policy mismatch: the API
accepted an HTTP webhook but the worker rejected it. The worker now uses the same
runtime-environment policy as the API. Production still requires HTTPS outside
an isolated sandbox. An encrypted-destination regression checks all eight
HTTP/HTTPS, development/production, and sandbox/non-sandbox combinations.

This verifies the local implementation. It does not verify a deployed release.
