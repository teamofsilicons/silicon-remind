# Parallel Accounts production release

The user requested migrating the apps while leaving existing IAM Silicons in
place. This supersedes `cutover.md`: no IAM identity, custody, schedule,
subscription, encrypted secret, or historical delivery is adopted or rewritten.

| Plane | Existing IAM | New Accounts |
| --- | --- | --- |
| API origin | `backend.remind.teamofsilicons.com` | `api.remind.teamofsilicons.com` |
| Containers | `remind-api`, `remind-worker` | `remind-accounts-api`, `remind-accounts-worker` |
| Production DB | `silicon_remind` | `silicon_remind_accounts` |
| Testing DB | `silicon_remind_test` | `silicon_remind_accounts_test` |
| Runtime roles | existing roles | `remind_accounts_runtime`, `remind_accounts_testing` |
| Secret | `silicon-remind/runtime-production` | `silicon-remind/accounts-production` |

The fresh stores have distinct passwords and a fresh encryption keyring. Keep the
old keyring and all old runtimes unchanged: existing subscriptions continue to
sign and deliver with their original secrets. The new worker has its own unit,
telemetry mount and 9090 listener within its container. All containers share the
existing private Docker network, but only the API has the new public route.

## Release gates

1. Back up both old RDS stores and all runtime configuration privately. Restore
   and verify locally; leave existing workers running throughout.
2. Create the two new stores/roles. Restrict CONNECT; migrate production and the
   dedicated test control store. Apply `deploy/runtime-grants.sql` with only the
   role name changed to `remind_accounts_runtime`; the test role owns its isolated
   schemas and needs CREATE/TEMPORARY only in the new test database.
3. Stage fresh Accounts app/webhook secrets. Start the new digest-pinned ARM64
   containers privately with small pools and API/worker memory limits. Verify
   readiness, and configure the new Caddy API vhost to return 503 initially.
4. Wait for coordinated Accounts/Apps/Browser UUID completion. Open the new API
   and register `https://api.remind.teamofsilicons.com/webhook/` at Accounts.
   Verify anonymous 401, canonical UUID auth and signed webhook receipt.
5. After the website GO, run `deploy/aws/deploy-web.py` with the new web digest.
   It connects to `remind-accounts-api`, then changes only the website route.
   Preserve the old frontend files/session store for rollback. Verify hosted
   login, a reminder lifecycle, subscription delivery, and test environments.
6. Publish the new Apps CLI. Existing Honeycomb packages and IAM clients remain
   unchanged and continue using the old API.

The new Accounts website starts with no legacy reminders. Old Silicons retain
all schedules and deliveries through their existing clients. Rollback closes
new API ingress and restores the previous website route; never reverse database
migrations or drop either store as an application rollback.
