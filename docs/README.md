# Start using Silicon Remind

Schedule reminders for your Silicon, inspect their delivery history, and connect a webhook receiver when you need notifications. Carbons and Silicons can read reminders throughout their organization; only a Silicon can change its own reminders.

## Install

On macOS or Linux, run:

```sh
curl -fsSL https://docs.remind.teamofsilicons.com/install.sh | sh
```

The installer sets up Rust when needed, builds the public CLI/client source bundle, installs `remind`, and starts its hourly update service. It does not authenticate you. It uses `$SILICON_HOME/.remind` for application state when `SILICON_HOME` is set; otherwise it uses `~/.remind`. See the [CLI guide](cli/README.md) for manual setup, configuration, and service management.

## Sign in and schedule

```sh
remind iam --json
remind login '<SLT-from-IAM>'
remind login status --json
remind create --text 'Review the build' --cron '0 9 * * MON-FRI' --timezone Asia/Kolkata
remind list --json
```

Generate the SLT with the official IAM CLI or web consent screen for the application shown by `remind iam`. Remind never asks for your IAM password or OTP. A one-time reminder uses the same five-field cron syntax with `--kind one-time`. Subscribe a receiver with `remind webhook subscribe <url>`; reminders do not require a subscription.

For a Carbon: sign in to inspect your organization's reminders, or ask your Silicon to install Remind, sign in with its own SLT, and create the schedule. Use the [website](https://remind.teamofsilicons.com) for the browser workflow.

## Try a sandbox

```sh
remind env use --secret-stdin < /private/remind-app-secret
remind login '<existing-test-public-id-or-test-SLT>'
remind list --json
remind env exit
```

The IAM application's `app_secret` selects its sandbox automatically. Test actions use that identity's actual permissions. [Testing guide →](testing-environments.md)

## Use and build on Remind

- [CLI commands, configuration, and offline manuals](cli/README.md)
- [Website sessions and settings](browser.md)
- [Build an integration with the Rust client](client/README.md)
- [Call the HTTP API](api/README.md) and download [OpenAPI](../openapi.yaml)
- [Verify webhook deliveries](webhook-delivery.md)
- [Understand IAM authorization](iam.md)
- [Version policy and compatibility matrix](version-policy.md)
- [Bug reports and telemetry settings](diagnostics.md)
- [Operator deployment guide](deployment.md)

Run `remind <command> --help` to explore the command tree, or `remind docs <topic>` for complete offline manuals. Report a reproducible bug with `remind report 'steps, expected result, actual result' --pr <optional-fix-URL>` using your Remind session. Reports are queued for Postmark delivery; sandbox reports are simulated. [Source repository](https://github.com/teamofsilicons/silicon-remind).
