# silicon-remind-cli

`remind` is the command line for Silicon Remind: durable one-time and recurring reminders for
Silicons, on five-field cron, in any IANA timezone, delivered to the Silicon's webhook
subscriptions. It signs in with Silicon Accounts. Requires Rust 1.98 or newer to build.

```sh
silicon-apps install remind                                        # Silicon Apps keeps it up to date
remind login                                                       # Carbons: approve a code
silicon-accounts login --app remind -q | remind login --slt-stdin  # Silicons: a short-lived token
remind webhook subscribe https://hook.example/remind --unsigned    # optional delivery target
remind create --text 'Daily check-in' --cron '0 9 * * *' --timezone Asia/Kolkata
remind list
```

A Silicon creates and changes its own reminders. Its custodian (the Carbon who looks after it)
and the custodian's other Silicons can read them, and so can any account it shares them with
(`remind share add c:ada`). Carbons read; they never write reminders. Every `remind create`
needs `--timezone` with an IANA identifier; there is no default.

The discovery commands work signed out, offline and in an empty home:
`remind --help`, `remind accounts --json` and `remind login status --json`
(`{"authenticated":false}` when nobody is signed in; it always exits 0 with `--json`).

Sign-ins, settings and test environment keys live in `{home}/.remind/` (`{home}` is
`$SILICON_HOME` when set, otherwise `~`), readable only by you. Add `--test <id>` to any command
to run it inside a Remind test environment.

The complete CLI, sign-in, API, client, webhook and testing guides are bundled: run
`remind docs <topic>`, or read them at https://docs.remind.teamofsilicons.com. Every command has
`--help` with examples.

Licensed under Apache-2.0. The Remind service source is licensed separately.
