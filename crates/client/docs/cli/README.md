# remind CLI

`remind` is the command line for Silicon Remind, built only on the public
[`silicon-remind-client`](../client/README.md) crate. It signs you in with Silicon Accounts, keeps
that sign-in fresh, and gives every Remind action a command. Every command has `--help` with its
flags and examples, and `remind --help` shows the whole tree.

## Install

Silicon Apps installs Remind and keeps it up to date; `remind` never updates itself.

```sh
silicon-apps install remind          # production
silicon-apps install 'remind>dev'    # development releases
remind --help
```

From source (Rust 1.98 or newer): `cargo install silicon-remind-cli`, or
`cargo run -p silicon-remind-cli -- --help` in a checkout.

## Sign in

```sh
remind login                                                       # Carbon: approve a code
silicon-accounts login --app remind -q | remind login --slt-stdin  # Silicon: a short-lived token
remind login status --json
remind logout
```

A Carbon's `remind login` prints a code and a link; approve the code on the account site from any
device and `remind` finishes on its own (the code expires after 10 minutes). A Silicon never sees
a page: it mints a short-lived token for Remind (single use, two minutes) and hands it over.
`remind login <slt>`, with the token as the only argument, does the same as `--slt-stdin`. Every
refusal says why and what to do. [Signing in and who sees what](../accounts.md) covers sign-in in
depth: refusal codes, refresh, `login status` fields.

```sh
remind accounts --json      # no sign-in, no network
```

```json
{"app_id":"remind","client_id":"remind","accounts_url":"https://accounts.teamofsilicons.com",
 "api_url":"https://api.remind.teamofsilicons.com","version":"0.6.0","api_version":2,
 "sign_in":{"carbon":"remind login","silicon":"silicon-accounts login --app remind -q | remind login --slt-stdin",
 "status":"remind login status --json"},"docs_url":"https://docs.remind.teamofsilicons.com"}
```

`remind login status --json` always exits 0 and prints `{"authenticated":false}` when nobody is
signed in; signed in, it prints `uuid`, `id`, `kind`, `display_name`, `expires_at`,
`refresh_expires_at` and `verified` (Remind accepted the token just now), among others.

## Where state lives

`remind` keeps its settings, sign-ins and test environment keys in `{home}/.remind/`:
`{home}` is `$SILICON_HOME` when set (an empty value is an error), otherwise `~`.

```sh
SILICON_HOME=/srv/silicons/scout remind login status --json
remind config home /srv/remind-state     # an existing directory; later commands use it
remind config show
```

`config home` writes a pointer in the default home's `.remind/home`; it does not move existing
sign-ins or keys. On Unix the directory is mode 0700 and its files 0600. `state.json` is
replaced atomically, and an exclusive lock on `state.lock` is held only while state changes or
a sign-in is refreshed, never while waiting for a Carbon to approve a code. Reading never
creates anything: `--help`, `accounts` and `login status` leave a clean home untouched.

One sign-in is kept per Remind origin, and one per test environment when you sign in with
`--test`. The access token is refreshed when less than a minute is left, single-flight across
processes, and a request Remind refuses with 401 is retried once after a refresh. A state file
from Remind 0.5 or earlier is read without its sign-ins (sign in again) and archived as
`state.legacy-<time>.json` on the next change; an unreadable `state.json` is moved aside as
`state.corrupt-<time>.json`.

## Reminders

A Silicon creates and changes its own reminders:

```sh
remind webhook subscribe https://hook.example/remind --secret-stdin < secret.txt
remind create --text 'Review the build' --cron '*/15 * * * *' --timezone Asia/Kolkata
remind create --text 'Release call' --cron '30 16 10 10 *' --kind one-time --timezone UTC
remind list
remind get <reminder_id>
remind edit <reminder_id> --text 'Review the release build'
remind pause <reminder_id> <reminder_id>
remind resume <reminder_id> <reminder_id>
remind executions <reminder_id>
remind archive <reminder_id>
remind list --archived
```

- Cron has five fields: minute, hour, day of month, month, day of week.
- `--timezone` is mandatory on `create` and takes an IANA identifier (`Asia/Kolkata`, `UTC`);
  a missing, blank or unknown timezone is refused with what to type instead. `edit` keeps the
  stored timezone unless you pass one.
- A one-time reminder fires at the first future match, then moves to the archive.
- `pause` and `resume` take 1 to 100 ids and apply all or nothing.
- Archived and fired one-time reminders stay readable for 45 days (`list --archived`), then go
  to the deleted-reminders log.
- Reminders work without any webhook subscription; they just are not delivered anywhere. See
  [webhook delivery](../webhook-delivery.md).

Carbons read. A custodian sees the reminders of every Silicon it looks after:

```sh
remind silicons                       # the Silicons you can read, with relation and count
remind list --silicon si:scout
remind executions <reminder_id>
remind webhook list --silicon si:scout   # read-only
```

`list --silicon` takes a `si:` id or uuid. Pass `next_cursor` from one page to `--cursor` (or
`--after` for `silicons` and `env list`) for the next.

## Sharing

```sh
remind share add c:ada                          # as the Silicon
remind share add si:ledger --silicon si:scout   # as si:scout's custodian
remind share list
remind share remove c:ada
remind allow add si:scout                       # as si:ledger: let si:scout share with me
remind allow list
remind allow remove si:scout
```

A Silicon's reminders are visible to it, its custodian and its custodian's other Silicons.
Anyone else needs a share. Any Carbon can be granted; a Silicon outside your custodian's
Silicons must first allow you with `remind allow add`. Removing an allow-list entry also ends
the shares it made possible. See [who sees what](../accounts.md#who-sees-what).

## Webhooks

```sh
remind webhook subscribe https://hook.example/remind     # asks for an optional signing secret
remind webhook subscribe https://hook.example/remind --secret-stdin < secret.txt
remind webhook subscribe https://hook.example/remind --unsigned
remind webhook list
remind webhook get
remind webhook unsubscribe <subscription_id>
remind webhook disable                                   # ends every subscription
```

A Silicon may have no subscription, one, or several; each due reminder is posted to every one.
The signing secret is never shown again. Without a terminal, `subscribe` needs `--secret-stdin`
or `--unsigned`.

## Test environments

A test environment is an isolated copy of Remind: create one, then add `--test <id>` to any
command. Inside it you are still the account you signed in as; only the data is separate.

```sh
remind env create release-qa --description 'Manual release checks'   # its key is saved here
remind --test <test_id> create --text 'Try it' --cron '* * * * *' --timezone UTC
remind --test <test_id> list
remind --test <test_id> test-info
remind --test <test_id> clean            # erase its data; the environment and key stay
remind env use <test_id>                 # every later command runs there…
remind --production list                 # …except with --production
remind env exit                          # back to production
```

Manage environments from production (no `--test`): `env list`, `env get`, `env key` (prints the
key and saves it here), `env rotate`, `env delete` (recoverable for 30 days), `env restore`.
Someone who received a key saves it with `remind env import <test_id> --key-stdin < key.txt`
(checked before saving, no sign-in needed); `env forget` removes a key and that environment's
own sign-in from this machine only. A test environment holds at most 100 reminders and retires
after 15 days without activity. While one is selected, every command ends with a line on stderr
naming it. The [testing guide](../testing-environments.md) has the details.

## Settings

| setting | flag | environment | saved with |
|---|---|---|---|
| Remind API origin | `--url` | `REMIND_URL` | `remind config set-url <url>` |
| Silicon Accounts origin | `--accounts-url` | `ACCOUNTS_URL` | `remind config set-accounts-url <url>` |
| home | | `SILICON_HOME` | `remind config home <directory>` |
| telemetry | | `REMIND_TELEMETRY_ENABLED=false` | `remind config telemetry on\|off` |
| app id (development only) | | `REMIND_APP_ID` | |

The defaults are `https://api.remind.teamofsilicons.com` and
`https://accounts.teamofsilicons.com`. Origins must be https; plain http is accepted only for
this machine (`localhost`, `127.0.0.1`, `::1`), for local development:

```sh
remind config set-url http://127.0.0.1:4181
remind config set-accounts-url http://localhost:9590
remind health --ready
```

A sign-in keeps the Silicon Accounts origin that issued it: refresh and sign-out go there even
if you change `ACCOUNTS_URL` later.

## Output, errors and exit status

With `--json`, every result is one JSON object on stdout; progress and warnings go to stderr as
JSON lines, and suggestions are left out. Without it, results are pretty-printed and suggested
next steps follow on stderr.

Failures go to stderr. With `--json` they look like this; `hint`, `status`, `request_id` and
`retry_after` appear when known:

```json
{"error":{"code":"not_reminder_owner","message":"Only si:scout changes this reminder.","hint":"…","status":403,"request_id":"…"}}
```

| exit | meaning |
|---|---|
| 0 | success (and `login status --json`, signed in or not) |
| 1 | any other failure: network, conflicts, not found, server errors |
| 2 | invalid arguments or input, refused before anything was sent |
| 3 | not signed in, sign-in refused or ended, or HTTP 401 |
| 4 | signed in but not allowed (HTTP 403) |
| 130 | stopped with Ctrl-C while waiting for a code to be approved |

Reuse `--idempotency-key <key>` when retrying the exact same `create`, `edit`, `pause`,
`resume` or `report` after an uncertain failure, so it is applied once.

## Manuals and reports

```sh
remind docs                 # this guide; also: accounts, api, client, testing, webhooks, releases
remind report 'Steps, expected result, actual result' --pr https://github.com/teamofsilicons/silicon-remind/pull/123
remind report-status <report_id>
```

`report` emails the Remind team using your sign-in (`--pr` is optional); inside a test
environment it is simulated. Never include secrets.

## Telemetry

Operational telemetry is on by default: after a command that used your sign-in, `remind` sends
one `command_completed` event (success, duration) through Remind to Space Station. It never
includes arguments, credentials, reminder text or webhook URLs. `remind config telemetry off`
or `REMIND_TELEMETRY_ENABLED=false` turns it off, for API request observations too. Events
inside a test environment stay in that environment. See [diagnostics](../diagnostics.md).

## Command reference

| command | what it does |
|---|---|
| `accounts` | Remind's app id and Silicon Accounts origin; offline, no sign-in |
| `login` | Carbon device sign-in; `--open`, `--label`, `--force` |
| `login --slt-stdin`, `login --slt <t>`, `login <t>` | Silicon sign-in with a short-lived token |
| `login status` | who is signed in; `--offline`; with `--json` always exits 0 |
| `logout` | end this machine's sign-in |
| `whoami` | the account as Remind sees it |
| `create` | `--text`, `--cron`, `--timezone` (all required), `--kind recurring\|one-time` |
| `list` | `--silicon`, `--archived`, `--status active\|paused\|completed`, `--cursor`, `--limit` |
| `get <id>` | one reminder |
| `edit <id>` | at least one of `--text`, `--cron`, `--timezone`, `--kind` |
| `pause <id>…`, `resume <id>…` | 1 to 100 reminders, all or nothing |
| `archive <id>` | archive one of your reminders |
| `executions <id>` | delivery history; `--cursor`, `--limit` |
| `silicons` | the Silicons you can read; `--after`, `--limit` |
| `share add\|list\|remove` | share a Silicon's reminders; `--silicon` for custodians |
| `allow add\|list\|remove` | who may share with a Silicon; `--silicon` for custodians |
| `webhook subscribe\|list\|get\|unsubscribe\|disable` | delivery subscriptions |
| `env create\|list\|get\|key\|rotate\|delete\|restore` | manage test environments (from production) |
| `env import\|forget\|use\|exit` | keys and selection on this machine |
| `test-info`, `clean` | inside a test environment only |
| `docs [topic]` | offline manuals |
| `report <message>`, `report-status <id>` | bug reports |
| `config show\|set-url\|set-accounts-url\|home\|telemetry` | local settings |
| `health` | API liveness; `--ready` checks its database |

Global flags: `--url`, `--accounts-url`, `--test <id>`, `--production`, `--json`,
`--idempotency-key`, `-h`/`--help`, `-V`/`--version`.
