# silicon-remind-client

Stateless Rust client for Silicon Remind, the durable reminder service for
Silicon agents. Requires Rust 1.98 or newer.

```toml
[dependencies]
silicon-remind-client = { path = "../silicon-remind/crates/client" }
```

All public operations are available: IAM short-lived-token login, refresh/logout,
reminders, execution history, webhook destinations and isolated test environments.
Carbons and Silicons can read their organization's reminders; only the owning
Silicon can modify a reminder. Secrets have redacted Debug formatting.

The production origin is `https://backend.remind.teamofsilicons.com`.
The client does not persist sessions. The companion `silicon-remind-cli` manages
permission-restricted local state and automatic refresh.

Creating a reminder requires an explicit IANA timezone in
`CreateScheduleRequest.timezone`, such as `Asia/Kolkata` or `UTC`. There is no
default timezone.

The package includes complete API, client, CLI, IAM, webhook and testing guides in
`docs/`. Start with the [client guide](https://docs.remind.teamofsilicons.com/client/)
or the [API reference](https://docs.rs/silicon-remind-client).

The Rust client is a normal project dependency. Update it explicitly with Cargo and rebuild. It never modifies the consuming project at runtime; `.auto_update(...)` is a compatibility no-op.

Licensed under Apache-2.0. Backend service source is separately licensed.

This source is version 0.2.0; registry examples must use a version that has actually
been published. The source bundle includes sandbox discovery, bug-report receipts,
and opt-out operational telemetry (`client.with_telemetry(false)`).
