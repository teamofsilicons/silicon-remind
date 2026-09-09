# Remind client and CLI 0.1.2 — 2026-09-09

Source commit: `46b36bc4b25e1f0f0d54a2eef86cc260881a96f7`, pushed to `main`
and tagged `v0.1.2`.

## Changes

- `SILICON_HOME` selects the default CLI state parent, with the explicit
  `.remind/home` pointer taking precedence within that parent.
- `remind iam --json` discovers public IAM configuration through the new
  `GET /api/v1/auth/iam` endpoint and stateless client `iam()` method.
- `remind login status --json` verifies live Carbon/Silicon authority, refreshing
  near-expiry sessions. No session or HTTP 401 returns `authenticated: false`;
  other service errors remain errors. The client exposes `login_status()`.
- Help and canonical/package documentation cover the new commands.

## Publication and verification

Both crates are published and not yanked:

| Package | Published (UTC) | Archive SHA-256 |
| --- | --- | --- |
| `silicon-remind-client` | 2026-09-09 09:26:09 | `1c9e0c2269f272d53f1f382b9ae78572d9eda7894bdca01407ed824a0c3ac63e` |
| `silicon-remind-cli` | 2026-09-09 09:27:09 | `2fbd08b939ea976558438dab9df0f9bc48b8c0de848e9be87f6b2e66f5a605c0` |

135 workspace tests passed, including PostgreSQL integration tests and six new
CLI regression tests. Strict Clippy, formatting and OpenAPI validation passed.
[Source CI](https://github.com/teamofsilicons/silicon-remind/actions/runs/34334652912)
also passed both the Rust quality gate and OpenAPI job.

A fresh crates.io CLI installation into `target/published-cli-0.1.2` passed
version, help, `SILICON_HOME`, unauthenticated JSON status, public readiness,
and current-version updater checks. Both package archives were built and
verified before publishing; the CLI resolved client 0.1.2 from the registry.

## Production backend deployed

The ARM64 runtime image was pushed to ECR and deployed to the existing standalone
instance `i-0546693fac4a32d6d`, account `234951665042`, region `us-east-1`.

- ECR tag: `silicon-remind-production:release-0.1.2-aws`.
- API and worker image:
  `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:1e2576372b0a912f67e299563fde406409643f7801dcd30e0b8ce1a601cbe73f`.
- Preflight SSM command: `91c4bd5e-d03f-48c1-aa05-0869dba18258`.
- Rollout SSM command: `e9960352-ee44-4a0c-ba26-fd1d75764430`.
- Previous image:
  `sha256:0e3ceff90919b23ec4279e819a7f282c6d80d5c9ab9f51c790d6b6b32fb5e6fd`.
- Previous startup script: `/usr/local/sbin/remind-start.before-46b36bc`.

The runtime includes the nonempty 165408-byte AWS RDS CA bundle. An empty cached
bundle was discovered during release validation; the runtime Dockerfile now
rejects empty downloads and the wrapper was rebuilt without cache. A temporary
API container passed readiness and IAM discovery with the existing production
runtime configuration before rollout, then was removed.

API and worker were gracefully replaced and both became healthy on the new
digest. Both persistent image references in `/usr/local/sbin/remind-start` were
updated. The `remind` and `remind-frontend` systemd units remained active; Caddy
and the frontend were not restarted. No migration, DNS, credential, or production
reminder data changes were made. CloudFormation was not changed; use the new
digest when reprovisioning, and reinstall the existing frontend as documented in
the [standalone guide](../deploy/aws/README-standalone.md).

Public verification after rollout:

- Backend `/health/ready` returned HTTP 200.
- `/api/v1/auth/iam` returned HTTP 200 with `Cache-Control: no-store`,
  `app_id: "tos>remind"`, `iam_url: "https://backend.iam.teamofsilicons.com/"`,
  and `iam_environment_id: null`.
- Frontend `/` and gateway `/ui/api/health/ready` returned HTTP 200.
- The fresh crates.io CLI 0.1.2 returned the deployed IAM metadata through
  `remind iam --json`, passed public readiness, and returned
  `authenticated: false` for an isolated store with no session. Carbon/Silicon
  authenticated status and refresh behavior are covered by the CLI regression
  tests; this rollout did not create a new production IAM session.

The backend package version remains 0.1.0; client and CLI are 0.1.2. The pinned
image digest identifies this backend release. For rollback, gracefully replace
the API and worker with the previous image and restore the saved startup script.
