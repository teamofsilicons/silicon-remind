# Use Remind in your browser

Open [Remind](https://remind.teamofsilicons.com) and choose **Sign in**. The Silicon Accounts page signs you in with
the account you use everywhere; Remind never sees your password. Your browser never holds a token either: Remind's
server keeps the sign-in in a sealed, httpOnly cookie and renews it as you work. **Sign out** ends it, and so does
signing out of Remind on the account site.

The website is for Carbons. Silicons never see a page: they use the CLI (`silicon-apps install remind`) and sign in
with a short-lived token, as the [CLI guide](cli/README.md) shows.

## What you see

- **Your Silicons**: every Silicon you look after, each with the reminders it set, current and archived, and their
  execution history. Archived reminders stay readable for 45 days.
- **Shared with you**: the reminders of Silicons whose owner, or custodian, shared them with you.
- **Webhook subscriptions** of your Silicons, read-only: where each reminder is posted, never the signing secret.

You read reminders; your Silicons create, change, pause and archive them. A custodian never acts as its Silicon.

## Share your Silicons' reminders

Open one of your Silicons and share its reminders with another account by its id, `c:…` for a Carbon or `si:…` for
a Silicon. They can read the reminders until you remove them. Any Carbon can be added. A Silicon from outside the
Silicons you look after must first allow you (or its custodian allows you for it), because a Silicon acts on what it
receives. See [who sees what](accounts.md#who-sees-what).

## Test environments

Create a test environment, open it, and manage the ones you own or your Silicons own: read its key, rotate it, retire
it and restore it within 30 days. Inside a test environment you are still yourself; only the reminders, deliveries and
logs belong to the test environment. [Testing guide](testing-environments.md).

## When something fails

Every error says what happened and what to do. If a request keeps failing, keep its request id for diagnosis. When
your sign-in ends (you signed out elsewhere, or Remind's access was removed on the account site), the website asks
you to sign in again; nothing is lost.

## Telemetry preference

Settings → Telemetry controls Remind's operational analytics for this browser. It is on by default; turning it off
clears pending events and sends the opt-out header on Remind requests. Remind reduces browser events to fixed event
codes, durations and status codes before sending them: no URLs, error messages, input contents or tokens.
[Bug reports and telemetry](diagnostics.md).
