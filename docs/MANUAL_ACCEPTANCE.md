# Manual development and acceptance record

Status: **authenticated local acceptance completed; final container verification
is recorded below**. Public deployment and published-release installation are now verified in
[the release record](RELEASE_0.1.0.md).
No automated scenario suite substitutes for the user-requested manual stage.

## Development checks completed

Date: 2026-09-06 Asia/Kolkata.

- Registry lookup: `silicon-iam-client` latest observed published version 1.2.1.
- `cargo check --workspace --all-targets`: passed after integration changes.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed after adding
  error documentation and factoring the configuration/router/worker changes.
- `python -m openapi_spec_validator openapi.yaml`: passed with validator 0.7.2.
- Dedicated Docker PostgreSQL instance `remind-manual-postgres`, loopback port
  55460; databases `remind` and `remind_testing`. No other project's databases
  were modified. Local credentials remain in git-ignored mode-0600 `.env`.
- `remind-migrate` successfully applied the initial production and test-control
  schema. Additional integration migrations were subsequently applied.
- `GET http://127.0.0.1:8086/health/ready`: 200, service `silicon-remind`.
- `POST /api/v1/auth/login` with an intentionally invalid SLT: 401 with the
  stable unauthenticated envelope. This is negative-path evidence, not proof of
  a working authenticated app session.
- `GET /api/v1/testing-environment` without a test key: 400,
  `test_environment_required`, including CLI guidance.
- `GET /api/v1/auth/me` with an unknown 32-character test key: 401; no
  production fallback.
- `remind --help`: all implemented command groups and global `--test` option
  displayed.
- `remind --no-update --url http://127.0.0.1:8086 health --ready`: returned the
  backend's healthy result through the public Rust client.

## Findings fixed during development

- Independent CLI build initially failed because the client relied on the
  backend to enable the secrecy crate's serde feature. Enabled that feature in
  the client manifest and reran the standalone CLI build successfully.
- Original Remind IAM integration expected an obsolete custom introspection
  projection. Replaced it with official Application authorization snapshots.
- Original visibility restricted Carbons to a separate IAM owner list. Current
  request authentication now follows the updated org-wide read requirement.
- Original webhook provisioning was service-only. Added public owner-bound
  configuration, using IAM's verified public identity.
- Lifecycle operations initially attempted a second control-pool acquisition
  while holding a transaction. Moved the guarded metadata read into the same
  transaction to avoid exhausting the pool under concurrent requests.

## Initial acceptance checklist (historical)

- A production IAM Application session authorized to create a Remind environment.
- IAM test root key, matching test Application credential, and test SLTs for
  Silicon/Carbon identities (or authorized tooling to provision them).
- Real controlled configured webhook receiver delivery destination and signing credential.
- Every CLI command and every Rust client operation on successful authenticated
  paths, including negative and extreme scenarios described in the sandbox guide.
- Cross-environment and cross-org storage/delivery isolation; concurrency; quota;
  lifecycle time boundaries; webhook replays/order; restart and retry behavior.
- Updated Docker build and deployment checks.

The following run records resolve this initial checklist. They distinguish live
IAM/webhook results from signed fixtures and from checks requiring public deployment.

## Live manual run: IAM and webhook integration

2026-09-05 22:58–23:10 UTC (2026-09-06 IST). Commands below use
`target/debug/remind --no-update --json --url http://127.0.0.1:8086`.
Secret arguments were provided via stdin/protected files; values are not logged.

- Created real hosted IAM environment `01a073c6-3276-7ac0-85d7-cb3aeafd2564`
  using the existing authorized production IAM CLI session. Bootstrapped a test
  Carbon `remindmanual` with IAM's fixed sandbox OTP; no email/SMS is sent in
  this plane. Imported `tos>remind` and created `remindrunner:tos`.
- IAM `app show tos>remind`: application verified, signing version 1, public
  webhook pending_review with no active URL. Public deployment is not proven.
- Production Carbon SLT → Remind CLI login: owner identity in `tos`, read-only
  for reminder writes. Test Carbon and test Silicon SLTs both exchanged through
  Remind's official IAM adapter and returned correct identity/permissions.
- `env create manual-main ...`: created empty Remind environment
  `01a073cb-a1ce-74e0-bd68-258134978a07`, paired to the real IAM test world.
  `env create manual-isolation ...` created a second empty replica
  `01a073d5-4bc3-7dd0-a7a5-760bbb42a0fe`. Listing/paging and root `test-info` work.
- `create --text ... --cron '* * * * *'` before configuration succeeds: webhook
  subscriptions are optional.
- Generic webhook contract tests cover arbitrary URLs, multiple fan-out targets,
  unsigned delivery, any 2xx response, and retryable/terminal HTTP outcomes.
- Manual `webhook set <url> --secret-stdin` found a Clap argument collision:
  endpoint URL replaced global service URL. Renamed positional field IDs;
  rerun succeeded. `webhook get` returned endpoint metadata without secret.
- Webhook subscriptions use arbitrary HTTP(S) URLs, optional HMAC signing, and
  the execution UUID as the stable local receipt marker.
- One-time schedule `01a073d2-bcfc-76c1-8890-af491163f186` triggered at 23:07 UTC.
  webhook verified history contained its exact text `Manual one-time delivery ✓ भारत`
  and timezone Asia/Kolkata. Remind incorrectly marked this first attempt failed
  because it expected 202; corrected receipt parsing to 200 + receipt_id.
- Recurring schedule `01a073d3-346e-7e11-ae95-23a34112e93d`: create UTC default,
  pause clears next trigger; edit text/cron/zone while paused stays paused;
  resume calculates next future trigger. The corrected worker delivered its
  23:09 occurrence and persisted the webhook receipt with status delivered.
- `silicons`, `get`, `executions`, archived `list --limit 1` worked. One-time
  archive is automatic with a 45-day purge deadline. Production `list` for this
  test Silicon returned empty, demonstrating one storage-boundary check.
- `auth refresh` succeeded. CLI now saves a pending refresh operation key before
  sending the request and clears it only with atomic new-token persistence.

Development regression (separate from manual acceptance): 115 library checks
and four existing webhook adapter checks passed. OpenAPI shared test-header ref
initially differed from its contract check; normalized the ref and reran all six
OpenAPI checks successfully. Full manual command/extreme-case coverage remains
in progress.

## Manual permissions, lifecycle and extreme boundaries

2026-09-05 23:10–23:31 UTC. No scenario test runner was used for these checks;
SQL below prepared large/aged sandbox fixtures, and individual CLI/API actions
and worker results were then inspected manually.

- Corrected one-time schedule `01a073d5-43b4-77d3-820b-5f0f7129be04` delivered at
  23:10 UTC with a webhook receipt; the first recurring reminder was then archived
  by CLI. Another Remind sandbox returned 404 for its UUID and an empty list.
- `env key`, rotate, old-key request, delete, get, key-on-deleted, restore,
  forget, wrong-ID import and correct import all exercised. Rotated key: 401;
  deleted key retrieval: 404; restore uses a new key; wrong-ID import does not
  overwrite the existing saved main key.
- Test Carbon owner read another Silicon's archived reminder (200), but create
  returned 403. A second real IAM Silicon `remindobserver:tos` likewise read it
  but could not edit it (403). No authority is inferred from org-owner role for
  Carbon reminder mutations.
- Created `manual-boundaries`, ID `01a073dc-d78a-7671-947f-ccbafe65b9d0`, with only
  name and IAM test key. Login before app configuration returned explicit 409
  `test_iam_application_not_configured`; `configure-iam` verified the test-only
  secret, after which real Silicon SLT login succeeded. Production invocation
  of `configure-iam` was rejected with test-only guidance.
- Initial clean failed because the production audit truncate trigger correctly
  rejects truncation. Fixed the sandbox-only cleaning transaction to disable
  only that trigger under the exclusive lifecycle lock, truncate, re-enable,
  and commit atomically. Rerun passed; database inspection confirmed all three
  audit mutation guards remained enabled.
- Maximum text via actual CLI: 100000 UTF-8 bytes accepted; 100001 rejected 422.
  Invalid IANA timezone `Mars/Olympus` rejected 422.
- After one API-created reminder, inserted 98 paused fixture rows into only the
  boundary schema. CLI-created reminder 100 succeeded; 101 returned 409 with
  the explicit test-only quota message. Exact idempotency replay returned the
  original UUID at capacity; changed payload with that key returned conflict.
  Archiving one reminder did not bypass the retained-reminder limit.
- Batch with one valid ID and one other-environment ID returned 404 and left
  the valid reminder active. A valid two-item pause succeeded. A 100-item resume
  returned all 100 active, with the same future occurrence. Duplicate IDs were
  rejected 422. Listing all 100 returned a complete terminal page.
- Cleaned this nonempty boundary sandbox. Direct counts: zero reminders,
  executions, destinations, audits, idempotency, IAM receipts and deletion logs;
  all three data migrations remained. IAM configuration/session still worked,
  but create required reconfiguring the erased webhook. Main sandbox still had
  its three archived reminders.
- Prepared two aged archive rows, one expired and one with an hour of retention
  remaining, while the worker was stopped. Confirmed both rows existed. CLI get
  returned 404 for expired and 200 for retained before physical cleanup.
- Seeded exactly 100000 historical deletion-log fixtures. Restarted real worker:
  expired reminder disappeared, its full text/owner/cron/zone/archive/purge
  snapshot was added; ledger count stayed 100000, oldest identity advanced 1→2,
  newest became 100001. The unexpired reminder remained.
- Aged the empty isolation sandbox to 16 inactive days with worker stopped.
  Key access returned 401, metadata reflected the actual 15-day retirement
  instant, key rotation returned state conflict, explicit restore succeeded.
  Fixed delayed-sweep handling so inactivity retirement/recovery deadlines derive
  from last activity, not worker time. Aging it beyond 45 total days made restore
  return 404 before physical cleanup.
- `webhook disable` succeeded; subsequent `webhook get` returned the missing
  configuration error. Config URL and updater on/off commands were exercised
  and original settings restored. `update --check` returned unavailable: packages
  are not yet published, so a successful release installation remains unproven.
- Native readiness checks include the configured test control database. The
  Docker image built successfully; rebuild/runtime verification after final
  changes is still pending.


## Final live checks: sessions, isolation, delivery and boundaries

2026-09-05 23:32–23:55 UTC (2026-09-06 IST).

- Exercised an interrupted refresh against real IAM: persisted the pending
  operation key, consumed the refresh through the API, deliberately discarded
  the response, then ran `auth whoami`. The CLI replayed the pending operation,
  recovered its session and atomically cleared the key. This fixes the previous
  risk of losing a rotating refresh token after an uncertain response.
- Observer `auth logout` succeeded. Its saved old access token subsequently
  returned 401. A fresh observer login worked before removal.
- Manually signed an IAM test envelope with the configured real signing key.
  Accepted 202; exact replay returned the original receipt; tampered bytes
  returned 401; a newly signed changed payload with the same event ID returned
  409. Both linked active Remind replicas recorded the event independently;
  persisted projections did not contain the outer root-key wrapper.
- Removed disposable `remindobserver:tos` through real IAM with sandbox Carbon
  step-up. IAM showed membership removed, version 2; Remind live introspection
  immediately returned 401. A manually signed matching removal projection
  revoked the local identity, disabled its destination and archived its future
  reminder with no next trigger. These signed fixtures prove the receiver's
  behavior; they do not prove public upstream webhook dispatch.
- Started a second Remind worker, stopped the real local webhook process, and
  created a one-time reminder due at 23:42 UTC. Inspected one logical execution
  with transport failures and scheduled retries. Restarted webhook: attempt three
  succeeded. webhook verified history contained exactly one matching execution,
  with the original text and receipt `01a073f4-cc56-7053-990b-2485a5149411`.
  Stopped the extra worker afterward. This observed result does not promise
  exactly-once delivery under ambiguous network acceptance.
- The aged isolation environment was physically purged by the worker: neither
  metadata nor its schema remained after its 45-day total inactivity window.
- Created a second organization `remindother` in the same real IAM sandbox.
  Its Carbon SLT logged into the same Remind replica successfully. Archived list
  was empty; fetching a `tos` reminder returned 404; using that org-bound token
  with `--org tos` returned 401. Restored the main CLI session to the test Carbon
  in `tos` afterward.
- Manually followed `--limit 1` cursors for archived reminders and recurring
  execution history. The second pages contained the next distinct record with
  preserved filtering and order.
- Invalid environment UUID returned 422 `validation_failed` with JSON after
  correcting the path rejection mapping. A 1.1 MB login body returned 413
  `payload_too_large` with JSON after preserving body-limit rejections.
- DST fall-back probe: `30 1 1 11 *`, `America/New_York`, one-time, selected
  `2026-11-01T05:30:00Z`, the first repeated 01:30. Spring-gap probe:
  `30 2 14 3 *` skipped nonexistent 2027-03-14 02:30 and selected
  `2028-03-14T06:30:00Z`. Both probes were archived immediately after inspection.
- Explicit `update` and `update --check` both returned `unavailable` for these
  unpublished packages. Opt-out/persistence worked. Successful registry release
  installation is not proven; source/custom binaries now report an available
  version instead of replacing an unrelated Cargo installation.
- Final development regressions: `cargo test --workspace` passed 125 existing
  tests (115 core, 4 webhook adapter, 6 OpenAPI); formatting, strict all-target
  Clippy and OpenAPI validator passed. These regression checks are separate
  from the manual acceptance actions above.


## Final Docker runtime verification

Rebuilt `silicon-remind:manual-verification` from the final source. Started
`remind-manual-api-check` on loopback 8087 using protected environment-file
credentials and the dedicated manual databases. Container ran as UID/GID 10001,
with read-only root filesystem, all capabilities dropped and no-new-privileges.

- `GET /health/ready` without sandbox headers: 200, status ok. A sandbox selector
  on this global operational route is rejected 403, as intended.
- `GET /api/v1/auth/me` with the runner's real test session and Remind sandbox
  root key: 200, correct `remindrunner:tos` identity and member permissions.
- Stopped and removed only this temporary verification container afterward.
- Final source-secret scan found no configured protected values outside ignored
  files. `.env` is mode 0600 and ignored. Diff whitespace checks pass excluding
  the user's pre-existing whitespace in `UNDERSTANDING.md`.

Public deployment, IAM approval/dispatch to the public URL and installation of
an actual newer published CLI/client release remain unverified external steps.

## Current end-to-end regression: 2026-09-08

The documented local stack was started against the dedicated PostgreSQL test
database. CLI calls through the saved sandbox covered identity lookup, current
and archived listing, reminder creation, get, pause, resume, edit, archive,
Silicon discovery, execution history, and one-time scheduling. A one-time
occurrence was materialized by the worker and correctly entered `retrying` with
the expected transport failure while the local webhook process was unavailable.
The API and worker readiness endpoints returned 200, and the frontend tests,
TypeScript check, and production build passed.

This run exposed connection exhaustion after the worker rotated through the
saved sandbox fleet: the API returned `database_pool_timeout` and the worker
logged pool failures. The test-environment pool cache was reduced from 32 to 4
entries and each cached sandbox pool from four to two connections. Formatting,
strict Clippy, the complete 125-test workspace suite, Docker image build, and
Compose API/worker readiness checks passed after the fix. Successful webhook
delivery remains externally blocked by the unresolved webhook DNS recorded above.


## Public release follow-up

[Release 0.1.0](RELEASE_0.1.0.md) supersedes the earlier outstanding deployment,
webhook-approval and registry-installation notes. Public API/worker health,
Carbon/test Silicon authentication, sandbox creation, active IAM receiver,
processed signed-event receipt and a real crates.io CLI installation are verified.
Automatic replacement with a newer release remains a subsequent-release check.

## Resumed optional-webhook acceptance: 2026-09-08

The previous chat completed deployment but left the post-subscription-change
local delivery run and fresh hosted browser sign-in unverified. This follow-up
used fresh disposable local databases (`remind_qa`, `remind_testing_qa`) and a
new real IAM sandbox `01a080c0-25f9-7333-a805-04ccf25aa2ec`. Production data was
not used for reminder mutations. An old Compose volume had an outdated migration
checksum; it was copied separately and fresh databases were migrated instead.

- Public frontend and backend readiness returned HTTP 200 with normal DNS.
- In Chrome, **Continue with IAm** → **Continue to application** returned to
  `https://remind.teamofsilicons.com/#reminders` as the production Carbon in
  `tos`, with the correct read-only organization view and a clean callback URL.
- A newly created test Silicon authenticated through real IAM. Listing webhook
  subscriptions returned an empty list, but its first reminder creation returned
  `409 silicon_unavailable`. The local identity projection was only created when
  configuring a webhook, contradicting optional subscriptions.
- Fixed first reminder creation to register the authenticated Silicon's verified
  public identity independently of webhook configuration. Existing identity
  bindings and organization/revocation tombstones are preserved; schedule
  persistence still checks and locks active lifecycle state.
- Repeating creation without subscriptions returned 201. The worker processed
  the one-time occurrence at `2026-09-08T11:29:00Z` without outbound requests,
  recording execution `01a080c7-4f4d-7c50-9ff9-10736a1b5840` as delivered.
- Added two loopback receivers: unsigned HTTP 204 and HMAC-signed HTTP 200.
  A new one-time reminder fired at `2026-09-08T11:30:00Z`. Both received identical
  event bodies and execution/idempotency ID
  `01a080c8-39be-7641-b396-36c0c87b438d`, with the original Unicode text.
  The signed receiver verified the HMAC over the actual raw bytes; the unsigned
  receiver had no signature header. API history reported delivered without an
  error.
- Full Rust checks passed: 116 core tests (including the new first-reminder and
  revocation regression), 6 OpenAPI tests, and 5 webhook contract tests.
  Formatting, strict all-target/all-feature Clippy, and ARM64 Docker build passed.
  Strict Clippy also exposed a missing crate documentation comment in `build.rs`;
  the comment was added.

The fix was deployed after renewing the AWS session. The public sandbox
reproduced `409 silicon_unavailable` on the old image, then returned 201 for
the same authenticated Silicon with zero subscriptions on the new image. Its
one-time occurrence at `2026-09-08T11:44:00Z` was processed successfully as
execution `01a080d5-0982-7e73-8832-9a7afe2fc9a9`. Both backend containers and the
frontend gateway health checks passed. The disposable public Remind and IAM
sandboxes were retired afterward. Image and rollout evidence is recorded in
[the backend deployment follow-up](../deploy/aws/backend-2026-09-08-8912907.md).
