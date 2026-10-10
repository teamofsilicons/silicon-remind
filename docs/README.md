# Start using Silicon Remind

Schedule reminders for your Silicon, see what was delivered, and connect a webhook receiver when you want the
reminders posted somewhere. A Silicon sets and changes its own reminders; the Carbon who looks after it, and that
Carbon's other Silicons, can read them.

## Install

A Silicon installs the CLI from Silicon Apps, on Linux, macOS or Windows:

```sh
silicon-apps install remind
```

Silicon Apps picks the build for your system and keeps it up to date on its own; `remind` never updates itself.
Development releases install as `silicon-apps install 'remind>dev'`. Don't have `silicon-apps` yet? See
[installing Silicon Apps](https://developers.teamofsilicons.com/docs/apps/start/install). The CLI keeps its state in
`$SILICON_HOME/.remind` when `SILICON_HOME` is set, otherwise `~/.remind`. See the [CLI guide](cli/README.md) for
every command and the [release guide](releases.md) for how releases are built.

## Sign in and schedule

```sh
silicon-accounts login --app remind -q | remind login --slt-stdin
remind login status --json
remind create --text 'Review the build' --cron '0 9 * * MON-FRI' --timezone Asia/Kolkata
remind list --json
```

The first line asks Silicon Accounts for a short-lived token for Remind and hands it to `remind`; Remind never asks
for a password. A one-time reminder uses the same five-field cron syntax with `--kind one-time`. Subscribe a
receiver with `remind webhook subscribe <url>`; reminders do not need a subscription.

For a Carbon: open the [website](https://remind.teamofsilicons.com) and sign in with Silicon Accounts, or run
`remind login`, which shows a code to approve on the account site. You see the reminders of the Silicons you look
after, and of anyone who shared theirs with you; you read them, your Silicons write them. Ask your Silicon to install
Remind, sign in, and create the reminder. [Signing in and who sees what →](accounts.md)

## Try a test environment

```sh
remind env create 'release check'
remind --test <environment-id> create --text 'Test ping' --cron '*/5 * * * *' --timezone UTC
remind --test <environment-id> list --json
```

A test environment is an empty copy of Remind with its own reminders, deliveries and logs, at most 100 reminders.
You stay signed in as yourself; its 32-character key opens it to anyone you give it to.
[Testing guide →](testing-environments.md)

## Use and build on Remind

- [CLI commands, configuration and offline manuals](cli/README.md)
- [The website](browser.md)
- [Signing in, sharing and who sees what](accounts.md)
- [Build an integration with the Rust client](client/README.md)
- [Call the HTTP API](api/README.md) and download [OpenAPI](../openapi.yaml)
- [Verify webhook deliveries](webhook-delivery.md)
- [Test environments](testing-environments.md)
- [Version policy and compatibility](version-policy.md)
- [Bug reports and telemetry settings](diagnostics.md)
- [Operator deployment guide](deployment.md) and the [service-only routes](internal-api.md)

Run `remind <command> --help` to explore the command tree, or `remind docs <topic>` for the complete offline
manuals. Report a reproducible bug with `remind report 'steps, expected result, actual result' --pr <optional-fix-URL>`
while signed in. Reports are queued for Postmark delivery; reports from a test environment are simulated.
[Source repository](https://github.com/teamofsilicons/silicon-remind).
