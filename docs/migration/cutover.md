# Remind: production cutover to Silicon Accounts and Silicon Apps

What a Carbon does to switch production Remind from Silicon IAM and Honeycomb to Silicon Accounts and Silicon Apps.
Nothing here was done by the migration agents: they never touch production. Every command that changes production
is marked **run at cutover**; read the whole runbook first.

The switch is all at once. Contract 1 (`/api/v1`) is retired by the new service, and IAM tokens stop working the
moment the new image runs, so every caller of Remind moves at the same time. Plan a window of about an hour.

The commands assume a shell with:

```sh
export AWS_PROFILE=silicon-production AWS_REGION=us-east-1
export INSTANCE=i-0546693fac4a32d6d
export RUNTIME_SECRET=arn:aws:secretsmanager:us-east-1:234951665042:secret:silicon-remind/runtime-production-dbqkfb
export ECR=234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production
export ACCOUNTS=https://accounts.teamofsilicons.com
read -rs REMIND_APP_SECRET && export REMIND_APP_SECRET   # Remind's app secret at Silicon Accounts; never echo it
```

Commands on the host run as root through SSM (`aws ssm start-session --target $INSTANCE`, then `sudo -i`), the way
the earlier deployments did ([records](../history/deploy/)); export `ECR`, `RUNTIME_SECRET`, `BACKEND_DIGEST` and
`WEB_DIGEST` there too. The digests are written `sha256:<64 hex digits>`.

## 0. Where Remind sits in the order, and what must be ready outside it

- **Remind calls no other app** (it issues no proofs) and no app among the eight being migrated calls Remind, so
  its window does not depend on theirs. Its deliveries go to the Silicons' own webhook subscriptions (Hook or any
  https URL), signed with each subscription's secret, not with an identity: they keep working through the switch.
  The delivery payload gains `silicon_uuid`; `silicon_id` stays the current id.
- **Ting**: no prerequisite. Remind does not deliver through Ting.
- **Silicon Interface** (outside this migration) reads Remind for Carbons and Silicons. It must switch to User
  verification proofs at the same time (step 2.9); until it does, its Remind views fail.
- **Silicon runtime** (stemcell `silicon connect`, outside this migration) must mint the Silicon's token from Silicon
  Accounts (`silicon-accounts login --app remind -q`) and install Remind with Silicon Apps instead of Honeycomb. It
  keeps calling `remind login <SLT>` and `remind iam --json`; both keep working in 0.6.0 (the second prints the
  `remind accounts --json` object, hidden and kept for one release). The CLI refuses a token that does not start with
  `slt_` (for example an `oac_` token from the previous identity service) before sending it, with a hint.
- **The fleet**: every Silicon machine needs `silicon-apps` (with its updater) and `silicon-accounts`. The Silicon
  Apps installer sets both up ([install guide](https://developers.teamofsilicons.com/docs/apps/start/install)).

## 1. Before the window (nothing changes for users)

### 1.1 The sign-in setup of `remind` at Silicon Accounts (run at cutover, any time before the window)

The production app `remind` exists. Read its setup, then send back the arrays you want whole (arrays replace) with
the version you read:

```sh
curl -s -u "remind:$REMIND_APP_SECRET" "$ACCOUNTS/v1/apps/remind" \
  | python3 -c 'import json,sys; a=json.load(sys.stdin); c=a.get("signin_config",a); print(json.dumps({"config_version":a.get("config_version"),"redirect_uris":c.get("redirect_uris"),"required_fields":c.get("required_fields"),"optional_fields":c.get("optional_fields"),"device_flow":c.get("device_flow"),"public_client":c.get("public_client")},indent=1))'
```

It must end up with:

- `timezone` in `required_fields` (reminders run on the account's clock; keep it required);
- `https://remind.teamofsilicons.com/auth/callback` in `redirect_uris` (the web; keep every URI already there);
- `device_flow: true` (Carbons' `remind login`) and `public_client: true` (the CLI exchanges Silicons' short-lived
  tokens and refreshes with `client_id=remind` alone; it holds no secret). Without them `remind login` answers
  `sign_in_not_allowed` and says how to turn them on.

```sh
curl -s -X PATCH -u "remind:$REMIND_APP_SECRET" "$ACCOUNTS/v1/apps/remind/signin-config" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: remind-cutover-signin-1' \
  -d '{"expected_version": <config_version>, "device_flow": true, "public_client": true,
       "required_fields": [<every field you read>, "timezone" unless it is there],
       "redirect_uris": [<every uri you read>, "https://remind.teamofsilicons.com/auth/callback"]}'
```

`allowed_origins` only matters for pages that frame the sign-in buttons; the web uses the hosted pages, so it needs
none.

### 1.2 The webhook secret (run at cutover, before the window)

Remind's app webhook is `https://backend.remind.teamofsilicons.com/webhook/`, the URL IAM delivers to today. Do
not point Silicon Accounts at it until the new service runs (step 2.6): the old service would refuse every delivery.
Make the secret first; saving the URL later keeps it:

```sh
curl -s -u "remind:$REMIND_APP_SECRET" "$ACCOUNTS/v1/apps/remind/webhook"          # url, secret_set, events
curl -s -X POST -u "remind:$REMIND_APP_SECRET" -H 'Idempotency-Key: remind-cutover-whsec-1' \
  "$ACCOUNTS/v1/apps/remind/webhook/generate-secret"                                # {"secret":"whsec_…"}, shown once
```

If the first call shows a URL already set, the webhook exists: use `…/webhook/rotate-secret` instead (the old secret
stops at once) and note that deliveries are already failing against the old service until step 2.6.

### 1.3 The runtime secret (run at cutover, before the window)

Add the new keys to Secrets Manager `silicon-remind/runtime-production`, keeping every existing field (the IAM-era
keys stay until rollback is no longer wanted; the new service ignores them with one warning each):

```sh
aws secretsmanager get-secret-value --secret-id "$RUNTIME_SECRET" --query SecretString --output text \
  | python3 -c '
import json, os, sys, getpass
secret = json.load(sys.stdin)
secret["REMIND_APP_SECRET"] = os.environ["REMIND_APP_SECRET"]
secret["REMIND_ACCOUNTS_WEBHOOK_SECRET"] = getpass.getpass("whsec_ secret from 1.2: ")
secret.setdefault("ACCOUNTS_URL", "https://accounts.teamofsilicons.com")
secret.setdefault("REMIND_APP_ID", "remind")
secret.setdefault("REMIND_PROOF_ISSUERS", "remind.schedules.read=interface")
print(json.dumps(secret))' \
  | aws secretsmanager put-secret-value --secret-id "$RUNTIME_SECRET" --secret-string file:///dev/stdin
```

The merged secret goes from one process to the next and never touches a file or the shell history.

The web reads `REMIND_APP_SECRET` from the same secret ([install-web.sh](../../deploy/aws/install-web.sh)).

### 1.4 Images, packages and crates (build now, publish in the window)

- **Backend image**: run `.github/workflows/deployment-builds.yml` (image `backend`) on the release commit, or build it
  as [README-standalone.md](../../deploy/aws/README-standalone.md#backend-image) shows; push it to `$ECR` and note
  its digest as `BACKEND_DIGEST` (`sha256:…`).
- **Web image**: the same workflow (image `frontend`) builds `web/Dockerfile`; push it and note `WEB_DIGEST`.
- **CLI archives**: tag `v0.6.0`; the release workflow keeps `remind-silicon-apps-release` (six archives and
  `SHA256SUMS`). Upload the two Linux archives and make a development release (run at cutover, before the window;
  nobody gets it until it is promoted):

  ```sh
  sha256sum --check SHA256SUMS
  silicon-apps upload remind --target linux-x86_64 remind-0.6.0-linux-x86_64.tar.gz
  silicon-apps upload remind --target linux-aarch64 remind-0.6.0-linux-aarch64.tar.gz
  silicon-apps packages remind                           # both accepted by their validation workers
  silicon-apps release remind --version 0.6.0 --package <linux-x86_64 id> --package <linux-aarch64 id>
  ```

  The macOS and Windows archives wait until Silicon Apps has validation workers for those targets. Check that
  `silicon-apps setup remind show` has the description, public access and the links (website
  `https://remind.teamofsilicons.com`, docs `https://docs.remind.teamofsilicons.com`).
- **Crates**: `silicon-remind-client` 0.6.0, then `silicon-remind-cli` 0.6.0 (the CLI depends on the client, and
  its `cargo package` verification passes only once the client is on crates.io). Publish them in the window (2.8):
  0.6.0 cannot talk to the old service.
- **End to end, on the release commit**: against a local Silicon Accounts stack,
  `REMIND_TEST_STACK=<stack file> scripts/e2e-accounts.sh` passes every check (it starts and stops the local service
  itself; see the README, "Run against a local Silicon Accounts stack"). Keep its `.mig/e2e/<run>/results.json` with
  the release notes. It never touches production.

### 1.5 The identity mapping, on a restored copy (run at cutover, the day before)

Existing reminders, subscriptions and test environments belong to IAM principals. `remind-migrate link-identities`
attaches each principal to the Silicon Accounts account it became, without rewriting a row. It needs migration 0010,
so prepare the mapping on a copy of production first, as the 0.4.0 cutover did:

1. Dump both databases with the migration owner's credentials (`pg_dump -Fc` of `silicon_remind` and
   `silicon_remind_test`) and restore each into a scratch database next to it (`remind_cutover_copy` beside
   `silicon_remind`, `remind_cutover_copy_test` beside `silicon_remind_test`).
2. On the host, with the new image, migrate the copy and take a dry run with an empty mapping. Put the copy's owner
   URLs in a root-only env file first: `REMIND_ENVIRONMENT=production`, `REMIND_MIGRATOR_DATABASE_URL` (the copy of
   `silicon_remind`, as its owner) and `REMIND_TEST_MIGRATOR_DATABASE_URL` (the copy of `silicon_remind_test`), both
   with `sslmode=verify-full&sslrootcert=/opt/silicon-remind/aws-rds-global-bundle.pem` like the runtime URLs:

   ```sh
   # on the host (run at cutover)
   install -d -m 0700 /etc/remind/cutover
   docker run --rm --network host --env-file /etc/remind/cutover/copy.env \
     --entrypoint /usr/local/bin/remind-migrate "$ECR@$BACKEND_DIGEST"
   docker run --rm --network host --env-file /etc/remind/cutover/copy.env \
     -v /etc/remind/cutover:/cutover:ro --entrypoint /usr/local/bin/remind-migrate "$ECR@$BACKEND_DIGEST" \
     link-identities --file /dev/null --dry-run > /etc/remind/cutover/report.json
   ```

   The report's `unmatched` list names every IAM-era principal that still owns reminders or subscriptions, with the
   `si:`/`c:` id it had and what it owns.
3. Turn the report into a mapping (from any machine with the report and the app secret):

   ```sh
   python3 scripts/suggest-identity-links.py report.json --output mapping.csv
   ```

   It looks each id up at Silicon Accounts with Remind's credentials and writes commented suggestions with the
   current kind and custodian. A Carbon must confirm **every** principal-to-account relationship before uncommenting
   its line. Matching handles alone are not proof of identity: handles can be released and claimed by someone else.
   Find the account using independent ownership records, or leave the principal unlinked. Unlinked reminders keep firing, but nobody can see or change them until they are linked.
4. Copy `mapping.csv` to `/etc/remind/cutover/` and dry-run it against the copy with the app secret in the env file
   (`REMIND_APP_SECRET`, `ACCOUNTS_URL`), so every uuid is checked with Silicon Accounts:

   ```sh
   docker run --rm --network host --env-file /etc/remind/cutover/copy.env \
     -v /etc/remind/cutover:/cutover:ro --entrypoint /usr/local/bin/remind-migrate "$ECR@$BACKEND_DIGEST" \
     link-identities --file /cutover/mapping.csv --dry-run
   ```

   The report must show `"refused": []`. The left side of a line may also be the raw principal UUID; `si:old,-`
   removes a link.
5. Drop the two copy databases.

The migrator logs to standard error, so standard output is the report alone. The whole path (dry run, suggestion,
online dry run, apply, rerun) was proven against the local Silicon Accounts stack; see
`docs/migration/progress.md` (packaging stage).

## 2. In the window

On the host, put the production owner URLs in `/etc/remind/cutover/owner.env` (root, 0600):
`REMIND_ENVIRONMENT=production`, `REMIND_MIGRATOR_DATABASE_URL` (`silicon_remind` as the migration owner) and
`REMIND_TEST_MIGRATOR_DATABASE_URL` (`silicon_remind_test` as `remind_testing`, as the bootstrap task does). The
earlier cutovers carried these in a temporary SecureString parameter and deleted it afterwards; do the same.

### 2.1 Freeze and back up (run at cutover)

```sh
ts=$(date -u +%Y%m%dT%H%M%SZ); backup=/var/backups/remind/accounts-cutover-$ts
install -d -m 0700 "$backup"
systemctl stop remind-frontend.service             # the SolidJS web: no new sessions
docker stop remind-api remind-worker               # writers stop; nothing fires while the schema changes
cp -p /etc/remind/runtime.env /etc/remind/Caddyfile /usr/local/sbin/remind-start /etc/systemd/system/remind-frontend.service "$backup/"
```

Dump both databases after writers stopped (`pg_dump -Fc`, checked with `pg_restore --list`), copy the dumps off the
host to `s3://silicon-browser-production-234951665042-us-east-1/backups/accounts-cutover-$ts/`, and take RDS snapshots
of both instances (`aws rds create-db-snapshot --db-instance-identifier <instance> --db-snapshot-identifier
<instance>-accounts-cutover-$ts`; the instance names are in `deploy/aws/production.outputs.json`). This is how the
0.4.0 cutover kept a way back ([record](../history/deploy/public-identifiers-live-2026-09-23.md)).

### 2.2 Migrate (run at cutover)

```sh
docker run --rm --network host --env-file /etc/remind/cutover/owner.env \
  --entrypoint /usr/local/bin/remind-migrate "$ECR@$BACKEND_DIGEST"
```

Migration 0010 only adds tables (`accounts`, `account_keys`, `identity_links`, `reminder_viewers`,
`silicon_allowances`) and lets `org_id` be empty; testing migration 0007 adds the environment owner. Nothing is
deleted. Then re-apply [runtime-grants.sql](../../deploy/runtime-grants.sql) as the migration owner, so
`remind_runtime` can use the new tables (`psql "$OWNER_URL" -v ON_ERROR_STOP=1 -f runtime-grants.sql`, for example from
a `postgres:17-alpine` container on the host network).

### 2.3 Link the identities (run at cutover)

Copy the reviewed `mapping.csv` from 1.5 to `/etc/remind/cutover/`, add `REMIND_APP_SECRET` and `ACCOUNTS_URL` to
`owner.env` so every uuid is checked again, and run it for real:

```sh
docker run --rm --network host --env-file /etc/remind/cutover/owner.env -v /etc/remind/cutover:/cutover:ro \
  --entrypoint /usr/local/bin/remind-migrate "$ECR@$BACKEND_DIGEST" \
  link-identities --file /cutover/mapping.csv > /etc/remind/cutover/link-report.json
```

`"committed": true` and `"refused": []`; keep the report. The run can be repeated at any time with corrections (a new
uuid re-points the same rows; `si:old,-` unlinks); nothing is rewritten.

### 2.4 The service's settings (run at cutover)

Add the new keys to `/etc/remind/runtime.env`, keeping every other line (the bootstrap wrote the file; later releases
edited it in place):

```sh
aws secretsmanager get-secret-value --region us-east-1 --secret-id "$RUNTIME_SECRET" --query SecretString --output text \
  | python3 -c '
import json, os, sys
secret = json.load(sys.stdin)
keys = ["ACCOUNTS_URL", "REMIND_APP_ID", "REMIND_APP_SECRET", "REMIND_ACCOUNTS_WEBHOOK_SECRET", "REMIND_PROOF_ISSUERS"]
path = "/etc/remind/runtime.env"
kept = [line for line in open(path).read().splitlines() if line.split("=", 1)[0] not in keys]
fd = os.open(path + ".new", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as out:
    out.write("\n".join(kept + [key + "=" + secret[key] for key in keys]) + "\n")
os.replace(path + ".new", path)
print("runtime.env now has", ", ".join(keys))'
```

The retired `REMIND_IAM_*`, `REMIND_INTERNAL_API_TOKEN` and `REMIND_HONEYCOMB_*` lines may stay until rollback is no
longer wanted: the new service ignores them with one warning each.

### 2.5 Switch the API and worker, and close the Honeycomb route (run at cutover)

```sh
sed -i "s#$ECR@sha256:[0-9a-f]\{64\}#$ECR@$BACKEND_DIGEST#g" /usr/local/sbin/remind-start
grep -c "$BACKEND_DIGEST" /usr/local/sbin/remind-start     # 2: the API and the worker
python3 - <<'PY'
from pathlib import Path
path = Path("/etc/remind/Caddyfile")              # bind-mounted: rewrite in place, keep the inode
text = path.read_text().replace(" /internal/honeycomb/organizations/*", "")
with open(path, "r+") as caddyfile:
    caddyfile.seek(0); caddyfile.write(text); caddyfile.truncate()
PY
systemctl restart remind.service                   # recreates Caddy, the API and the worker from remind-start
docker exec remind-api busybox wget -q -O- http://127.0.0.1:8080/health/ready
curl -fsS https://backend.remind.teamofsilicons.com/api/versions        # "current":2
```

### 2.6 Point Silicon Accounts at the new webhook (run at cutover)

```sh
curl -s -X PUT -u "remind:$REMIND_APP_SECRET" "$ACCOUNTS/v1/apps/remind/webhook" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: remind-cutover-webhook-1' \
  -d '{"url":"https://backend.remind.teamofsilicons.com/webhook/","events":["id_change","display_name_change","pfp_change","custodian_change","access_removed","account_deleted"]}'
curl -s -X POST -u "remind:$REMIND_APP_SECRET" -H 'Idempotency-Key: remind-cutover-ping-1' \
  "$ACCOUNTS/v1/apps/remind/webhook/test"
```

The answer's `"secret": null` means the secret from 1.2 was kept. `custodian_change` is not picked by default and
Remind needs it: who looks after a Silicon decides who reads its reminders (`"events": null`, every update, also
works). The ping shows in `docker logs remind-api` as "Silicon Accounts webhook delivery handled" with outcome
`ignored`; `GET $ACCOUNTS/v1/apps/remind/webhook/deliveries` shows 200s.

### 2.7 The web (run at cutover)

```sh
python3 deploy/aws/deploy-web.py "$ECR@$WEB_DIGEST" --instance "$INSTANCE"
```

It writes `/etc/remind/web.env` from the runtime secret, starts `remind-web.service`, disables
`remind-frontend.service` and points `remind.teamofsilicons.com` at the new web. Old browser sessions do not carry
over: Carbons sign in again with Silicon Accounts.

### 2.8 Release the CLI and the crates (run at cutover)

```sh
silicon-apps promote remind <development release id from 1.4> --version 0.6.0
cargo publish -p silicon-remind-client       # wait until crates.io shows 0.6.0, then:
cargo publish -p silicon-remind-cli
```

Silicon Apps' updater moves installed copies to 0.6.0 within about a minute.

### 2.9 At the same time, outside Remind

- The **Silicon Interface** switches to verification proofs for Remind: `Authorization: Proof sap_…`, issued by
  `interface` for receiving app `remind` with scope `remind.schedules.read`, on `GET /api/v2/schedules`,
  `/schedules/{id}`, `/schedules/{id}/executions`, `/silicons` and `/auth/me`. Its IAM calls stop working now.
- The **Silicon runtime** mints Accounts tokens for `remind login <SLT>` and installs Remind with Silicon Apps (§0).
- The **documentation site**: publish a build of this revision (`docs-site/`), as
  [docs-site/README.md](../../docs-site/README.md) describes.

## 3. Check

- `GET https://backend.remind.teamofsilicons.com/health/ready` is ok, `GET /api/versions` shows `"current": 2`, and
  `GET /api/v1/schedules` answers `410 api_version_retired`.
- On a clean machine: `remind --help`, `remind accounts --json` (shows `"app_id":"remind"`) and
  `remind login status --json` (`{"authenticated":false}`) all exit 0. Then
  `silicon-accounts login --app remind -q | remind login --slt-stdin`, `remind login status --json`
  (`"verified":true`), `remind create …`, `remind list`, `remind logout`; and a Carbon's `remind login` approved on
  the account site, then `remind silicons`.
- The Silicon's custodian sees the new reminder on https://remind.teamofsilicons.com; another Carbon does not, until
  the Silicon shares with it (`remind share add c:<carbon>`).
- The Silicons in the link report see their old reminders (`GET /api/v2/schedules`), and their old reminders fire.
- The webhook ping was `ignored` (2.6), and `deploy/aws/inspect-webhook-receipts.py` lists receipts with
  `"source":"silicon-accounts"`.
- The Silicon Interface shows reminders through its proofs.

## 4. Rolling back

Migration 0010 only adds tables and relaxes `NOT NULL` on `org_id`, so the old image can run on the migrated database
only until the new service writes its first row without an organization (any reminder, subscription, report or
idempotency record). After that the old code cannot read those rows: roll forward, or restore the 2.1 dumps and lose
what was written since.

To go back before that point (run at cutover, only if the checks fail):

1. Copy back `runtime.env`, `Caddyfile` (in place, keeping the inode), `remind-start` and
   `remind-frontend.service` from the 2.1 backup; `systemctl disable --now remind-web.service`;
   `systemctl daemon-reload && systemctl restart remind.service && systemctl enable --now remind-frontend.service`.
2. Remove the Silicon Accounts webhook URL (`DELETE $ACCOUNTS/v1/apps/remind/webhook`), so its deliveries stop.
   Remind's IAM webhook and Honeycomb entries are still in place (they are retired only in §6).
3. Do not promote the Silicon Apps release, or, if it was promoted, tell the Silicons to keep the Honeycomb-installed
   0.5 CLI until the next attempt: 0.6.0 cannot talk to the old service.
4. The Interface and the Silicon runtime go back to their IAM paths.

The `identity_links` rows and the new tables can stay; the old service never reads them, and the next attempt reuses
them.

## 5. Silicons still on the Honeycomb-installed CLI

The 0.5 CLI speaks contract 1, so after the switch every command answers `410 api_version_retired`, and
`remind login <SLT>` cannot work either. Installing 0.6.0 with Silicon Apps fixes it:

```sh
silicon-apps install remind
command -v remind && remind --version          # remind 0.6.0, from $SILICON_HOME/.apps/bin (or ~/.apps/bin)
silicon-accounts login --app remind -q | remind login --slt-stdin
```

If `command -v remind` still names Honeycomb's copy, remove that copy with Honeycomb's own uninstall (or put the
Silicon Apps directory first on `PATH`). On its first change 0.6.0 archives the old `state.json` as
`state.legacy-<time>.json` (keeping the API address, telemetry choice and 32-character test-environment keys) and asks
to sign in again; nothing else is needed on the machine. A machine that still runs the 0.1 updater service removes it
with `remind daemon uninstall` (hidden, kept for one release). Neither the CLI nor Silicon Apps' updater touches a
Honeycomb installation.

## 6. After the window

- Unregister Remind's IAM application webhook and remove Remind from Honeycomb (lifecycle participant, catalog entry
  `tos>remind`, the Silicon Interface bundle). Test environments that IAM or Honeycomb automation created stay dormant
  in the testing database: never listed, data kept.
- Once rollback is no longer wanted, remove `REMIND_IAM_*`, `REMIND_INTERNAL_API_TOKEN` and `REMIND_HONEYCOMB_*` from
  the runtime secret and `/etc/remind/runtime.env`, and delete `/etc/remind/cutover/`.
- `/etc/remind/frontend.env` and `/var/lib/remind-frontend/sessions` (the SolidJS web's encrypted sessions) are no
  longer read; keep them with the 2.1 backup for as long as the other backups, then delete them together.
- Run `scripts/suggest-identity-links.py` again on a fresh dry-run report whenever a Carbon finds the account of a
  principal that is still unmatched; `link-identities` applies the new lines without touching the others.
