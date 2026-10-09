# Remind: migration to Silicon Accounts and Silicon Apps — progress log

Every stage appends a dated section: what it did, commits, test commands and results, what is left, gotchas.
Branch `migrate/accounts-apps-20261010`, worktree `.worktrees/remind-accounts-apps`.

## 2026-10-10 — Stage 1: service (Rust backend)

### Baseline (before any change, origin/main 88d1986)

Environment: `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`,
Postgres 16.11 on 127.0.0.1:5460 (no Docker).

- `cargo test --locked --workspace --all-targets --no-run`: builds.
- Docker-free suite, `cargo test --locked --workspace --all-targets -- --skip infrastructure::postgres::tests
  --skip infrastructure::testing::tests --skip infrastructure::testing::honeycomb_tests
  --skip infrastructure::identity_keys::tests --skip removed_tombstones_resolve_canonical_and_retained_memberships
  --skip infrastructure::reports::delivery_tests --skip telemetry::isolation_tests`: **150 passed, 0 failed**
  (silicon-remind lib 118, openapi_contract 6, webhook_client_contract 5, CLI unit 4, CLI discovery 13, client 4).
- The 24 database tests hard-required testcontainers (Docker). Commit 574e14b adds `src/test_support.rs`: with
  `REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres` every DB test gets its own
  `remind_t_<uuid>` database (dropped afterwards); without it the tests still start a container (CI path).
  With the harness, on the unchanged code: **24 passed, 0 failed** (postgres::tests 16, testing::tests 2,
  honeycomb_tests 2, identity_keys 1, internal 1, reports 1, telemetry 1). No leftover databases.

So at baseline every test passes (174 total).

### What the service stage did

- **Identity:** Silicon Accounts only (`silicon-accounts-client` 0.4.0; `silicon-iam-client` and `vendor/` removed).
  Bearer access tokens verified locally against a cached JWKS (forced refetch on an unknown `kid` at most every 30 s);
  introspection on the sensitive routes; sign-out cutoff per account; precise 401 codes.
- **No organizations:** `X-Org-ID`, org-scoped idempotency and IAM projections are gone. Visibility = owner Silicon,
  its custodian, the custodian's other Silicons, plus viewer grants; Silicon allow-lists guard grants to Silicons from
  outside the circle. Carbons read only. New routes `/viewers`, `/allowed-accounts`; `/auth/me` and `/silicons`
  reshaped.
- **Proofs:** `Authorization: Proof sap_…` on the five read routes, scope `remind.schedules.read`, issuers from
  `REMIND_PROOF_ISSUERS` (default deny).
- **Webhook:** `POST /webhook/` receives Silicon Accounts events (signed, deduplicated by `event_id`); sign-out,
  access removal, deletion, id/profile/custodian changes applied as in `decisions.md` §5.
- **Data:** migration 0010 (additive): `accounts`, `account_keys`, `identity_links`, `reminder_viewers`,
  `silicon_allowances`; `org_id` nullable; AAD v2 for new subscriptions; testing migration 0007 (environment owner).
  `remind-migrate link-identities --file mapping.csv [--dry-run]` re-keys IAM-era data without rewriting rows.
- **Contract 2:** `/api/v2` + `X-Remind-API-Version: 2`; `/api/v1` answers 410. `openapi.yaml` rewritten and
  validated; crate copies synced.
- **Profiles:** display name and photo come from Remind's user base, not lookups (found by the local E2E run).
- Docs for the Carbon: `docs/migration/decisions.md`, `cutover.md`, `understanding-proposal.md`; root
  `decisions.md` D-040 points to them.

### Commits

574e14b Run database tests against a provided PostgreSQL server ·
6731043 Move the service to Silicon Accounts identity, without organizations ·
d2edc42 Test the API end to end against a stub Silicon Accounts ·
07b4e77 Add remind-migrate link-identities for the cutover re-key ·
6425a73 Word retired-credential errors without the old service names ·
a3a5ab7 Describe API contract v2 in openapi.yaml ·
f4a3f2e List the Silicon Accounts settings in .env.example ·
9fe955b Read display names from Remind's user base, not from lookups ·
plus the docs commit that adds this section.

### Tests (final run, all with `REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres`)

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean.
- `cargo test --workspace --all-targets`: **all pass** — silicon-remind lib 150 (incl. 12 API tests against a
  wiremock Silicon Accounts with real Ed25519-signed tokens, 30+ PostgreSQL tests: migrations empty + upgrade path,
  link-identities, lifecycle events, visibility, bulk status, retention, testing environments), openapi_contract 7,
  webhook_client_contract 5, CLI unit 4, CLI discovery 13, client 4. No leftover `remind_t_%` databases.
- `python -m openapi_spec_validator openapi.yaml` (0.7.2, scratch venv): OK.
- E2E against the shared local stack (API on 127.0.0.1:4181, worker, a local delivery receiver on 4183, databases
  `remind_e2e*` on 5460, real tokens from `mint.mts`): 24/24 identity, visibility, proof, introspection and
  test-environment checks; a one-time reminder fired and was delivered signed with `silicon_uuid`; real webhook
  events (ping, `membership.signed_out` from refresh-token reuse → older tokens refused, `account.updated` →
  new display name); sharing by `c:` id through a real lookup by id, sibling visibility; `link-identities` with
  real lookups (wrong kind refused, dry run, apply, linked reminder visible to its Silicon and custodian). remind's
  webhook URL on the stack was pointed at 4181 for the run and restored to `http://127.0.0.1:9593/remind/webhooks`
  (same secret, `events: null`). All processes stopped, E2E databases dropped. Scripts:
  `<scratchpad>/remind/e2e/` (`env.sh`, `mint-all.sh`, `e2e.py`, `e2e_webhook.py`, `e2e_profile.py`,
  `e2e_share.py`).

### Left for later stages

- Client crate and CLI still speak `/api/v1` with IAM sessions (client/CLI stage): contract 2, Silicon Accounts
  sign-in, positional `remind login <slt>`, hidden `iam --json` alias, version 0.6.0.
- Web (Next.js + Arc UI), docs (`docs/*.md`, `API_DOCS.md`, `INTERNAL_API.md`, version policy, docs-site `iam.md`),
  dated IAM/Honeycomb records to `docs/history/`, packaging (`apps.yaml`, `honeycomb.yaml`, release workflow),
  deploy templates (`deploy/aws/*` still IAM settings).

### Gotchas

- `psql` is not on PATH: `/opt/homebrew/opt/postgresql@16/bin/psql -h 127.0.0.1 -p 5460 -U postgres`.
- The service loads `.env` from its working directory and every parent; run it from a directory with none (the
  E2E ran from `<scratchpad>/remind/e2e/run`).
- Silicon Accounts lookups never carry display name or photo; read `GET /v1/apps/remind/users/{uuid}`.
- A fired one-time reminder is `completed` and moves to the archived section; list with `section=archived`.
- The test stub's lookup answers now match the real service (no profile fields).

### Blocked on (outside this app)

- The Silicon Interface must switch to verification proofs at cutover (recorded in `cutover.md`).
- The Silicon runtime (`silicon connect`) keeps calling `remind login <SLT>`: the CLI stage must keep it.
- Production sign-in setup and webhook (with `custodian_change`) for `remind`: a Carbon's step, in `cutover.md`.
