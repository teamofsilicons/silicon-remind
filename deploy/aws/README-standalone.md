# Standalone EC2 deployment

## Current parallel Accounts release

Follow [parallel-production.md](../../docs/migration/parallel-production.md).
The old IAM API/worker, `backend.remind` origin, `silicon_remind` databases, runtime
secret and fleet remain live. New Accounts containers are `remind-accounts-api`
and `remind-accounts-worker`, using `silicon_remind_accounts` and
`silicon_remind_accounts_test`, secret `silicon-remind/accounts-production`, and
`api.remind.teamofsilicons.com`. These stores start empty; no IAM identities or
subscriptions are imported. Do not rerun the legacy CloudFormation bootstrap.

The new Next.js website uses `http://remind-accounts-api:8080/api/v2`. Its installer
switches public website ingress and may run only after the coordinated website GO.
The remaining sections describe the original host and historical replacement path;
they do not authorize replacing the old API/worker or database.

Production Remind runs on one ARM64 `t4g.small` in `vpc-04b23a487cfe0bd8e`, public subnet
`subnet-07945746462c26b2d`, created by the CloudFormation stack `silicon-remind-standalone`
([standalone.yaml](standalone.yaml)). There is no load balancer: the instance has a public IPv4 address and
Caddy terminates TLS for three names, all on this host:

| name | served by |
| --- | --- |
| `backend.remind.teamofsilicons.com` | `remind-api` (`/api/*`, `/health/*`, `/webhook`, `/webhook/`; everything else 404) |
| `remind.teamofsilicons.com` | `remind-web`, the Next.js web ([install-web.sh](install-web.sh)) |
| `docs.remind.teamofsilicons.com` | the static documentation site ([docs-site](../../docs-site/README.md)) |

The worker (`remind-worker`) has no public route; its operational listener (`:9090`, health and metrics) and the
API's `/metrics` stay inside the instance. The instance has no SSH key: use SSM Session Manager. Its role can read
only the runtime secret and pull only the `silicon-remind-production` ECR repository. RDS access is granted to the
existing security group `sg-0fbecb17f3e0c2521` on port 5432 from the instance's security group.

## Runtime secret

Secrets Manager `silicon-remind/runtime-production` holds the service's configuration. The bootstrap copies only
known keys into `/etc/remind/runtime.env` (root, 0600), which the API and worker read. These must be present and
non-empty:

| key | what it is |
| --- | --- |
| `REMIND_APP_SECRET` | Remind's app secret at Silicon Accounts (app id `remind`). Server only; the web reads it from here too. |
| `REMIND_ACCOUNTS_WEBHOOK_SECRET` | the `whsec_…` secret Silicon Accounts signs Remind's app webhook with; two, comma-separated, while a rotation overlaps |
| `REMIND_ENCRYPTION_KEYRING` | the AES-256-GCM keyring for webhook subscription secrets |
| `REMIND_DATABASE_URL`, `REMIND_TEST_DATABASE_URL` | the restricted runtime connections to `silicon_remind` and `silicon_remind_test` |

Optional keys it passes on when present: `ACCOUNTS_URL` (default `https://accounts.teamofsilicons.com`),
`ACCOUNTS_API_URL`, `REMIND_APP_ID` (default `remind`), `REMIND_PROOF_ISSUERS` (default
`remind.schedules.read=interface`: the Silicon Interface may read reminders for an account with a User verification
proof), `REMIND_ACCOUNTS_REQUEST_TIMEOUT_MS`, `REMIND_ACCOUNTS_LOOKUP_TTL_SECONDS`,
`REMIND_ENCRYPTION_CURRENT_VERSION`, the database pool size, log filter, request and webhook timeouts and the
delivery concurrency. Every setting is described in [.env.example](../../.env.example).

Postmark (bug reports) and Space Station telemetry settings, and the telemetry spool mount, were added to the live
host by hand (see the [deployment records](../../docs/history/deploy/)); the template does not create them. Carry them
over before you re-provision the instance.

The restricted `remind_testing` role needs `CREATE` and `TEMPORARY` on `silicon_remind_test`: each isolated schema
replays migrations that use temporary mapping tables. `bootstrap-task.py` grants these only to the testing role.
Keep database privileges revoked from `PUBLIC`; `remind_runtime` keeps only `CONNECT` plus the table and sequence
grants in [runtime-grants.sql](../runtime-grants.sql).

## Backend image

Build the ARM64 image, then wrap it with [Dockerfile.runtime](Dockerfile.runtime) before pushing. The wrapper adds
the AWS RDS CA bundle that the production database URLs need; the base image alone cannot connect to RDS.

```bash
docker build --platform linux/arm64 -t silicon-remind:release .
docker build --platform linux/arm64 -f deploy/aws/Dockerfile.runtime \
  --build-arg BACKEND_IMAGE=silicon-remind:release \
  -t <ecr-repository>:<release-tag> .
docker push <ecr-repository>:<release-tag>
```

The manual workflow `.github/workflows/deployment-builds.yml` builds the same image (and the web image) on an ARM64
runner and keeps them as artifacts with their SHA-256 sums.

Neither the template nor the containers run migrations. Before switching the image, run the migrator once against
both databases with the migration owner's credentials (`/usr/local/bin/remind-migrate`), then re-apply
[runtime-grants.sql](../runtime-grants.sql) as that owner so the runtime role can use new tables. Verify the API in a
temporary container with the real runtime configuration (`/health/ready` answers 200), then roll out.

## Provision or replace the instance

```bash
AWS_PROFILE=silicon-production AWS_REGION=us-east-1 \
  deploy/aws/deploy-standalone.sh \
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:<backend-digest>
```

The script needs a digest-pinned backend URI. It creates or updates the stack, and prints the instance id, public
IP and URL. Point the Namecheap `backend.remind` A record at the output `PublicIp`; `remind` and `docs.remind` are
CNAMEs of it. Caddy obtains and renews certificates once DNS resolves; keep port 80 open for the ACME HTTP fallback.
The instance's public IP is assigned automatically: after a stop/start or replacement, update the A record.

Provisioning writes the backend Caddy route only. Install the web (below) and the documentation site again after
provisioning or replacing the instance.

Routine releases do not rerun CloudFormation: through SSM they replace the backend image references in
`/usr/local/sbin/remind-start` and replace the API and worker containers with the new image (the deployment records
show the exact steps).

## Web

The web is a Next.js server built from `frontend/` (a standalone build: `WORKDIR /app`, listening on `$PORT`, non-root).
It signs Carbons in on the Silicon Accounts pages, keeps each session in a sealed httpOnly cookie, and calls the API
over the private `remind` Docker network (`APP_API_URL=http://remind-accounts-api:8080/api/v2`). Push its ARM64 image to
`silicon-remind-production`, then install it by digest:

```bash
AWS_PROFILE=silicon-production AWS_REGION=us-east-1 \
  python3 deploy/aws/deploy-web.py \
  234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:<web-digest> \
  --instance <InstanceId>
```

The SSM installer writes `/etc/remind/web.env` (root, 0600): `APP_ID=remind`, `ACCOUNTS_URL`, `APP_API_URL`,
`PUBLIC_URL=https://remind.teamofsilicons.com`, `PORT=3000`, `APP_SECRET` (read from `REMIND_APP_SECRET` in the
runtime secret on every run) and `SESSION_SECRET` (generated once and kept; replacing it signs every browser out).
Values an operator changes in the file are kept. It runs `remind-web.service` (read-only root filesystem, no Linux
capabilities, 384 MiB, rotating logs), waits for the web to answer, points the `remind.teamofsilicons.com` vhost at
`remind-web:3000` (validating the Caddyfile and restoring it on failure) and reloads Caddy.

The first install also stops and disables `remind-frontend.service`, the SolidJS web it replaces. Its
`/etc/remind/frontend.env` and the encrypted session files in `/var/lib/remind-frontend/sessions` are left in place
and no longer read; nothing carries over (everyone signs in again).

Production sign-in needs `https://remind.teamofsilicons.com/auth/callback` among the redirect URIs of the app
`remind` at Silicon Accounts. To roll back the web, run the same command with the previous web digest.

## Check a deployment

```bash
aws --profile silicon-production --region us-east-1 ssm start-session --target <InstanceId>
curl --fail --show-error https://backend.remind.teamofsilicons.com/health/ready
curl --fail --show-error https://backend.remind.teamofsilicons.com/api/versions    # "current": 2
curl --fail --show-error --head https://remind.teamofsilicons.com/
```

Logs: `docker logs remind-api`, `docker logs remind-worker`, `docker logs remind-caddy`, `journalctl -u remind-web`.
[inspect-webhook-receipts.py](inspect-webhook-receipts.py) lists the latest Silicon Accounts webhook receipts through
the restricted runtime role.

The one-time switch from the previous identity service is in
[docs/migration/cutover.md](../../docs/migration/cutover.md). Earlier deployments are recorded in
[docs/history/deploy/](../../docs/history/deploy/).
