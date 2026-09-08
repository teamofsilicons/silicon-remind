# Release 0.1.0

2026-09-06 UTC. Backend source: `13951a4`, with the subsequent database test
fixture precision correction. Runtime code is unchanged by that test-only fix.

## Public deployment complete

- AWS account 234951665042, region us-east-1, `silicon-remind-production` stack:
  initial creation and activation update completed successfully.
- API and worker Fargate tasks are both RUNNING and HEALTHY. The dedicated ALB
  target is healthy. `/health/ready` returns 200 over publicly trusted HTTPS.
- ACM certificate issued; Namecheap validation and `backend.remind` CNAME records
  saved, with five-minute TTL. Public origin:
  `https://backend.remind.teamofsilicons.com`.
- Bootstrap task `9da5f6c27552409ea4b4f2a232ac0611` exited 0 after both databases
  migrated, production runtime grants applied and restricted secrets published.
- Production Carbon `saket` logged in through real IAM with owner/read permissions.
- Created public sandbox `01a0741b-559b-7bd1-a700-f6747113fd8f`, bound to the existing
  IAM test world. Test Silicon `remindrunner:tos` logged in successfully; sandbox
  metadata is accessible. Webhook subscriptions are optional, so reminders can
  be created before a receiver is configured.
- Unsigned public `/webhook/` request is rejected 401. Internal routes and metrics
  are not forwarded by the public load balancer and return 404.
- Original remote CI found four timestamp equality fixtures assuming nanosecond
  database precision on Linux. Fixtures now explicitly use PostgreSQL microsecond
  precision. All 14 local PostgreSQL tests and strict Clippy passed afterward;
  remote CI subsequently passed (run `34021885902`).

The checked-in production parameters contain immutable runtime/bootstrap image
digests; production outputs record resource endpoints. Credentials remain in
Secrets Manager and ignored protected local files. The temporary local database
used for earlier manual acceptance was not deployed or imported.

## IAM webhook active and receiving

Live IAM metadata confirms `tos>remind` has the active receiver
`https://backend.remind.teamofsilicons.com/webhook/`, signing secret version 1,
webhook version 2. Approval was already complete when this verification resumed;
no repeated approval or replacement credential was issued.

A read-only ECS inspection through the restricted production database role found
receipt `01a0745d-1190-79b0-a48f-d8d41ce87606`, type
`organization.silicon.removed.v1`, received at 2026-09-06 01:37:32.293705 UTC and
processed at 01:37:32.297936 UTC. The inspector printed metadata only, exited 0,
and its temporary task definition was deregistered. This proves the deployed
signed receiver processed that event. A matching sender log was not found in
CloudWatch, so this record does not independently trace the entire sender path.
IAM's current app dead-letter list is empty.

## Client and CLI published and verified

Both packages are published, not yanked, at version 0.1.0:

- `silicon-remind-client`: published 2026-09-06 01:39:57 UTC, archive SHA-256
  `9c297fa6bf97f06eb8ab0a31b832fc2c2aa84a969461ae8c7f20ee0cf86a245f`.
- `silicon-remind-cli`: published 2026-09-06 01:40:15 UTC, archive SHA-256
  `622dcb25a004e56b8686e2419530b0d731a6e9934cc573ff52c3850fc5860b02`.

Publication was already complete when verification resumed, so no duplicate
upload was attempted. Both downloaded registry archive checksums match the
registry metadata, and every Rust source file matches this checkout.

Installed the CLI directly from crates.io using `cargo install
silicon-remind-cli --version 0.1.0 --locked --root target/published-cli-verification`.
The installed binary reported `remind 0.1.0`, returned healthy public readiness,
authenticated the existing production Carbon session correctly, and returned
`current` from `update --check`. This verifies registry discovery and a real
registry installation; automatic replacement by a newer version awaits a later
release. Backend implementation remains unpublished and proprietary.

Install for ordinary use with `cargo install silicon-remind-cli --locked`.
Add the client with `cargo add silicon-remind-client`.
