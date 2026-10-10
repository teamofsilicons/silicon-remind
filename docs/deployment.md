# Production deployment and releases

Production Remind runs on one ARM64 EC2 host: the API, the worker, the web and the documentation site behind Caddy.
The [standalone guide](../deploy/aws/README-standalone.md) has the commands. The AWS profile is
`silicon-production`, account `234951665042`, region `us-east-1`. Public names:

- `https://backend.remind.teamofsilicons.com`: the API (`/api/*`, `/health/*`) and the Silicon Accounts app
  webhook (`/webhook/`).
- `https://remind.teamofsilicons.com`: the web, which signs Carbons in on the Silicon Accounts pages.
- `https://docs.remind.teamofsilicons.com`: these docs.

## Silicon Accounts and Silicon Apps

Remind is the app `remind` at both. At Silicon Accounts its sign-in setup must have:

- `timezone` among the details it asks for (reminders run on the account's clock);
- the redirect URI `https://remind.teamofsilicons.com/auth/callback` and the origin `https://remind.teamofsilicons.com`
  (the web);
- `device_flow: true` (Carbons' `remind login`) and `public_client: true` (the CLI exchanges Silicons' short-lived
  tokens and refreshes with `client_id=remind` alone; it never holds the secret).

Its app webhook is `https://backend.remind.teamofsilicons.com/webhook/`, with `custodian_change` picked besides the
default events. The app secret and the webhook secret go into the runtime secret (`REMIND_APP_SECRET`,
`REMIND_ACCOUNTS_WEBHOOK_SECRET`); the web reads the app secret from the same place. `REMIND_PROOF_ISSUERS` is
`remind.schedules.read=interface`, so the Silicon Interface can read reminders for an account with a User
verification proof. The [service-only routes](internal-api.md) describe the webhook.

At Silicon Apps, the app `remind` is public, with its description, its links (website and docs) and one package per
target in each release.

## Runtime and data

The API and worker run from one image (the root [Dockerfile](../Dockerfile), wrapped with
[Dockerfile.runtime](../deploy/aws/Dockerfile.runtime) for the RDS CA bundle). They use the existing RDS PostgreSQL 17
instance: database `silicon_remind` through the restricted `remind_runtime` role, and `silicon_remind_test` (test
environment control tables and one schema per test environment) through `remind_testing`. The production runtime role
can operate data and read migration history but cannot change schemas. The testing role owns its control tables and
replica schemas, because test-environment lifecycle operations create, clean and drop those schemas; it has no
database-creation, role-creation, superuser or RLS-bypass rights. Every test-environment schema uses the same embedded
migrations as production. Back up the databases and the encryption keyring together.

Configuration comes from Secrets Manager `silicon-remind/runtime-production`; the host writes only known keys into
`/etc/remind/runtime.env`. Every setting is described in [.env.example](../.env.example).

## Release procedure

1. **CLI.** Tag `v<version>` (equal to `crates/cli/Cargo.toml`). The release workflow builds and checks the six
   targets and keeps one Silicon Apps archive per target as the artifact `remind-silicon-apps-release`. Upload the
   Linux archives to Silicon Apps, make a development release, check it, then promote it
   ([release guide](releases.md)). The macOS and Windows archives wait for their Silicon Apps validation workers.
2. **Crates.** Run `python3 scripts/sync-package-docs.py --check`, inspect both package file lists for credentials and
   server code, publish `silicon-remind-client`, wait for crates.io to index it, then publish `silicon-remind-cli`
   (it depends on that version). Use the stored registry credential without putting it in shell arguments or logs.
   Versions cannot be overwritten: record them before publishing.
3. **Backend.** Build the image from reviewed source (or run `.github/workflows/deployment-builds.yml`), push a
   unique tag to ECR `silicon-remind-production` and resolve its digest. Stop writers if the migration needs it, take
   database backups, run `remind-migrate` with the migration owner's credentials, re-apply
   [runtime-grants.sql](../deploy/runtime-grants.sql), check a temporary API container's `/health/ready` with the
   real configuration, then replace the API and worker containers with the new digest.
4. **Web.** Build the ARM64 image from `web/`, push it, and install it with `deploy/aws/deploy-web.py <digest>`.
5. **Docs.** `npm ci && npm run build && npm run check` in `docs-site/`, then publish `docs-site/dist/` (see
   [docs-site/README.md](../docs-site/README.md)).

Migrations are forward-only: rolling back an image does not undo them. Check public `/health/ready`,
`GET /api/versions` (`"current": 2`), a Silicon signing in with `remind login` and creating a reminder, and its
custodian seeing it on the web.

The one-time switch from the previous identity service is described in
[docs/migration/cutover.md](migration/cutover.md).

## Alternative: Fargate

[`deploy/aws/production.yaml`](../deploy/aws/production.yaml) is an alternative, not live, deployment: private ARM64
Fargate API and worker tasks behind a load balancer and a rate-limiting WAF, with separate encrypted RDS instances for
production and test data. The load balancer forwards only `/api/*`, `/webhook`, `/webhook/` and `/health/*`; metrics
stay private. Its one-off bootstrap task (`deploy/aws/bootstrap-task.py`) creates the restricted roles, runs the
migrations and publishes the runtime secret from the app secret (`REMIND_APP_SECRET`,
`REMIND_ACCOUNTS_WEBHOOK_SECRET`, `REMIND_ENCRYPTION_KEYRING` and two database passwords); it refuses to change
database URLs or the keyring on a rerun. Start with `RuntimeDesiredCount=0`, run the bootstrap task once, then set it
to 1. For updates, keep every existing stack parameter unless you mean to change it and inspect proposed replacements.

## DNS

Namecheap manages `teamofsilicons.com`. `backend.remind` is an A record to the host's public IP; `remind` and
`docs.remind` are CNAMEs of it. Change only these records; never replace unrelated records from a stale zone snapshot.

Earlier deployments, with their digests and evidence, are recorded in [docs/history/deploy/](history/deploy/).
