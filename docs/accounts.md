# Signing in and who sees what

Remind signs every Carbon and Silicon in with [Silicon Accounts](https://accounts.teamofsilicons.com).
There is no Remind password and nothing to register: your Silicon Accounts account is your Remind
account. Remind's app id there is `remind`.

```sh
remind accounts --json        # Remind's app id and the Silicon Accounts it uses; no sign-in needed
```

## Sign in

### Carbons: approve a code

```sh
remind login
```

```text
To sign in to Remind, open https://accounts.teamofsilicons.com/device and enter the code

    WDJB-MJHT

(Direct link: https://accounts.teamofsilicons.com/device?code=WDJB-MJHT)
Waiting for approval; the code expires in 10 minutes (Ctrl-C to cancel).
```

Open the page on any device where you are signed in to Silicon Accounts (your phone works),
check that the code matches, and approve it. `remind` polls as often as Silicon Accounts allows,
slows down when asked, and finishes the moment you approve. Denying the request, or letting the
code expire after 10 minutes, ends the attempt with exit status 3. Nothing opens by itself; add
`--open` to open the page in your default browser, and `--label 'build laptop'` to name this
device in your list of sign-ins.

With `--json`, progress goes to stderr as JSON lines (`{"event":"device_code","user_code":…,
"verification_uri":…}`, then `{"event":"slow_down","interval":10}` when asked to slow down) and
the result to stdout as one object.

Running `remind login` again while signed in says so and changes nothing; `remind login --force`
signs in again.

### Silicons: hand over a short-lived token

A Silicon never sees a page. It asks Silicon Accounts for a short-lived token for Remind and
gives it to `remind`:

```sh
silicon-accounts login --app remind -q | remind login --slt-stdin
```

The token (`slt_…`) works once, for two minutes, and only for Remind. `remind` exchanges it with
Silicon Accounts as Remind's own public client (it holds no secret) and never prints or stores
it. These forms do the same:

```sh
remind login --slt "$SLT"     # visible in the process list; prefer --slt-stdin
remind login "$SLT"           # the token as the only argument
```

A token sign-in always replaces the sign-in saved in that home and ends the previous one, so a
Silicon that signs in on every start never piles up sign-ins.

When Silicon Accounts refuses the token, `remind` says exactly why, and every refused token is
used up:

| code | why | what to do |
|---|---|---|
| `slt_already_used` | it was exchanged before | mint a fresh one and use it right away |
| `slt_expired` | more than two minutes passed | mint a fresh one and use it at once |
| `slt_wrong_app` | it was minted for another app (named in the message) | mint one with `--app remind` |
| `slt_unknown` | mistyped, cut short, or from another Silicon Accounts | pass it whole; check `ACCOUNTS_URL` |
| `not_a_short_lived_token` | it does not start with `slt_`; nothing was sent | mint one with `silicon-accounts login --app remind -q` |
| `sign_in_refused` | anything else, for example the Silicon's STK was rotated after the token was minted | read the message; sign in to Silicon Accounts again |

### What `remind` keeps, and where

The sign-in lives in `{home}/.remind/state.json`, where `{home}` is `$SILICON_HOME` when set,
otherwise `~` (`remind config home <directory>` moves it). The directory is readable only by
you (0700, files 0600) and the file is replaced atomically.

One sign-in is kept per Remind origin, plus one per test environment if you signed in with
`--test` (otherwise a test environment uses the production sign-in of the same origin). Give
every Silicon its own `SILICON_HOME` so their sign-ins never replace each other.

The access token lasts 30 minutes. `remind` refreshes it when less than a minute is left, under
an exclusive lock on `state.lock`, so two `remind` processes sharing a home never present the
same refresh token. That matters: refresh tokens rotate on every use, and presenting a used one
ends the whole sign-in. A sign-in lasts up to 900 days.

### Check and end the sign-in

```sh
remind login status --json
```

```json
{"authenticated":true,"uuid":"8HV","id":"si:scout","kind":"silicon","display_name":"Scout",
 "expires_at":"2026-10-10T08:30:00Z","refresh_expires_at":"2029-03-28T08:00:00Z","verified":true,
 "url":"https://backend.remind.teamofsilicons.com","accounts_url":"https://accounts.teamofsilicons.com",
 "app_id":"remind","method":"slt","custodian":{"uuid":"zQo","id":"c:saket"},
 "can_manage_reminders":true,"visible_silicons":3}
```

With `--json` it always exits 0: `{"authenticated":false}` when nobody is signed in, plus a
`reason` and `message` when a saved sign-in no longer works (`sign_in_ended`,
`account_deleted`, `sign_in_again` for a sign-in saved by Remind 0.5 or earlier). `verified` is
true when Remind accepted the token just now; when Remind or Silicon Accounts cannot be reached,
the saved sign-in is reported with `verified: false` and a `warning`. `--offline` reads only the
file. Without `--json`, the command exits 1 when nobody is signed in.

```sh
remind whoami     # the account as Remind sees it: custodian, what you may do, Silicons you see
remind logout     # ends this machine's sign-in at Silicon Accounts and forgets it here
```

`remind logout` ends only this machine's sign-in; your other devices stay signed in. When
Silicon Accounts cannot be reached, the sign-in is still forgotten here, and you can end it from
your list of sign-ins on the account site.

### When a sign-in stops working

Commands that need a sign-in exit 3 with one of these codes:

| code | meaning | what to do |
|---|---|---|
| `not_signed_in` | no sign-in for this Remind origin (or the saved one is from Remind 0.5 or earlier) | sign in |
| `sign_in_ended` | the sign-in was ended elsewhere (signed out on the account site, the Silicon's STK rotated, Remind's access removed, a used refresh token presented); it was removed from this machine | sign in again |
| `token_revoked` | Remind saw the sign-in end before Silicon Accounts told this machine | sign in again |
| `account_deleted` | the account was deleted | nothing; the account is gone |

## Who sees what

Every reminder belongs to the Silicon that created it. Accounts are known by their permanent
Silicon Accounts uuid and shown by their current `c:`/`si:` id, which can change.

| who | sees | may change |
|---|---|---|
| the Silicon | its own reminders, and those of its custodian's other Silicons | only its own reminders and webhook subscriptions |
| its custodian (the Carbon who looks after it) | the reminders of every Silicon it looks after | nothing in the reminders; it manages who they are shared with, and sees (read-only) their webhook subscriptions |
| an account it shared with | that Silicon's reminders | nothing |
| anyone else | nothing | nothing |

Carbons read; they never create, change, pause or archive reminders, and a custodian never acts
as its Silicon. `remind silicons` lists the Silicons whose reminders you can read and why:
`self`, `custodian` (you look after it), `sibling` (another Silicon of your custodian) or
`shared`.

### Share with someone else

```sh
remind share add c:ada                          # as the Silicon: c:ada may read my reminders
remind share add si:ledger --silicon si:scout   # as si:scout's custodian
remind share list
remind share remove c:ada
```

Any Carbon can be granted. Silicons are not open to everyone: a Silicon outside your custodian's
Silicons must first allow you, because a Silicon acts on what it receives.

```sh
remind allow add si:scout                        # as si:ledger: si:scout may share with me
remind allow add si:scout --silicon si:ledger    # the same, as si:ledger's custodian
remind allow list
remind allow remove si:scout                     # also ends the grants it made possible
```

Sharing and allow-lists are about real accounts, so they are refused inside a test environment.

### Test environments

A test environment belongs to the account that created it. You see the ones you own, those of
the Silicons you look after, and those of your custodian and its other Silicons; you can read
their keys. Only the owner, or the owner's custodian when the owner is a Silicon, rotates,
retires or restores one. Anyone with the key can use and clean it. See
[testing environments](testing-environments.md).

## For other apps: reading for an account

Another app (the Silicon Interface, for example) reads an account's reminders with a Silicon
Accounts User verification proof instead of an access token:

```http
GET /api/v2/schedules
Authorization: Proof sap_…
X-Remind-API-Version: 2
```

The proof must be valid, issued for receiving app `remind`, carry the scope
`remind.schedules.read`, and come from an app this Remind deployment trusts for that scope. It
works only on `GET /schedules`, `GET /schedules/{id}`, `GET /schedules/{id}/executions`,
`GET /silicons` and `GET /auth/me`, and reads exactly what that account may read. A proof never
writes. In Rust, attach one with `Client::with_proof`. Remind calls no other app, so it issues no
proofs.

## For the API: access tokens

Every other request carries `Authorization: Bearer <access token>`: a Silicon Accounts access
token issued to Remind (`aud` = `remind`). Remind checks it against Silicon Accounts' public
keys, and additionally asks Silicon Accounts whether it is still active before anything that
cannot be undone or that reveals a secret (archiving, webhook subscriptions, sharing, test
environment keys). A token for another app is refused with `token_wrong_audience`; tokens from
Remind 0.5 and earlier get `legacy_token_rejected`. See the [API reference](api/README.md).

## When an account changes

Remind follows Silicon Accounts as accounts change:

- A new id or display name appears on the next request.
- When a Silicon gets a new custodian, the new custodian sees its reminders and the old one stops.
- Signing out on the account site, or a Silicon's STK being rotated, ends Remind access tokens
  issued before it; reminders keep firing.
- When an account removes Remind from its apps, its reminders stop firing until it signs in again.
- When an account is deleted, its reminders are archived (kept 45 days, then recorded in the
  deleted-reminders log), its webhook subscriptions end and its test environments are retired.
