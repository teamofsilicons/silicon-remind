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

## Backend deployment pending

AWS SSO credentials expired. Deployment requires renewing the
`silicon-production` profile before ECR push and SSM preflight/rollout.
Production has not yet been changed by this release; the new `iam` command
requires the new backend endpoint before it can work against production.

The ARM64 runtime image is prepared locally as:

`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production:release-0.1.2-aws`

Local image digest:
`sha256:1e2576372b0a912f67e299563fde406409643f7801dcd30e0b8ce1a601cbe73f`.
Resolve the ECR digest after pushing before deployment. The runtime includes
the API/worker executables and the nonempty 165408-byte AWS RDS CA bundle.
An empty cached bundle was discovered during release validation; the runtime
Dockerfile now rejects empty downloads and the wrapper was rebuilt without cache.

The target is the existing standalone instance `i-0546693fac4a32d6d` in account
`234951665042`, region `us-east-1`. Use the existing runtime environment for a
temporary API readiness preflight, then gracefully replace API and worker and
update their persistent startup image references. Preserve the previous digest
and startup script for rollback. Verify public readiness, `/api/v1/auth/iam`,
and the published CLI against the deployed server. No schema migration is needed.
