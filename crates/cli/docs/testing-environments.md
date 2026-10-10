# Test a complete reminder workflow

A Remind test environment is an isolated copy of Remind with its own reminders, deliveries,
subscriptions and logs. It starts empty, behaves like production, and never touches production
data. You use it with your normal Silicon Accounts sign-in; only the data is separate.

## Create one and use it

```sh
remind env create release-qa --description 'Manual release checks'
remind --test <test_id> create --text 'Sandbox check' --cron '*/5 * * * *' --timezone Asia/Kolkata
remind --test <test_id> list --json
remind --test <test_id> executions <reminder_id>
remind --test <test_id> clean
```

`env create` runs in production (without `--test`), returns the environment and saves its
32-character key on this machine. Add `--test <test_id>` to any ordinary command to run it in
the environment; commands that only make sense there (`test-info`, `clean`) refuse to run
without it: "This action is only possible inside a test environment."

To stay in an environment for a while:

```sh
remind env use <test_id>       # every later command runs there; stderr names it after each one
remind --production list       # one command in production meanwhile
remind env exit                # back to production
```

## Who can do what

| who | can |
|---|---|
| anyone with the key | use the environment and clean it |
| the owner (the account that created it) | everything: read the key, rotate it, retire and restore the environment |
| the owner's custodian, when the owner is a Silicon | the same as the owner |
| the owner's custodian and its other Silicons, or the Silicons a Carbon owner looks after | see it in `remind env list` and read its key |

Inside an environment you are the account you signed in as. Everyone there reads every reminder
of the environment, and writes still follow ownership: only the Silicon that created a reminder
changes it, and Carbons only read.

Sharing (`remind share`, `remind allow`) is about real accounts, so it is refused inside a test
environment.

## Share it

```sh
remind env key <test_id>                                 # prints the key and saves it here
remind env import <test_id> --key-stdin < key.txt        # on the other machine; no sign-in needed
```

`env import` checks that the key belongs to that environment before saving it. Treat a key like
a password: anyone holding it can use and clean the environment. `remind env rotate <test_id>`
replaces it; the old key stops working at once. `remind env forget <test_id>` removes the key
(and the environment's own sign-in, if you made one with `remind --test <test_id> login`) from
this machine only.

## Limits and lifecycle

- At most 100 reminders at a time. This is a test environment limitation only.
- An environment with no activity for 15 days is retired automatically. Scheduler polls do not
  count as activity.
- A retired environment (automatically or with `remind env delete <test_id>`) can be restored
  for 30 days with `remind env restore <test_id>`, which issues a new key. After that it is gone.
- `remind env list --include-deleted` shows retired environments that can still be restored.

## Deliveries

Outbound deliveries are simulated inside test environments by default: the execution completes
and is recorded without contacting the subscribed URL. A Remind deployment that should deliver
for real lists exact receiver URLs in `REMIND_TEST_WEBHOOK_URLS`; only those receive requests.
Use receivers made for testing, never production notification endpoints. Check
`remind --test <test_id> executions <reminder_id>` to see what happened, and the
[webhook guide](webhook-delivery.md) for the request format.

## HTTP API

Send the key in `X-Remind-Test-Key` together with your normal access token:

```sh
curl https://api.remind.teamofsilicons.com/api/v2/schedules \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "X-Remind-API-Version: 2" \
  -H "X-Remind-Test-Key: $REMIND_TEST_KEY"
```

`GET /api/v2/testing-environment` and `POST /api/v2/testing-environment/cleanings` work with the
key alone. Managing environments (`/api/v2/test-environments…`) is a production action: a request
there carrying the key is refused with `test_key_not_allowed_here`. Keep keys out of URLs and
shell history (a protected curl config file works well).

## Rust client

```rust,no_run
use silicon_remind_client::{Client, Secret, models};

async fn example(access_token: Secret, key: Secret) -> silicon_remind_client::Result<()> {
    let remind = Client::new("https://api.remind.teamofsilicons.com")?.with_session(access_token)?;
    let sandbox = remind.with_test_environment(key)?;
    let environment = sandbox.current_environment().await?;
    let page = sandbox.reminders(&models::ListSchedules::default()).await?;
    println!("{}: {} reminders", environment.name, page.items.len());
    Ok(())
}
```

`with_test_environment` keeps the access token: inside the environment you are the same account.

## Environments from before Silicon Accounts

Environments that the previous identity service's automation created are dormant: never listed,
never selectable, their data kept. Environments created with a Remind key by an account from
before Silicon Accounts become visible to that account once the operator links it to its Silicon
Accounts account. Creating an environment takes only a name and a description; the old key and
secret fields are refused with `test_key_field_retired`.
