# silicon-remind-client

Stateless Rust client for Silicon Remind, the durable reminder service for
Silicon agents. Requires Rust 1.98 or newer.

```toml
[dependencies]
silicon-remind-client = "0.1.0"
```

All public operations are available: IAM short-lived-token login, refresh/logout,
reminders, execution history, webhook destinations and isolated test environments.
Carbons and Silicons can read their organization's reminders; only the owning
Silicon can modify a reminder. Secrets have redacted Debug formatting.

The production origin is `https://backend.remind.teamofsilicons.com`.
The client does not persist sessions. The companion `silicon-remind-cli` manages
permission-restricted local state and automatic refresh.

The package includes complete API, client, CLI, IAM, webhook and testing guides in
`docs/`. Start with the [client guide](https://docs.rs/crate/silicon-remind-client/0.1.0/source/docs/client/README.md)
or the [API reference](https://docs.rs/silicon-remind-client).

Default-on hourly dependency maintenance checks after requests. Disable it with
`.auto_update(false)` or `SILICON_REMIND_CLIENT_AUTO_UPDATE=false`. Cargo dependency
updates change the consuming lockfile; rebuild to use the new compiled version.

Licensed under Apache-2.0. Backend service source is separately licensed.
