# remind CLI

`remind` is built entirely on `silicon-remind-client`. It stores preferences,
application sessions, refresh tokens, and test keys under `~/.remind/`. On Unix,
the directory is mode 0700 and state files are mode 0600. A process lock serializes
state mutations and refreshes; saves use an atomic rename. State is separated by
server origin and test-environment UUID so switching servers or sandboxes never
reuses another context's session.

## Build and start

```sh
cargo build -p silicon-remind-cli
cargo run -p silicon-remind-cli -- --help
cargo install --path crates/cli --locked
```

The default origin is `https://backend.remind.teamofsilicons.com`. For local work:

```sh
remind config set-url http://127.0.0.1:8086
remind --no-update health --ready
remind auth login --org tos
```

Login securely prompts for the short-lived token supplied by IAM. It does not
start an OTP ceremony or redirect a browser. For an agent/noninteractive shell:

```sh
remind auth login --org tos --slt-stdin < /secure/path/slt.txt
remind auth whoami
```

The SLT must be for `tos>remind` and bound to the desired organization. A successful
login verifies the organization before saving the new session. Near expiry, a
normal authenticated command rotates the saved refresh token before proceeding.
`auth refresh` requests an explicit rotation; `auth logout` revokes the IAM family
and then removes local credentials.

## Ordinary reminder workflow

```sh
remind webhook set 'https://hook-service/api/v1/your-issued-endpoint'
remind create --text 'Review the build' --cron '*/15 * * * *'
remind list
remind get <reminder-id>
remind edit <reminder-id> --text 'Review the release build'
remind pause <reminder-id>
remind resume <reminder-id>
remind executions <reminder-id>
remind archive <reminder-id>
remind list --archived
```

Use the actual URL and signing credential issued by Silicon Hook. `webhook set`
prompts securely for the signing secret; use `--secret-stdin` to supply a protected
file through stdin. `webhook get` never reveals the secret.

For a one-time reminder, add `--kind one-time`. It fires at the first future cron
match and enters the archive automatically. For a local wall-clock schedule, use
`--timezone Asia/Kolkata` or another IANA identifier. UTC is the default.

`pause` and `resume` accept up to 100 UUIDs and are atomic. A Carbon cannot create
or mutate reminders; it can use `silicons`, `list`, `get`, and `executions` for any
Silicon in its organization. Archiving retains a reminder for 45 days.

## Command reference

| Command | Purpose / useful options |
| --- | --- |
| `auth login` | Secure SLT prompt; `--org`, `--slt-stdin` |
| `auth whoami` | Live IAM identity and permissions |
| `auth refresh` | Rotate current refresh token |
| `auth logout` | Revoke and forget this session |
| `create` | Required `--text`, `--cron`; `--kind`, `--timezone` |
| `list` | `--silicon`, `--archived`, `--status`, `--cursor`, `--limit` |
| `get <id>` | Full reminder details |
| `edit <id>` | At least one of `--text`, `--cron`, `--timezone`, `--kind` |
| `pause <id>…` | Atomic pause of 1–100 owned reminders |
| `resume <id>…` | Atomic resume of 1–100 owned reminders |
| `archive <id>` | Move an owned reminder to the archive |
| `executions <id>` | `--cursor`, `--limit`; inspect deliveries/failures |
| `silicons` | `--after <uuid>`, `--limit`; registered org Silicons |
| `webhook set <url>` | Secure secret prompt or `--secret-stdin` |
| `webhook get` | Read endpoint metadata |
| `webhook disable` | Disable the current Silicon's endpoint |
| `env create <name>` | `--description`, `--iam-key-file`, `--iam-app-secret-file` |
| `env list` | `--include-deleted`, `--after <uuid>`, `--limit` |
| `env get <id>` | Environment metadata and deadlines |
| `env key <id>` | Retrieve, print and locally save the active key |
| `env rotate <id>` | Replace the root key and save its successor |
| `env delete <id>` | Retire; recoverable for 30 days |
| `env restore <id>` | Restore with a new root key |
| `env import <id>` | Save a shared key; prompt or `--key-stdin` |
| `env forget <id>` | Remove this computer's key/session only |
| `test-info` | Selected sandbox metadata; requires `--test` |
| `clean` | Clear selected sandbox data; requires `--test` |
| `config show` | Preferences and counts; no saved secrets |
| `config set-url <origin>` | Change the saved service origin |
| `config auto-update on\|off` | Persist updater preference |
| `update --check` | Query the registry without installation |
| `update` | Explicitly install a newer published CLI |
| `health` | Liveness; `--ready` checks database readiness |

Every command accepts `-h`/`--help`. Missing required flags produce the relevant
usage. Global flags are `--url`, `--org`, `--test <id>`, `--json`, `--no-update`, and
`--idempotency-key`. `REMIND_URL` and `REMIND_ORG` supply URL/org defaults for an
invocation. They do not move existing sessions between contexts.

`--json` emits machine-readable JSON and suppresses success suggestions. Errors
go to stderr. Exit status is 0 for success, 2 for CLI/local input failures, 3 for
API authentication failure, 4 for API forbidden, and 1 for other failures. Use the
API's machine error code in the error text to distinguish state conflicts.

## Sandboxes

Manage environments with the production session, without `--test`:

```sh
remind env create release-qa --description 'Manual release verification' \
  --iam-key-file /secure/path/iam-test-key \
  --iam-app-secret-file /secure/path/iam-test-app-secret
```

The returned UUID is your selector; the root key is saved locally. Use ordinary
commands with the prefix:

```sh
remind --test <id> test-info
remind --test <id> auth login --org test-org
remind --test <id> webhook set <test-hook-endpoint>
remind --test <id> create --text 'Sandbox reminder' --cron '* * * * *'
remind --test <id> list
remind --test <id> clean
```

A teammate can share the root key: `remind env import <id> --key-stdin < key.txt`.
Import verifies that the key belongs to the requested UUID before saving it.
The environment key provides sandbox administration; ordinary reminder commands
still need an IAM test identity. Cleaning clears all Remind data and logs but
keeps the environment, root key and IAM binding. It does not clean IAM itself.

The sandbox supports at most 100 retained reminders. It is retired after 15 days
without successful user activity; scheduler polls do not keep it alive. Deleted
environments can be recovered for 30 days. See the [full guide](../testing-environments.md).

## Updating

Auto-update is on by default. After the command finishes, if at least one hour
has elapsed since the previous check, the CLI checks crates.io and installs a
newer `silicon-remind-cli` with Cargo. Attempts are persisted, including failures.
The current command finishes with its existing binary; the next invocation uses
the update. No daemon or idle timer runs. Cargo must be available for installation.
Automatic replacement applies to a Cargo-installed `bin/remind` executable and
uses that installation's root, including custom Cargo roots. A source build or
copied binary reports `available` instead of installing an unrelated executable;
install the release with `cargo install silicon-remind-cli --locked --version
<version>` or rebuild the source checkout yourself.

Use `config auto-update off` to persist an opt-out, or `--no-update` for one
invocation. `update --check` and `update` are explicit actions and work regardless
of the automatic preference. Unpublished packages or registry/Cargo failures are
reported as unavailable and do not change the ordinary command's result.

`env create` needs only a name and IAM root key; `--iam-app-secret-file` is
optional. To install the test app secret later, use
`remind --test <id> configure-iam --iam-app-secret-file /private/test-app-secret`.
Ordinary actions fail with an explicit configuration error until this is done.

Refresh retries persist their operation key before contacting IAM. After an
uncertain response, run the command again using this same local store; it safely
replays the pending refresh. Do not copy a rotating session family between machines.

Runtime `--json` failures return `error.code` and `error.message`. Backend errors
also retain HTTP `status`, `request_id` and optional `retry_after`; local argument
validation uses `invalid_input`. Clap usage/help errors retain its standard CLI
help format. Credentials and response bodies are not included in errors.
