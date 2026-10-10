# Remind: production cutover to Silicon Accounts

What a Carbon does to switch production Remind from Silicon IAM and Honeycomb to Silicon Accounts. Nothing here was
done by the migration agents: they never touch production. Later stages append their own steps (CLI, web, packaging).

The switch is all at once. Contract 1 (`/api/v1`) is retired by the new service, and IAM tokens stop working the
moment the new image runs, so every caller of Remind moves at the same time.

## 1. Before the window

1. **Sign-in setup of the production app `remind`** (Silicon Accounts developer platform, or `PATCH
   /v1/apps/remind/signin-config` with the current `config_version`):
   - keep `timezone` among the requested scopes, as required (reminders run on the account's clock);
   - add the web callback the web stage documents, and its origin;
   - turn on what the CLI stage documents (device flow, public client) for `remind login`.
2. **Webhook.** Set the app webhook to `https://backend.remind.teamofsilicons.com/webhook/` (the URL IAM used). Pick
   `id_change`, `display_name_change`, `pfp_change`, `custodian_change`, `access_removed` and `account_deleted`, or every
   update (`events: null`). `custodian_change` is not picked by default and Remind needs it: who looks after a Silicon
   decides who reads its reminders. Store the `whsec_…` secret shown once.
3. **Service secrets and settings** (where the API and worker get their environment):
   - `ACCOUNTS_URL=https://accounts.teamofsilicons.com`
   - `REMIND_APP_ID=remind`
   - `REMIND_APP_SECRET=<the app secret of remind>`
   - `REMIND_ACCOUNTS_WEBHOOK_SECRET=<whsec_… from step 2>` (comma-separate old and new during a rotation)
   - `REMIND_PROOF_ISSUERS=remind.schedules.read=interface`
   - remove `REMIND_IAM_*`, `REMIND_INTERNAL_API_TOKEN` and `REMIND_HONEYCOMB_*` (left in place they are ignored, with
     a warning per variable at startup).
4. **Mapping file** for existing data: one line `iam_principal_id,accounts_uuid` per IAM-era Silicon or Carbon that owns
   reminders, subscriptions or test environments. The left side may be the `si:`/`c:` id the IAM binding names (the
   `iam_identity_bindings` table) or the raw principal UUID. For each id, find the Silicon Accounts uuid
   (`silicon-accounts lookup si:<handle>` or `GET /v1/accounts/by-id/si:<handle>` with the app credentials). Ids that
   changed or were never claimed need a Carbon's judgement.
5. **Dry run** against production data with the production environment loaded (so every uuid is checked with Silicon
   Accounts): `remind-migrate link-identities --file mapping.csv --dry-run`. The JSON report must show `refused: []`.
   `unmatched` lists IAM-era principals that still own live data and have no line: their reminders keep firing but
   nobody can see or change them until they are linked.

## 2. In the window

1. Deploy the new image. Run `remind-migrate` first (applies migration 0010; additive, no data deleted), then the API
   and the worker.
2. At the same time:
   - the **Silicon Interface** switches to verification proofs for Remind: `Authorization: Proof sap_…`, issued by
     `interface` for receiving app `remind` with scope `remind.schedules.read`, on `GET /api/v2/schedules`,
     `/schedules/{id}`, `/schedules/{id}/executions`, `/silicons` and `/auth/me`. Its IAM OBO calls stop working;
   - the **Remind CLI and client 0.6.0** and the **web** ship (later stages); older CLIs get `410 api_version_retired`;
   - the **Silicon runtime** keeps running `remind login <SLT>` (the CLI stage keeps that form working).
3. Apply the mapping: `remind-migrate link-identities --file mapping.csv`. It can be re-run at any time with
   corrections (a new uuid re-points the same rows; `si:old,-` unlinks); nothing is rewritten.
4. Retire the IAM side: unregister Remind's IAM application webhook and remove Remind from Honeycomb (participant and
   catalog entries). Test environments created by IAM or Honeycomb automation stay dormant in the testing database.

## 3. Check

- `GET https://backend.remind.teamofsilicons.com/health/ready` is ok and `GET /api/versions` shows `current: 2`.
- A Silicon signs in (`remind login <slt>`), creates and lists a reminder; its custodian sees it in the web; another
  Carbon does not.
- `GET /v1/apps/remind/webhook/deliveries` at Silicon Accounts shows 200s; send a test ping
  (`POST /v1/apps/remind/webhook/test`) and see `ignored` in the API log.
- The link report's linked Silicons see their old reminders (`GET /api/v2/schedules`).

## 4. Rolling back

Migration 0010 only adds tables and relaxes `NOT NULL` on `org_id`, so the old image can run on the migrated database
only until the new service writes its first row without an organization (any reminder, subscription, report or
idempotency record). After that the old code cannot read those rows: roll forward instead.

## 5. The CLI and the Rust client (added by the client and CLI stage)

Before the window:

1. The production sign-in setup of `remind` must have **`device_flow: true`** (Carbons' `remind login`) and
   **`public_client: true`** (the CLI exchanges Silicons' short-lived tokens and refreshes with `client_id=remind`
   alone; it holds no secret). The local test stack already has both. Without them `remind login` answers
   `sign_in_not_allowed` and says how to turn them on.
2. Publish `silicon-remind-client` 0.6.0, then `silicon-remind-cli` 0.6.0 to crates.io (in that order; the CLI depends
   on the client), and upload the 0.6.0 archives to Silicon Apps (the packaging stage prepares them). The CLI's
   `cargo package` verification can only pass once the client is on crates.io.

In the window, at the same time as the service:

3. **Silicons on the old CLI (0.5, installed by Honeycomb).** It speaks contract 1, so after the service switch every
   command answers `410 api_version_retired`; `remind login <SLT>` cannot work either. Installing 0.6.0 through Silicon
   Apps fixes it. On its first change 0.6.0 archives the old `state.json` as `state.iam-<time>.json` and asks to sign
   in again; nothing else is needed on the machine. A machine that still runs the 0.1 updater service can remove it
   with `remind daemon uninstall` (hidden, kept for one release).
4. **The Silicon runtime** (`silicon connect` in stemcell) keeps running `remind login <SLT>` and `remind iam --json`;
   both keep working (the second prints the `remind accounts --json` object). The runtime must mint the token from
   Silicon Accounts (`silicon-accounts login --app remind -q`, an `slt_…`), not from the previous identity service:
   the CLI refuses anything that does not start with `slt_` before sending it, with a hint that says how to mint one.
   This is a change in stemcell, outside this migration.

Check:

5. On a clean machine: `remind --help`, `remind accounts --json` (shows `"app_id":"remind"`) and
   `remind login status --json` (`{"authenticated":false}`) all exit 0. Then
   `silicon-accounts login --app remind -q | remind login --slt-stdin`, `remind login status --json`
   (`"verified":true`), `remind create …`, `remind list`, `remind logout`; and a Carbon's `remind login` approved on
   the account site, then `remind silicons`.
