# Unscoped IAM login deployment — 2026-09-08

Source commit `706fe8e` removes organization-specific browser login and uses
IAM-selected organization grants with silicon-iam-client 1.4.0.

- Instance: `i-0546693fac4a32d6d`, account `234951665042`, region `us-east-1`.
- API and worker image:
  `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:0e3ceff90919b23ec4279e819a7f282c6d80d5c9ab9f51c790d6b6b32fb5e6fd`.
- Frontend image:
  `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:648eae71f3860603f28c98822848b61cd0401e0191464b23f018a12e2d242072`.
- Backend preflight SSM: `79030097-ee2d-4a90-8ed9-c2dbb75e7e3d`.
- Backend rollout SSM: `3d636696-370f-47d8-a9da-3d41bffb07be`.
- Frontend rollout SSM: `eaff7c2c-9d55-4113-ba83-4ca3a60a4ba6`.
- Final container check SSM: `8807bb12-bcdd-4e65-a9f0-c5375b50bf95`.

Both images were built for ARM64. The backend retains the AWS RDS certificate
bundle through `deploy/aws/Dockerfile.runtime`; an isolated API container passed
readiness with the real runtime configuration before rollout. API and worker
were replaced gracefully and both persistent startup image references updated.
The frontend installer preserved its encrypted session store and configuration.
No database migration, DNS or credential change was required.

## Verification

- 128 Rust tests passed, along with strict Clippy, formatting, package-doc sync,
  the frontend gateway regression suite and production build.
- Both API and worker reported healthy on the new digest; the frontend was
  running on its new digest. Both systemd services were active.
- Public backend readiness and frontend gateway readiness returned 200.
- An existing production browser session survived the frontend update and
  discovered its authorized organization.
- Continue with IAm navigated to IAM with application ID and callback only.
  IAM displayed Choose organizations and the existing authorized `tos` grant.
  Continuing returned to `https://remind.teamofsilicons.com/#reminders` as
  `saket`, with `tos` selected and the correct Carbon read-only view.
- No production reminder data was modified. Local QA services were stopped.

## Rollback and reprovisioning

The previous backend digest was
`sha256:257c4f53ee2e507265fa9345fd7a302ada7f6d5b9b6c8587bba8e8d281a72dc6`;
the prior startup script is `/usr/local/sbin/remind-start.before-706fe8e`.
The previous frontend digest was
`sha256:eeeec3c91b007948f06eceb2803bffc21bc4e149f17edb20f9703536b120b486`.
The old frontend uses organization-specific login, which current IAM rejects.
CloudFormation was not changed by this in-place release; use the new backend
digest on reprovisioning and reinstall the new frontend afterward.
