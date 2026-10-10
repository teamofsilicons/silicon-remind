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
d30122c Record the Silicon Accounts migration decisions and cutover steps ·
9589d37 Keep the access token's scopes and sign-in family on the actor ·
and the commit that completes this list.

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
- `cargo deny --offline check licenses` fails only on the workspace's own `license = "Proprietary"` (unchanged from
  baseline; deny.toml has no private-crate exemption, and CI does not run cargo-deny). Bans and sources pass, and
  every third-party license, including `silicon-accounts-client`'s, is allowed.

### Blocked on (outside this app)

- The Silicon Interface must switch to verification proofs at cutover (recorded in `cutover.md`).
- The Silicon runtime (`silicon connect`) keeps calling `remind login <SLT>`: the CLI stage must keep it.
- Production sign-in setup and webhook (with `custodian_change`) for `remind`: a Carbon's step, in `cutover.md`.

## 2026-10-10 — Stage 2: client crate and CLI

Started from an interrupted earlier attempt at this stage that had left an uncommitted rewrite of
`crates/client` (reviewed, kept: it compiled, passed clippy and matched the service's contract 2 shapes) and half
of `crates/cli/src/args.rs` (replaced). Everything else in the CLI was written in this run.

### What this stage did

- **`silicon-remind-client` 0.6.0**: `accounts::SignIn` (device flow, SLT exchange as a public client, refresh,
  revoke; typed refusals from Silicon Accounts' own messages) and `Client` for `/api/v2` with an access token or a
  User verification proof (`Authorization: Proof`). Organization, IAM, server-side login and update code removed;
  sharing, allow-lists and visible-Silicon paging added. Errors keep `{error:{code,message,hint}}`.
- **`remind` 0.6.0**: `login` (device flow, `--open`, `--label`, `--force`), `login --slt`, `--slt-stdin`,
  positional `login <slt>`, `login status [--offline]` (`--json` always exits 0), `logout` (revokes at Silicon
  Accounts), `accounts --json` (offline, empty home), hidden `iam --json` alias, `whoami`, `share`, `allow`,
  `webhook list --silicon`, `config set-accounts-url`. Removed `--org`, `--account`, `auth`, `configure-iam`, IAM
  sandbox flags, `update`, `config auto-update`, the updater daemon (hidden `daemon uninstall|status` remain).
- **State** schema 2 in `{home}/.remind/state.json` (0600, atomic), lock only around writes and refreshes,
  single-flight refresh under 60 s left, one retry after a 401, 0.5 state archived, corrupted state moved aside.
- **Manuals**: new `docs/accounts.md`; rewritten CLI, client, API (contract 2), testing, webhook and release guides
  and both crate READMEs; `scripts/sync-package-docs.py --check`; docs-site navigation `iam.md` → `accounts.md`.
- **Records**: `decisions.md` §11–13, `understanding-proposal.md` (CLI paragraphs), `cutover.md` §5.

### Commits

e44747d Sign the client and CLI in with Silicon Accounts and speak API contract 2 ·
eea91bb Rewrite the client and CLI manuals for Silicon Accounts and Silicon Apps ·
151bca6 Group the global flags in help and use the real device link format ·
1c1b50a Sign in for production unless --test names a test environment ·
c2f8bb7 Report the production fallback consistently after signing in ·
64b65d9 Record the client and CLI stage: decisions, cutover, proposal, progress ·
2ba6965 Keep the previous identity service's name out of user-visible text (the 0.5 state archive is
`state.legacy-<time>.json`) · and the commit that completes this list.

### Tests

All with `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`.

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`: clean.
- `REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace --all-targets`:
  **217 passed, 0 failed** — service lib 150, openapi_contract 7, webhook_client_contract 5; CLI unit 16 (state:
  permissions, atomic writes, lock under 8 threads, legacy and corrupted files; arguments; output), CLI discovery 9
  (golden `accounts --json`, empty-home contract, home selection, 0.5 state, broken state, retired commands, footer),
  CLI session 10 (refresh rotation, single-flight across 3 processes, 401 retry, uncurable 401, ended sign-in,
  status verified/offline/down, logout, logout offline, test-environment fallback), CLI sign-in 11 (device flow
  pending → slow_down → approved with timing, denied, server-expired, locally expired, device flow off; SLT via
  stdin/flag/positional never echoed or stored; every `invalid_grant` reason; non-`slt_` token never sent; Remind
  refusing a new token → revoked and nothing saved; replacing revokes the previous; Remind down → saved unverified;
  selected environment signs in for production), client 4 + 5 (contract 2 headers and paths, proofs, error bodies,
  origins, typed sign-in failures). No leftover `remind_t_%` databases.
- `cargo test -p silicon-remind-client --doc`: 1 passed.
- `python3 scripts/sync-package-docs.py --check`: clean. `npm run build --prefix docs-site` (30 pages) and
  `npm run check --prefix docs-site` ("Verified 30 pages and 864 local links/assets"): pass.
- `cargo package --list` for both crates: docs, README, LICENSE, openapi.yaml and sources only.
  `cargo package -p silicon-remind-client --allow-dirty --offline`: packages and builds standalone.
- `~/.apps/bin/silicon-apps validate <staged dir> --json` (apps.yaml for macos-aarch64 + this `remind`, local only):
  `"valid": true, "errors": []`.

### End to end, against the shared stack and the migrated service

Service: `remind-migrate` then `remind-api` on 127.0.0.1:4181 (databases `remind_clie2e`, `remind_clie2e_testing`
on 5460; env `<scratchpad>/remind/cli-e2e/env.sh`, which sources the service stage's settings), Accounts at
http://localhost:9590. Scripts and full transcripts: `<scratchpad>/remind/cli-e2e/{silicon,carbon,discovery}.sh`,
`out/*-transcript.txt`. Identities `si:remind-cli-s1-1010c` (custodian `c:remind-cli-c1-1010c`) and
`c:remind-cli-c2-1010c`. Every `remind` ran with `env -i` and its own `SILICON_HOME`.

Silicon (`silicon.sh`):
```
$ printf %s $SLT | remind login --slt-stdin --json
{"authenticated":true,"can_manage_reminders":true,"custodian":{"id":"c:remind-cli-c1-1010c","uuid":"9Qc"},
 "id":"si:remind-cli-s1-1010c","kind":"silicon","method":"slt","uuid":"L9V","verified":true,…}   exit=0
$ (same token again)          {"error":{"code":"slt_already_used","message":"The short-lived token was already used; each one works once. Get a new one.","hint":"… `silicon-accounts login --app remind -q` …"}}   exit=3
$ remind login $SLT_FOR_BRIEFCASE   {"error":{"code":"slt_wrong_app","hint":"It was minted for 'briefcase'. …"}}   exit=3
$ remind login status --json  → authenticated true, verified true             exit=0
$ remind webhook subscribe http://127.0.0.1:4183/hook --unsigned → {"silicon_uuid":"L9V","version":1,…}
$ remind create … --timezone Asia/Kolkata → next_run_at 2026-10-12T03:30:00Z; list, get, pause (next_run_at null),
  resume, edit, share add c:remind-cli-c2-1010c (201 grant), share list, silicons (relation self)   all exit=0
$ (access token forced to expire in 5 s) remind list → refreshed at Silicon Accounts: sar_wqmy… → sar_2mtH…, 1800 s
$ remind login $SLT (positional) → signed in again, previous refresh token revoked   exit=0
$ remind iam --json → identical to accounts --json;  ls ~/.remind → state.json and state.lock, both -rw-------
$ remind logout --json → {"revoked":true,"signed_out":true,…};  login status → {"authenticated":false};
  list → not_signed_in, exit=3
```
(The transcript's token mask also hid the two error codes above as `slt_<masked>`; the mask was tightened
afterwards.)

Carbon (`carbon.sh`):
```
$ remind login --json --label 'cli e2e carbon' &   stderr: {"event":"device_code","user_code":"F6Z2-5VDW",
  "verification_uri":"http://localhost:9590/device","verification_uri_complete":"http://localhost:9590/device?code=F6Z2-5VDW","interval":5,…}
$ mint.mts approve --email remind-cli-c1-1010c@example.test --code F6Z2-5VDW → 204
  remind login finished: exit=0 {"authenticated":true,"kind":"carbon","method":"device","verified":true,"visible_silicons":1,…}
$ remind login --json → "already_signed_in":true;  silicons → relation "custodian", reminder_count 1
$ remind list --silicon si:remind-cli-s1-1010c → the Silicon's reminder;  executions; webhook list --silicon (read-only)
$ remind pause <id> / create … → 403 silicon_only, exit=4
$ remind env create … → owned by c:remind-cli-c1-1010c; --test <env> test-info / list (footer names it);
  env use → footer "· back to production: remind env exit"; env exit; env delete (recovery_days 30)
$ (second Carbon, device flow) silicons → relation "shared"; get <id> → the shared reminder; both logout revoked:true
```

Discovery (`discovery.sh`, empty `HOME` and `SILICON_HOME`, `env -i`): `--help` exit 0 (105 lines),
`accounts --json` exit 0 (`"app_id":"remind"`), `login status --json` → `{"authenticated":false}` exit 0,
`login status` exit 1, `iam --json` exit 0; `find $HOME` shows nothing written.

The stack's `remind` sign-in setup and webhook were not changed (device flow and public client were already on).

### Left for later stages

- Packaging stage: `apps.yaml`, `scripts/package-apps.sh`, `release.yml`; keep `docs/releases.md` in step with what it
  builds (it describes the brief's convention). Exclude `docs/migration/` (and history) from the docs-site build:
  today it renders them.
- Documentation stage: `docs/iam.md`, `docs/honeycomb-lifecycle.md`, `docs/README.md`, `README.md`,
  `docs/internal-api.md`, `docs/version-policy.md`, `docs/browser.md`, `docs/diagnostics.md`, `docs/deployment.md`
  and the dated records still describe the previous identity service and Honeycomb; `docs/install.sh` too.
- Web stage: the CLI's state and sign-in rules are in `decisions.md` §12 for parity.

### Gotchas

- `silicon-accounts-client` 0.4.0 has no public-client SLT exchange; see `decisions.md` 11.2.
- The real `verification_uri_complete` is `/device?code=…`.
- Hidden retired commands take every following word, so `--json` must come before them
  (`remind --json auth …`).
- The global `--url`/`--accounts-url` show up in clap's usage line when set through the environment; harmless.
- `cargo package -p silicon-remind-cli` cannot verify until `silicon-remind-client` 0.6.0 is on crates.io.

### Blocked on (outside this app)

- stemcell's `silicon connect` must mint an Accounts SLT (`silicon-accounts login --app remind -q`) for
  `remind login <SLT>`; the CLI refuses `oac_…` tokens locally with a hint (`cutover.md` §5).
- Production sign-in setup needs `device_flow` and `public_client` (`cutover.md` §5).
