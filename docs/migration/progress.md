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

## 2026-10-10 — Stage 3: packaging, CI, deployment configuration and documentation

### What this stage did

- **Packaging** (`decisions.md` §14): `packaging/apps.yaml.in` and `scripts/package-apps.sh <version> <target>
  <binary>` (wrapper of `scripts/package_apps.py`) write `dist/apps/remind-<version>-<target>.tar.gz` and `.sha256`,
  one target per archive. They refuse a binary for another OS/CPU, a Linux binary needing a glibc newer than 2.28,
  wrong answers to the three discovery commands (or `--version` differing from `apps.yaml`, or files left in the empty
  home), then run `silicon-apps validate`, `pack`, a byte check of the archive and `validate` on the archive.
  `--check-only` makes the checks without the packer. `scripts/build-release.py` builds and packs the six targets on
  one Mac. `honeycomb.yaml`, `scripts/package-release.py` and `scripts/build-honeycomb-release.py` are gone.
- **CI** (§15): `release.yml` keeps the six native builds and tests, checks each binary on its own runner
  (`--check-only`, discovery required), packs every target once on Linux with `silicon-apps-cli@0.2.0`, uploads
  `remind-silicon-apps-release` with `SHA256SUMS`; tag = CLI version; nothing published. `ci.yml`: PostgreSQL 17
  service for the database tests, client doctest, `sync-package-docs.py --check`, packaging and deploy script checks;
  the SolidJS web keeps its own job until the web stage. `deployment-builds.yml` builds the web image from
  `web/Dockerfile` and refuses the SolidJS one.
- **Deployment configuration** (§16): `standalone.yaml` (live) and `production.yaml` (alternative) read
  `REMIND_APP_SECRET`/`REMIND_ACCOUNTS_WEBHOOK_SECRET`, set `ACCOUNTS_URL`, `REMIND_APP_ID`, `REMIND_PROOF_ISSUERS`;
  no IAM, internal-token or Honeycomb setting, no `/internal/honeycomb/*` route, ALB on `/api/*`. `bootstrap-task.py`
  publishes the new keys. `deploy-web.py` + `install-web.sh` install the Next.js web as `remind-web.service`
  (contract in §16.2). `README-standalone.md` rewritten; the receipt inspector shows `source`.
- **Cutover**: `docs/migration/cutover.md` is now one ordered runbook (order relative to other apps, preparation,
  the window step by step with commands marked "run at cutover", checks, rollback, Silicons on the Honeycomb CLI,
  retirement), keeping every fact the service and CLI stages recorded. New helper
  `scripts/suggest-identity-links.py` turns a `link-identities` dry run into a mapping for review. `remind-migrate` now
  logs to standard error so its JSON report can be redirected (it was mixed with production's JSON log lines).
- **Documentation** (§17): README, `API_DOCS.md`, `INTERNAL_API.md`, docs home, website guide, version policy,
  diagnostics, deployment guide, release guide, `docs/internal-api.md` (service-only routes and operator commands),
  `docs/install.sh` (Silicon Apps); docs-site reads the contract from `openapi.yaml`, has its own favicon (the old one
  was the IAM mark), skips `docs/history/` and `docs/migration/`. 21 dated records moved to `docs/history/` with an
  index; their outbound links point at `88d1986`. Removed `scripts/test-timezone-e2e.py` (IAM fixture) and
  `scripts/backfill-iam-identities.py` (IAM import). `understanding-proposal.md` gained the testing, login and release
  paragraphs; root `decisions.md` D-041.

### Commits

8e247f1 Package the CLI for Silicon Apps instead of Honeycomb ·
e5ce3be Move the IAM and Honeycomb era records to docs/history ·
f7c090d Configure the production deployment for Silicon Accounts ·
ed81753 Build Silicon Apps archives in the release workflow ·
8b96c3e Rewrite the manuals for Silicon Accounts and Silicon Apps ·
a1277a1 Suggest the cutover identity mapping from a link-identities dry run ·
6901ed6 Check every deploy and packaging script's syntax in CI ·
d52b849 Explain the retired contract without naming account groupings ·
987b335 Record the packaging stage: decisions, cutover runbook, proposal, progress ·
755eb91 Note that the web, not Caddy, sets the Content-Security-Policy ·
and the commit that completes this list.

### Tests and proofs

All Rust commands with `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=3`.

- `cargo fmt --all -- --check`: clean. `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`:
  clean (forced recheck of the changed files).
- `REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace --all-targets`:
  **217 passed, 0 failed** (service lib 150, openapi_contract 7, webhook_client_contract 5, CLI 16 + 9 + 10 + 11,
  client 4 + 5). `cargo test -p silicon-remind-client --doc`: 1 passed. No leftover `remind_t_%` databases.
- `python3 scripts/sync-package-docs.py --check`: clean. `npm run build --prefix docs-site`: 13 pages;
  `npm run check --prefix docs-site`: "Verified 13 pages and 445 local links/assets". In `docs/history/`, 35
  relative links checked, all resolve except one that was already broken before the move (kept as it was); 15 links
  point at `88d1986`.
- Packaging on this Mac: `cargo build --release -p silicon-remind-cli`, then
  `scripts/package-apps.sh 0.6.0 macos-aarch64 target/mig/release/remind` → archive with exactly `apps.yaml` and
  `bin/remind`; `silicon-apps validate` on the extracted directory and on the archive: `"valid": true, "errors": []`;
  from the extracted archive with `env -i HOME=<empty> SILICON_HOME=<empty>`: `remind --help` exit 0 (5.6 KB),
  `remind accounts --json` exit 0 (`"app_id":"remind"`), `remind login status --json` exit 0
  (`{"authenticated":false}`), hidden `remind iam --json` the same object; the home stayed empty. Packing the same
  binary twice gave the same sha256. The bundled manuals (`remind docs <topic>`, 7 topics) and `--help` contain no
  retired names.
- Refusals: wrong version, pre-release version, unknown target, Mach-O for linux-x86_64, arm64 Mach-O for
  macos-x86_64, a native binary answering `{"app_id":"other"}`, a binary writing `~/.remind/state.json` during
  discovery: each refused with its reason, nothing written. `PACKAGE_DISCOVERY=require` for linux-aarch64 on a Mac:
  refused.
- Linux: `cargo zigbuild --release --target {aarch64,x86_64}-unknown-linux-gnu.2.28` (zig 0.15.2, cargo-zigbuild
  0.23.4) → newest `GLIBC_2.28` (the `.gnu.version_r` parser agrees with a byte scan) → packed and valid; an
  `x86_64-unknown-linux-gnu.2.39` build needs `GLIBC_2.39` → refused with the zigbuild hint.
- `python3 scripts/build-release.py --jobs 3` (all six targets: Xcode, zigbuild, cargo-xwin with the cached MSVC SDK)
  → six valid archives and `SHA256SUMS`; the Linux archives reproduced the earlier hashes byte for byte; macos-x86_64
  discovery was skipped (no Rosetta), macos-aarch64 discovery passed.
- Workflows: PyYAML parses all three; actionlint 1.7.12 with shellcheck and pyflakes: clean. Templates: cfn-lint
  1.57.2 on `standalone.yaml` and `production.yaml`: clean. shellcheck on `package-apps.sh`, `install-web.sh`,
  `install.sh`: clean.
- `standalone.yaml`'s embedded bootstrap run against sample secrets (paths redirected): writes the new keys and
  defaults, drops IAM/Honeycomb/unknown keys, refuses an IAM-shaped secret. `bootstrap-task.py` `main()` with a stub
  boto3: publishes exactly `REMIND_APP_SECRET`, `REMIND_ACCOUNTS_WEBHOOK_SECRET`, `REMIND_ENCRYPTION_KEYRING` and the
  two URLs; refuses a missing key. `test_bootstrap_grants.py`: 2 passed. `install-web.sh`'s embedded steps: first run
  writes `web.env` (0600, secret file removed), a rerun keeps `SESSION_SECRET` and operator values and refreshes
  `APP_SECRET`, a missing secret is refused; the Caddyfile rewrite replaces the old vhost (other vhosts untouched) or
  appends it. `docs/install.sh`: missing `silicon-apps` → instructions, exit 1; a stub in `~/.apps/bin` → called with
  `install remind`. The receipt inspector's query runs on a migrated schema and shows `source`.
- Cutover re-key path against the shared stack (`<scratchpad>/remind-ship-links-e2e.sh`, database
  `remind_ship_links`, dropped after): IAM-era rows for `si:remind-cli-s1-1010c` (exists) and
  `si:remind-ship-gone-1010` (nobody) → `link-identities --file /dev/null --dry-run` lists both as unmatched →
  `suggest-identity-links.py` writes `si:remind-cli-s1-1010c,L9V` and a `# REVIEW` for the other → dry run with
  online checks: linked 1, refused [] → apply: committed → rerun: unchanged 1, the unknown one still unmatched. The
  helper also finds the report behind a production-style JSON log line, and refuses wrong or missing credentials.
- CI's script step run locally as written: `py_compile`, `bash -n` per script, `package-apps.sh --help`: ok.

### Sweep (`git grep -n -i -E 'iam|honeycomb|org_id|organi[sz]ation|\borg\b|tenant'`, outside `docs/history/`)

Every remaining hit is intentional:

- `UNDERSTANDING.md`: Carbon-only contract; proposed wording is in `understanding-proposal.md`.
- Root `decisions.md`: append-only history; D-040 and D-041 point to `docs/migration/decisions.md`.
- `docs/migration/*`: the migration record, allowed to name what changed.
- `migrations/0001`–`0009`, `testing/migrations/0001`–`0006`: applied migrations; their checksums must not change.
- `migrations/0010_accounts_identity.sql`, `testing/migrations/0007`, `src/infrastructure/identity_links.rs`,
  repository/lifecycle/retention/delivery/crypto code and their tests: IAM-era rows, the mapping table
  (`iam_principal_id`), nullable `org_id` and AAD version 1, which old rows still use.
- `src/config.rs`: the list of retired variables that are ignored with a warning.
- `src/api/tests/*`, `tests/openapi_contract.rs`, `crates/cli/src/args.rs`, `crates/cli/tests/discovery.rs`: tests
  asserting the old names are refused or absent from help; `crates/cli/src/{args,commands,main,state}.rs`: the hidden
  `iam --json` alias, retired-command answers and the 0.5 state reader (brief: transition aliases).
- Code comments saying "tenant" (`src/metrics.rs`, `src/api/middleware.rs`, `src/api/handlers/health.rs`,
  `src/infrastructure/postgres/error.rs`) and a test URL `?tenant=one` (`src/infrastructure/webhook.rs`): generic
  wording, not an account grouping, never shown to anyone.
- `scripts/sync-package-docs.py`: names the retired manual copies it deletes. `scripts/suggest-identity-links.py`,
  `docs/internal-api.md`, `src/bin/remind_migrate.rs`: the mapping file's literal column `iam_principal_id`.
- `deploy/aws/standalone.yaml`, `production.yaml`, `deploy-standalone.sh`: AWS IAM resources
  (`AWS::IAM::Role`, `CAPABILITY_NAMED_IAM`), not Silicon IAM.
- `space-windows/README.md`: Space Station's own organization `tos`, where the windows are published.
- `frontend/**`: the SolidJS web and its IAM popup, replaced and deleted by the web stages (only its README link to a
  moved record was updated).

### Left for later stages

- **Web stages**: build `web/Dockerfile` to the runtime contract in `decisions.md` §16.2 (Next.js standalone,
  `WORKDIR /app`, `$PORT` 3000, non-root, the kit's variables; `APP_API_URL=http://remind-api:8080/api/v2`, so the
  browser calls the OpenAPI paths) or change `deploy/aws/install-web.sh` with it; `deployment-builds.yml` already
  builds `web/Dockerfile`. Replace the `web` job in `ci.yml`. Keep `docs/browser.md` in step with the screens
  (it promises the telemetry preference, sharing and test environments); the web's mark may replace
  `docs-site/favicon.svg`. Production redirect URI: `https://remind.teamofsilicons.com/auth/callback`.
- **E2E stage**: `scripts/package-apps.sh` (or `build-release.py --package-only` over `target/apps-release/`, kept
  from this run) makes the archive for "discovery commands from a packaged archive".

### Gotchas

- `bash -n a b c` checks only `a`; the CI step loops over the scripts.
- `remind-migrate` logs to standard error now; its standard output is the report alone.
- `silicon-apps validate`/`pack` are local in 0.2.0, but run them with `--home <empty dir>` and without `APPS_TOKEN`:
  the CLI on this Mac is signed in to production.
- This Mac has no Rosetta: macos-x86_64 binaries cannot run here (the packager says so and skips discovery).
- `/dev/shm` does not exist on macOS; the runbook pipes secrets between processes instead of writing files.
- The live Caddyfile is bind-mounted: rewrite it in place (keep the inode) or recreate the Caddy container.

### Blocked on (outside this app)

Nothing new. Still outside Remind: the Silicon Interface's switch to proofs and stemcell's `silicon connect` change
(both in `cutover.md` §0 and §2.9), and the production Silicon Accounts and Silicon Apps steps a Carbon runs.

## 2026-10-10 — Stage 4: end to end against Silicon Accounts

### What this stage did

- **`scripts/dev-accounts.sh` / `scripts/dev-accounts-stop.sh`** (`scripts/dev_accounts.py up|down|restart|status`):
  idempotent local stack against the shared Silicon Accounts stack: databases `remind_e2e` and `remind_e2e_testing`
  on 5460 (created, migrated), a delivery receiver on 4183, `remind-api` on 4181, `remind-worker` on 4182, Remind's
  app webhook pointed at `http://127.0.0.1:4181/webhook/` (every update; the previous URL saved and given back by
  `down`), proven by a test ping after every (re)start. Pids in `.mig/pids`, everything else in `.mig/dev-accounts`
  (now in `.gitignore`). Decisions §18.1–18.4.
- **`scripts/e2e-accounts.sh`** (`scripts/e2e_accounts.py`): the eight scenarios of the stage plus every Silicon
  Accounts event, suspension, concurrent refresh and the Carbon's refresh (decisions §18.5–18.10). 131 checks; each
  scenario also runs alone (`--only N`); transcripts mask tokens, STKs and test keys; a run signs its CLI test homes
  out when it ends (the 23 homes earlier runs of this stage left signed in were signed out by hand).
- **Bug found and fixed** (decisions §19.1): an account first stored from a lookup (shared with by id, named in a
  `--silicon` filter) stayed without name and photo for up to 15 minutes after signing in. Its first token now forces
  a re-read. Regression test `an_account_first_looked_up_gets_its_profile_when_it_signs_in`.
- README ("Run against a local Silicon Accounts stack"), CI script checks, the cutover runbook's pre-flight
  (`cutover.md` §1.4: run the suite on the release commit), decisions §18–19.

### Commits

797c378 Read an account's profile at its first sign-in after a lookup ·
35c8c71 Run Remind and its end-to-end checks against a local Silicon Accounts ·
9383ab7 Check refreshes and suspension end to end ·
5f8be15 Keep error codes readable in end-to-end transcripts ·
7b4c30b Record the end-to-end stage: decisions, cutover pre-flight, progress ·
a4af827 Sign the end-to-end test homes out when a run ends ·
and the commit that completes this list.

### Tests

All Rust commands with `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=3`.

- `cargo test -p silicon-remind --lib an_account_first_looked_up` before the fix: **FAILED**
  (`left: (200, Some(""), Some(""))`, `right: (200, Some("Zed Shaw"), Some("https://example.test/pfp.png"))`); after
  it: 1 passed.
- `cargo fmt --all -- --check`: clean. `cargo clippy --locked --workspace --all-targets --all-features -- -D
  warnings`: clean.
- `REMIND_TEST_POSTGRES_URL=postgres://postgres@127.0.0.1:5460/postgres cargo test --locked --workspace
  --all-targets`: **218 passed, 0 failed** (service lib 151 with the new test, openapi_contract 7,
  webhook_client_contract 5, CLI 16 + 9 + 10 + 11, client 4 + 5).
- Scripts: `python3 -m py_compile scripts/*.py deploy/aws/*.py`, `bash -n` on every script CI lists, both `--help`s:
  ok; parse on Python 3.9.6 too. shellcheck 0.11.0 on the three wrappers, pyflakes on both scripts, actionlint 1.7.7
  (with shellcheck and pyflakes) on `ci.yml`: clean.
- `scripts/dev-accounts.sh` twice: the second run started nothing and sent no ping (`"webhook": "already pointed
  here"`). A restart with `REMIND_DEV_PROOF_ISSUERS='not a pair'` failed in 3.6 s with the service's own error in the
  message (`invalid environment variable REMIND_PROOF_ISSUERS: …`); `up` recovered and its ping was delivered.
- `scripts/e2e-accounts.sh --only 3`, `--only 4`, `--only 5`, `--only 6` (each alone, its own accounts): 7, 22, 35
  and 15 passed, 0 failed.
- **Final run, from nothing** (`scripts/dev-accounts-stop.sh --drop`, then `REMIND_TEST_STACK=<scratch>/test-stack.json
  scripts/e2e-accounts.sh`, run `101010125c2`): **131 passed, 0 failed in 147 s**: 1 Carbon on the API 15, 2 Silicon
  on the CLI 20, 3 device flow 6, 4 circle and sharing 19, 5 webhooks 45, 6 proofs 13, 7 discovery 7, 8 restart 6.
  Afterwards no process left, no pid file, the webhook back at `http://127.0.0.1:9593/remind/webhooks`, no token
  pattern in the transcript (`grep -cE 'slt_…{20,}|sar_…|eyJ…|stk-[0-9a-f]{8,}|whsec_…'` = 0).
- Silicon Accounts' own record of the final run (`GET /v1/apps/remind/webhook/deliveries`): 9 deliveries, every one
  `delivered` with 200: `account.deleted`, `account.id_changed`, `account.updated`, `silicon.custodian_changed`,
  `membership.access_removed` ×2, `membership.signed_out` ×2 (`app_revoked`, `stk_rotated`), `ping`; none failed or
  pending, in that run or any earlier one. The API and worker logs had no warning or error.

### What the final run showed (trimmed from `.mig/e2e/101010125c2/transcript.txt`)

1. Carbon on the API (`mint.mts app-signin --app remind --redirect http://localhost:4180/auth/callback --exchange`):
   ```
   GET /api/v2/auth/me → 200 {"kind":"carbon","can_manage_reminders":false,"credential":"access_token",…}
   POST /api/v2/schedules → 403 silicon_only
   POST /api/v2/test-environments → 201 {"environment":{"owner":{"id":"c:remind-e2e-c1-…","kind":"carbon"},…},"key":"<test key>"}
   GET list → listed · GET /{id} → read · key-rotations → new key; old key refused · cleanings (key only) → 204
   GET /{id} as another Carbon → 404 · DELETE → 204; not listed · restorations → new key · DELETE → 204
   ```
2. Silicon on the CLI (fresh `SILICON_HOME`, `env` = PATH, HOME, SILICON_HOME, REMIND_URL, ACCOUNTS_URL only):
   ```
   remind login --slt-stdin --json → {"authenticated":true,"kind":"silicon","method":"slt","verified":true,…} exit 0
   (same token) → {"error":{"code":"slt_already_used",…}} exit 3
   webhook subscribe http://127.0.0.1:4183/hook --secret-stdin; create ×3 (recurring, one-time due 04:39 UTC, the
   shared one); list; get; edit; pause; resume; archive → list --archived        all exit 0
   remind logout --json → {"revoked":true,"signed_out":true,…}; login status --json → {"authenticated":false} exit 0
   the Silicon's other sign-in still works after that logout (app_revoked ignored) · remind login slt_… → signed in
   4 × remind list at once with a token expiring in 5 s → all exit 0, token rotated; login status → verified
   ```
3. Device flow: `remind login --json` printed `{"event":"device_code","user_code":"…","verification_uri":
   "http://localhost:9590/device",…}`; `mint.mts approve` → `{"authenticated":true,"kind":"carbon","method":"device",
   "verified":true,…}`; `remind silicons` → relation `custodian`; `list --silicon`; `pause` → exit 4 `silicon_only`;
   the device sign-in refreshed when its token ran out.
4. Circle and sharing: custodian and sibling read (`/silicons` relations `custodian`, `sibling`, `self`); the sibling
   cannot edit; the unrelated Carbon gets 404 and an empty list until `remind share add c:…` (then relation
   `shared`), 404 again after `share remove`; the custodian's `POST /viewers` / `DELETE /viewers/{id}?silicon_id=`
   work the same; `remind share add si:<another custodian's Silicon>` → exit 4 `silicon_not_open`; after that Silicon's
   `POST /allowed-accounts` the share works; its custodian's `DELETE /allowed-accounts/…` ends the grant (404); the
   outside Silicon shows its name and photo to its custodian (the fixed bug).
5. Webhooks: the one-time reminder arrived at 04:39 signed (`webhook-signature: v1,…` verified) with `silicon_uuid`;
   the suspended Silicon got nothing during a minute, then its overdue occurrence right after signing in again;
   custodian's `POST /v1/me/silicons/{uuid}/id` → reminders, `/auth/me` (old token) and `remind login status` show
   the new id; a real replay through Silicon Accounts → delivered 200, one receipt, `duplicate` logged; transfer →
   new custodian sees it, old one and the former sibling do not; forged / unsigned / 10-minute-old / tampered →
   `401 webhook_signature_invalid` (nothing recorded); unsigned-event body → `400 webhook_body_invalid`; genuine
   delivery `ignored`, same `event_id` again `duplicate`; `silicon-accounts login --silicon … && apps remove remind`
   → old token `401 token_revoked` within 1.0 s, CLI exit 3 `sign_in_ended`, hidden from its custodian, back after
   signing in; display name change → shown; STK rotation → `token_revoked`, CLI ended, the new STK signs in;
   `DELETE /v1/me/silicons/{uuid}` → `401 account_deleted`, reminder archived (`purge_after` 45 days), subscription
   disabled.
6. Proofs (issuer `interface`, receiver Remind, scope `remind.schedules.read`): `/auth/me` → `"credential":"proof",
   "issuing_app":"interface"`; schedules, executions, silicons read; POST and `/viewers` → `401 proof_not_accepted`;
   `Bearer sap_…` → `401 proof_as_bearer`; scope `remind.other` → `403 proof_scope_missing`; `receiving_app:
   briefcase` → `401 proof_invalid`; revoked before use → `401 proof_invalid` at once; revoked after use → refused
   after 30 s; issuer `webkit` → `403 proof_issuer_not_allowed`.
7. Discovery: `cargo build --release -p silicon-remind-cli`; `scripts/package-apps.sh 0.6.0 macos-aarch64 …` →
   archive with exactly `apps.yaml`, `bin/remind`; from it, empty HOME and SILICON_HOME: `--help` exit 0,
   `accounts --json` exit 0 with `"app_id":"remind"`, `login status --json` → `{"authenticated":false}` exit 0,
   nothing written.
8. Restart (`dev_accounts.py restart`, new pids): the Silicon's CLI, the Carbon's token and the Carbon's CLI sign-in
   keep working; a delivery made before the restart → `duplicate`; a Silicon Accounts replay after it → delivered
   200, still one receipt.

### Left for later stages

- **Web stage**: the stack script reserves 4180 for the Next.js web; its BFF can run against `remind-api` on 4181
  (sign-in redirect `http://localhost:4180/auth/callback` is registered on the stack). Add the web to
  `dev_accounts.py` and a browser scenario to the suite when it exists.
- Nothing else in this stage's scope.

### Gotchas

- The stack allows 10 email codes per address in 10 minutes; the mint helper's hosted sign-in of a new Carbon spends
  3, `silicon`/`approve`/`carbon` one each. Make extra Silicons with the custodian's own session (decisions §18.6).
- Silicon Accounts sends an app events only about accounts that signed in to it: sign an account in to Remind before
  changing it if the check depends on the webhook.
- A new sign-in right after a sign-out signal must be a second newer (1.6); the suite waits 1.2 s.
- `dev_accounts.py up` does not restart a running service on a rebuilt binary: run `restart` (or `down`, `up`).
- The service's development log format is coloured unless `NO_COLOR` is set; the script sets it.

### Blocked on (outside this app)

Nothing new. Still outside Remind (`cutover.md` §0 and §2.9): the Silicon Interface's switch to proofs, stemcell's
`silicon connect` change, and the production Silicon Accounts and Silicon Apps steps a Carbon runs.
