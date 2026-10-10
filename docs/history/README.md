# Remind history

Dated records of how Remind was built, released and deployed before it moved to Silicon Accounts and
Silicon Apps (Remind 0.6.0). They describe what was true at the time: the identity service, the
organizations, the release channel and the hosts they name are gone or replaced. Nothing here is a
current instruction. The current guides are in [docs/](../README.md), and the move itself is recorded in
[docs/migration/](../migration/decisions.md).

These files are kept as they were, except that links to files outside this folder point at the
repository as of the move (revision `88d1986`). The documentation site and the manuals bundled in the
CLI and client leave this folder out.

| record | what it covers |
| --- | --- |
| [BUILD_STATUS.md](BUILD_STATUS.md), [MANUAL_ACCEPTANCE.md](MANUAL_ACCEPTANCE.md) | the first build and its acceptance evidence |
| [RELEASE_0.1.0.md](RELEASE_0.1.0.md), [RELEASE_0.1.2.md](RELEASE_0.1.2.md), [RELEASE_0.2.0_HONEYCOMB.md](RELEASE_0.2.0_HONEYCOMB.md), [release-0.3.0.md](release-0.3.0.md) | releases 0.1.0 to 0.3.0 |
| [UPDATE_2026_09_13.md](UPDATE_2026_09_13.md) | bug reports and telemetry |
| [timezone-e2e.md](timezone-e2e.md) | the 0.3.0 timezone end-to-end run |
| [IAM5_MIGRATION.md](IAM5_MIGRATION.md), [IAM_CANONICAL_ID_UPGRADE.md](IAM_CANONICAL_ID_UPGRADE.md), [public-identifier-migration.md](public-identifier-migration.md) | earlier identity migrations |
| [iam.md](iam.md), [internal-api-iam-era.md](internal-api-iam-era.md), [honeycomb-lifecycle.md](honeycomb-lifecycle.md) | the retired sign-in, internal API and test-environment lifecycle contracts |
| [deploy/](deploy/) | deployment records from September 2026 |
