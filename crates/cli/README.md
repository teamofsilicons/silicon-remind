# silicon-remind-cli

The `remind` command manages durable one-time and recurring reminders through
Silicon Remind. Requires Rust 1.98 or newer.

```sh
curl -fsSL https://docs.remind.teamofsilicons.com/install.sh | sh
remind -h
remind login <slt> --org tos
remind create --text 'Daily check-in' --cron '0 9 * * *' --timezone Asia/Kolkata
```

Login requests only an IAM short-lived token. Webhook subscriptions are optional;
add one with `remind webhook subscribe <url>` when outbound delivery is wanted.
Carbons and other Silicons can view reminders throughout their organization;
only the owning Silicon can mutate them.

The default origin is `https://backend.remind.teamofsilicons.com`. Preferences,
rotating sessions and sandbox keys are stored under `{home}/.remind/` with restrictive
permissions. Use `--test <id>` before ordinary commands for an isolated sandbox.

Complete API, CLI, client, IAM, webhook and testing guides are included in `docs/`.
Read the [CLI guide](https://docs.remind.teamofsilicons.com/cli/)
and run `remind <command> -h` for arguments and examples.

Default-on hourly updates run in the operating-system supervised daemon installed
by the setup script. Use `remind config auto-update off` to opt out. Cargo-installed copies update
in their existing installation root; source builds report availability.

Licensed under Apache-2.0. Backend service source is separately licensed.

Use `remind env use --secret-stdin`, `remind report`, `remind report-status`,
`remind docs`, and `remind config telemetry on|off` for sandbox discovery,
bug reporting, offline manuals, and diagnostic preferences. This source bundle
is version 0.2.0; use a registry version only after it has been published.
