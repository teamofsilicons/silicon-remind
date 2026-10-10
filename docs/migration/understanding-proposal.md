# Remind: proposed changes to UNDERSTANDING.md

`UNDERSTANDING.md` is changed only by Carbons, so the migration did not touch it. Below is wording a Carbon can paste.
Each heading names the part of the current file it replaces. The service stage wrote the backend parts; later stages
(CLI, web) may add theirs.

## Glossary (replace the three entries)

`Carbon` - A person's account at Silicon Accounts.
`Silicon` - A Silicon's account at Silicon Accounts. Every Silicon has a custodian.
`Custodian` - The Carbon who looks after a Silicon.

## How login works (replace the section)

Signing in and signing up are handled entirely by Silicon Accounts. Remind is the app `remind` there; its app secret
stays on the server. Carbons sign in on the Silicon Accounts pages and Silicons with a short-lived token from their own
sign-in. Every request to Remind carries the access token Silicon Accounts issued to Remind, and Remind checks it
itself. Use the official and latest `silicon-accounts-client` crate everywhere.

Remind's webhook (backend.remind.teamofsilicons.com/webhook/) hears from Silicon Accounts whenever an account Remind
knows changes its id, name, photo or custodian, signs out of Remind, removes Remind's access, or is deleted. A sign-out
ends that account's older tokens. Removing access pauses the account's reminders until it signs in again. Deleting the
account archives its reminders, ends its webhook subscriptions and retires its test environments.

Another app may read reminders for an account with a User verification proof from Silicon Accounts that carries the
scope `remind.schedules.read`, but only an app Remind trusts for that scope (today the Silicon Interface). Such an app
reads exactly what that account could read, and never changes anything.

## Who sees what (replace "any carbon in the organisation should be able to view reminders of any silicon in their organisation" and "Any silicon should also be able to list their reminders and other silicons reminders in their org")

The request for setting a reminder always comes from a Silicon, and the reminder belongs to that Silicon. A Carbon
sees the Silicons it looks after and all their reminders, and only views them. A Silicon lists its own reminders and
those of the other Silicons with the same custodian.

A Silicon, or its custodian, can share the Silicon's reminders with any other account by its id (`c:…` or `si:…`), and
end that sharing at any time. Shared reminders are read-only. Silicons are not open to everyone: before a reminder can
be shared with a Silicon from outside its custodian's Silicons, that Silicon or its custodian must allow the sharing
account. Anyone can share with a Carbon.

For carbons that log in, "all the silicons they have access to" means the Silicons they look after and the Silicons
that shared their reminders with them.

## Testing (replace the paragraphs about IAM test environments and the test key of IAM)

Remind has its own test environments, an exact replica of the main application that starts empty. Creating one needs
only a name and an optional description. Inside a test environment you sign in with your normal Silicon Accounts
account; only the reminders, deliveries and logs belong to the test environment.

## Creating Test Env (replace the paragraph)

Any Carbon or Silicon can create a test environment; it belongs to the account that created it. It returns the
32-character alphanumeric key that opens it; anyone with the key has the god view of that test environment. The key is
stored with the environment and can be retrieved any time by the creator, its custodian and the custodian's other
Silicons (for a Carbon creator: the Carbon and the Silicons it looks after).

## Rotate Key and Delete Test Env (replace "org_admin/org_head" and "org admins, owners")

The creator, or the creator's custodian when the creator is a Silicon, can rotate the key, delete the test environment
and recover it within 30 days.

## Logging in via cli (replace the organization resolution list and the paragraph after it)

`remind login <slt>` signs one account in, with nothing else supplied; this is how `silicon connect` signs a Silicon
in. A login is always exactly one account, so there is nothing to choose and no question to ask.

## Identifier schema (replace the last sentence of the first paragraph)

Every account also has a permanent 128-bit (16-byte) UUID from Silicon Accounts,
serialized as 36 lowercase characters with hyphens, for example
`d7ce239a-7b3e-4e0b-9236-b936405c1fda`. New accounts use UUIDv4. Remind keys everything
on that immutable UUID; public `c:`/`si:` ids can change. Existing short identities
move once through the coordinated migration. Delete "Organisation membership and
application ownership are stored separately under `org_id`."

## Docs (replace "IAM integration")

The API, Rust-client, CLI, Silicon Accounts sign-in and testing-environment guides are maintained in [docs/].

## Rust Package & CLI (replace "carbons, silicons, org, access keys, api keys" in the first paragraph)

Everyone should be able to use the CLI and the Rust package: Carbons and Silicons, for every client action (read,
write, patch, delete, test environments, sharing).

## Logging in via cli (replace the first paragraph of the section, written by the CLI stage)

Remind never asks for a password and never sends anyone to a page they did not ask for. A Silicon signs in with a
short-lived token it gets from Silicon Accounts for Remind (`silicon-accounts login --app remind -q`) and hands to
`remind login`; the token works once, for two minutes, and only for Remind. A Carbon runs `remind login`, which prints
a short code and a link; the Carbon approves the code on the Silicon Accounts site from any device, and the CLI
finishes on its own. The CLI keeps the sign-in in `{home_dir}/.remind/`, renews it by itself, and `remind logout`
ends it.

## Auto updater (replace the paragraph "For both cli and client we would also package in an auto updater …")

Silicon Apps installs the CLI (`silicon-apps install remind`) and keeps it up to date on its own; neither the CLI nor
the Rust package updates itself. The Rust package is updated like any other dependency.

## The specific commands (replace item 2 and adjust item 3 of "It should also expose these specific commands")

2) `accounts --json`: `remind accounts --json` returns `app_id` (`remind`) alongside the Silicon Accounts address and
the CLI version, signed out and offline.
3) `login status --json`: reports `authenticated: true` with which Carbon or Silicon it is (its uuid, id and kind)
when signed in, and `{"authenticated":false}` when not; with `--json` it always succeeds so a script can read the
answer.

## How it works (replace "For each silicon that is registered into the system they need to have logged in via IAM.", written by the packaging stage)

Every Silicon that sets reminders signs in to Remind with Silicon Accounts.

## Testing (delete the two links to IAM's testing guide, the sentence "So remind testing wouldn't support remind testing on the prod IAm, it would only support it in the testing enviorment of IAm.", and the line "Read [...] to understand how exactly are webhooks gonna work for this, etc.")

Reminders in a test environment fire like real ones, but their webhook deliveries never reach a production receiver:
they are simulated, unless the receiver is one of the test receivers the operator of Remind allows.

## Using a Test Enviorment (replace the first paragraph)

For using a test environment anyone with the key has the god view of that test environment. They sign in to Remind
with their usual Silicon Accounts account, and everything they do happens inside the test environment, exactly as it
would in the main Remind, so it is a sandboxed environment to test it all out.

## Releases (add at the end of "Rust Package & CLI")

The CLI is published on Silicon Apps as the app `remind`, one package for each system it supports (Linux, macOS and
Windows, on Intel and ARM). Silicons install it with `silicon-apps install remind`, and Silicon Apps keeps it up to
date. Every package answers `remind --help`, `remind accounts --json` and `remind login status --json` without anyone
signed in; Silicon Apps checks this before it accepts a package. Linux packages run on any Linux with glibc 2.28 or
newer.
