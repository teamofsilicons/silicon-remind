# silicon-remind-client

Stateless Rust client for Silicon Remind, the durable reminder service for Silicons. Requires
Rust 1.98 or newer.

```toml
[dependencies]
silicon-remind-client = "0.6"
```

- `accounts::SignIn` signs a Carbon (device flow) or a Silicon (short-lived token from
  `silicon-accounts login --app remind -q`) in to Remind with Silicon Accounts, as Remind's
  public client: no secret. It also refreshes (rotating refresh tokens) and signs out.
- `Client` calls the Remind API (contract 2, `/api/v2`) with the access token, or with a User
  verification proof when another app reads on an account's behalf. It covers every public
  operation: reminders, delivery history, sharing and allow-lists, webhook subscriptions, test
  environments, bug reports.

Errors are typed and keep Remind's `{"error":{"code","message","hint"}}`; refused sign-ins say
exactly why (`SltAlreadyUsed`, `SltExpired`, `SltWrongApp`, `SignInEnded`, `DeviceDenied`…).
Secrets redact themselves in `Debug` output. Nothing is stored or refreshed behind your back:
where tokens live is your decision (the `remind` CLI, `silicon-remind-cli`, keeps them in a
locked, private state file).

Accounts are keyed by their Silicon Accounts `uuid` (short, case-sensitive text) and shown by
their `c:`/`si:` id. Creating a reminder requires an explicit IANA timezone in
`CreateScheduleRequest.timezone`, such as `Asia/Kolkata` or `UTC`.

The package bundles the client, CLI, sign-in, API, webhook and testing guides in `docs/`. Start
with the [client guide](https://docs.remind.teamofsilicons.com/client/).

Licensed under Apache-2.0. The Remind service source is licensed separately.
