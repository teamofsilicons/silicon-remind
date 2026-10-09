# Remind: decisions for the move to Silicon Accounts and Silicon Apps

The record of judgement calls made while moving Remind off Silicon IAM and Honeycomb, for the Carbon to review.
Each later stage (client and CLI, web, docs, packaging) appends its own section. This file supersedes the
IAM-era entries of the root `decisions.md` (D-004, D-005, D-013, D-020, D-021, D-025, D-030, D-033, D-035 and both
2026-09-08 entries) and amends D-026; the root log carries a pointer here (it is append-only).

Inputs: the brief (D1 to D9), `apps/remind.md`, `apps/_cross-app.md`, the survey `surveys/remind.json` and
`UNDERSTANDING.md` (unchanged; proposed wording is in `understanding-proposal.md`).

## Stage 1: service (2026-10-10)

### 1. Signing in

1.1 **One identity provider.** Every API request carries a Silicon Accounts credential. The service uses only the
official `silicon-accounts-client` 0.4.0; `silicon-iam-client` and its vendored copy are gone.

1.2 **Access tokens.** `Authorization: Bearer <access token>` issued to the app id `remind`: an EdDSA JWT verified
locally (signature, `aud` = `REMIND_APP_ID`, `iss` = `ACCOUNTS_URL` exactly, expiry, `kid`). The JWKS is cached for an
hour. A token naming an unknown `kid` forces a refetch at most once every 30 seconds (the cold fetch and hourly
refreshes do not count), so a flood of made-up key ids cannot hammer Silicon Accounts, while a key rotation is
picked up on the first token that uses the new key.

1.3 **Introspection only where it matters.** Routes that cannot be undone or that hand out secrets also ask Silicon
Accounts whether the token is still active (answer cached 30 seconds): archive a reminder, add or end webhook
subscriptions (`PUT`/`DELETE /webhook`, `POST /webhooks`, `DELETE /webhooks/{id}`), grant or end viewer grants,
change a Silicon's allow-list, read, rotate or restore a test environment key, and retire a test environment. Every
other route relies on the local check plus the sign-out cutoff (1.6).

1.4 **What the token must say.** `sub` must be a Silicon Accounts uuid (1 to 64 base62 characters, case-sensitive,
never parsed as an RFC 4122 UUID) and `kind` must be present; a token whose `kind` differs from the account Remind
already knows under that uuid is refused (`token_kind_mismatch`). A token issued to another app gets
`token_wrong_audience` with a hint to send a verification proof instead.

1.5 **Old credentials get precise errors.** `oat_`/`ort_` tokens get `401 legacy_token_rejected` ("sign in with
Silicon Accounts"); a `sap_` proof sent as Bearer gets `401 proof_as_bearer`. Every 401 carries
`WWW-Authenticate: Bearer realm="remind"`.

1.6 **Sign-out cutoff.** Remind keeps, per account, `revoked_before`: tokens whose `iat` is at or before that second
are refused with `token_revoked` (the account signs in again). It moves forward on `membership.signed_out`,
`membership.access_removed` and `account.deleted`. Second granularity is deliberate: JWT `iat` has no fractions, and a
token minted in the same second as a sign-out is treated as part of the ended sign-in.

1.7 **Profile data.** A lookup (`GET /v1/accounts/{uuid}`) only ever shows uuid, kind, id, status and a Silicon's
custodian. The display name and photo come from Remind's user base (`GET /v1/apps/remind/users/{uuid}`), which only
has accounts that signed in to Remind. Both are read in parallel when Remind first sees an account and again when its
copy is older than `REMIND_ACCOUNTS_LOOKUP_TTL_SECONDS` (default 900), synchronously inside that request, and on every
`account.updated`, `account.id_changed` and `silicon.custodian_changed`. A failed read keeps the cached copy. Reads
stay inside Remind's own budget of 500 a minute (Silicon Accounts allows 600 per app), and one failing read is not
repeated for 60 seconds. Accounts that never signed in to Remind (for example a custodian who only uses the account
site) are shown by id with an empty display name: that is Silicon Accounts' privacy model, not a gap.
*Found by the end-to-end run: the first version took the profile from lookups and blanked every display name.*

1.8 **Configuration.** `ACCOUNTS_URL` (public origin, also the token issuer; default
`https://accounts.teamofsilicons.com`), optional `ACCOUNTS_API_URL` (where the service calls the API, for split local
stacks), `REMIND_APP_ID` (default `remind`), `REMIND_APP_SECRET` (required; at least 32 bytes in production),
`REMIND_ACCOUNTS_WEBHOOK_SECRET` (`whsec_…`, comma-separated, up to 4 so a rotation can overlap),
`REMIND_ACCOUNTS_REQUEST_TIMEOUT_MS` (3000), `REMIND_ACCOUNTS_LOOKUP_TTL_SECONDS` (900) and `REMIND_PROOF_ISSUERS`.
Accounts URLs must be https; http is accepted only for this machine (localhost, 127.0.0.1, ::1) and never in
production. Retired `REMIND_IAM_*`, `REMIND_INTERNAL_API_TOKEN` and `REMIND_HONEYCOMB_*` variables are ignored with one
startup warning each, so an old environment file does not stop a deploy.

### 2. Verification proofs from other apps

2.1 The Silicon Interface reads reminders for an account with a User verification proof:
`Authorization: Proof sap_…`, verified with `POST /v1/proofs/verify` (cached 30 seconds, never past expiry).

2.2 Accepted only on `GET /schedules`, `GET /schedules/{id}`, `GET /schedules/{id}/executions`, `GET /silicons` and
`GET /auth/me`; anywhere else `401 proof_not_accepted`. A proof never writes, so a valid proof on a write route is
refused before any handler runs.

2.3 The proof must be valid, of kind `user_verification`, for receiving app `remind`, carry the scope
`remind.schedules.read`, and come from an issuer that `REMIND_PROOF_ISSUERS` allows for that scope (format
`scope=app,scope=app`; production value `remind.schedules.read=interface`). The default is empty, which denies every
proof. The caller then reads exactly what the named account (`user.uuid`) may read, nothing more.

2.4 Remind has no OBO endpoint ids from the IAM era, so the scope name follows the brief's `<app>.<resource>.<read>`
pattern. Remind issues no proofs: it calls no other app.

### 3. Teams to personal accounts: the mapping

| IAM-era concept | Now |
| --- | --- |
| Organization, `X-Org-ID`, org selection at login | Gone. A request is one account; nothing names a group. |
| "Any Carbon in the org views any Silicon's reminders" | A Carbon reads the reminders of the Silicons it is custodian of. |
| "Any Silicon lists other Silicons' reminders in its org" | A Silicon reads its own and its siblings' (other Silicons with the same custodian). |
| Collaboration across the org | Explicit viewer grants: the owner Silicon, or its custodian, lets any account read that Silicon's reminders. |
| Anyone in the org could see a Silicon | Silicons are not open to everyone: a grant to a Silicon outside the owner's circle needs that Silicon (or its custodian) to have allowed the owner or the granting custodian first (`/allowed-accounts`). Carbons can be granted view by anyone. |
| Carbons view only (contract) | Unchanged: Carbons never create, change, pause or archive reminders, and never subscribe webhooks. Only the owner Silicon does. |
| org_admin / org_owner powers on test environments | The owner's custodian (see 6). |
| Membership removed from the org | `membership.access_removed`: suspended until the account signs in again (see 5). |

- The circle is derived live from Silicon Accounts custodian data cached in `accounts` (refreshed on sign-in, by the
  TTL re-read and on `silicon.custodian_changed`). Only active accounts with a known kind are visible.
- `GET /silicons` lists the Silicons the caller can read with `relation` = `self | custodian | sibling | shared` and
  each one's retained reminder count.
- The custodian manages its Silicons' viewer grants and allow-lists and lists their webhook subscriptions (read-only;
  endpoint URLs are shown, signing secrets never are). It never acts as the Silicon.
- Viewer grants and allow-lists are about real accounts, so they are refused inside a test environment
  (`403 not_in_test_environment`).
- Removing an account from an allow-list also ends the grants to that Silicon that only the entry made possible
  (grants from outside its circle by the removed account).
- User-facing copy never says "circle"; it says "you and the Silicons you look after" or "your custodian and its other
  Silicons".

### 4. Data: keep everything, re-key by mapping

4.1 **Storage keys stay.** Every owner column (`owner_principal_id` on schedules, executions, destinations,
idempotency records, reports) keeps holding a Remind-private UUID. Migration `0010_accounts_identity.sql` adds
`accounts` (uuid, kind, id, profile, custodian, status, cutoff), `account_keys` (storage key → account uuid, origin
`accounts` or `identity_link`) and `identity_links` (IAM principal → Silicon Accounts uuid, with its source). An
account seen for the first time gets one new key; a linked IAM principal's old key is attached to its account. No data
row is ever rewritten, so a wrong link is corrected by linking again, and an empty uuid unlinks.

4.2 **`remind-migrate link-identities --file mapping.csv [--dry-run] [--source <label>]`** (operator command, run at
cutover). Each line is `iam_principal_id,accounts_uuid`: the principal is the Remind storage key or the `si:`/`c:` id its
IAM binding names; a header line, blank lines and `#` comments are skipped, and `-` or nothing after the comma unlinks.
With `REMIND_APP_SECRET` set, every uuid is checked against Silicon Accounts (it must exist, not be deleted, and have the
same kind as the IAM binding) and its current id and custodian are stored; without it the kind comes from the IAM
binding. All-or-nothing: one refused line rolls the whole run back. The JSON report lists linked (with counts of
schedules and destinations), unchanged, unlinked, refused, and unmatched (IAM-era principals that still own live data
and have no link). Legacy key-based test environments whose creator is linked get that account as owner. Proven on a
real run against the local stack (refusal of a wrong kind, dry run, apply, the linked reminder visible to its Silicon
and custodian).

4.3 **Unlinked IAM-era data keeps working, unseen.** Until linked, an old Silicon's reminders keep firing under the
IAM-era rules (its `silicon_identities` row and organization must still be active), and nobody can see or change them.
They appear in the report's `unmatched` list. This follows `apps/remind.md` ("keep delivering, invisible and immutable
until mapped").

4.4 **Columns.** `org_id` becomes nullable on the six tables that had it; old rows keep their value, new rows write
NULL. Idempotency is scoped to the account (`org_id IS NULL` rows), superseding D-013's organization scope.
`executions.schedule_id` now cascades on schedule deletion so purging cannot strand executions.

4.5 **Identity is global, data is per environment.** Accounts and their keys live in the production database even for
test-environment requests (Silicon Accounts has no test copies of accounts); reminders, executions and subscriptions of
a test environment live in its own schema, as before.

4.6 **Subscription secrets.** New subscriptions bind their ciphertext to
`remind-destination:v2 \0 destination id \0 owner key \0 field` (AAD version 2, stored per row). Old rows keep
version 1 (bound to the organization and Silicon id) and still decrypt; nothing is re-encrypted automatically, and key
rotation keeps each row's version. Amends D-026.

4.7 **Delivery payload.** `remind.schedule.triggered` gains `silicon_uuid`; `silicon_id` is the owner's current id
(the creation-time id only for a still-unlinked row). Consumers that key on `silicon_id` keep working.

4.8 **Deleted-reminders ledger.** New lines use record schema 1.1 and add `owner_uuid` and `owner_id`; older lines are
unchanged.

4.9 Migrations are additive and were proven on an empty database and on a database populated through migrations
0001 to 0009 (fingerprint of every pre-existing row unchanged after 0010).

### 5. Account lifecycle (Silicon Accounts webhook)

5.1 `POST /webhook/` (and `/webhook`) receives Silicon Accounts app webhooks, the same URL IAM used, so production only
re-registers it. The signature (`X-Accounts-Signature`, `X-Accounts-Timestamp`, five-minute tolerance) is checked over
the raw body with each configured secret. Missing, wrong or stale signature: `401 webhook_signature_invalid`. Signed
but malformed, or naming an unreadable uuid: `400 webhook_body_invalid`. Each `event_id` is stored in
`internal_event_receipts` and applies once (`duplicate` afterwards). Unknown types and `ping` are recorded and answered
`200 ignored`. A failure while applying answers 5xx so Silicon Accounts retries.

5.2 **Per event.**
- `membership.signed_out`: moves the cutoff to the event time, so every older token is refused; reminders keep
  firing (a sign-out is not a removal). Reason `app_revoked` is ignored: it is Remind itself ending one sign-in (a CLI
  logout on one machine), and the account's other sign-ins must keep working.
- `membership.access_removed`: cutoff, then the account is suspended: its reminders stop materialising and delivering
  and pending deliveries are failed. A newer sign-in reactivates it. A delayed notice older than the account's latest
  token is ignored (it signed in again after removing access).
- `account.deleted`: cutoff, anonymised (id, display name and photo cleared), reminders archived (45 days, then the
  deleted-reminders ledger), subscriptions disabled, viewer grants and allow-list entries by or for it revoked, its
  test environments retired. A deleted account is refused even with a newer token.
- `account.id_changed`, `account.updated`, `silicon.custodian_changed`: re-read the account (1.7). If Silicon Accounts
  cannot be read, apply the event's own data guarded so a late event never undoes a newer one (id and custodian by
  occurrence time, profile by `account.version`).

Remind holds no sessions, refresh tokens or proofs of its own (clients keep their refresh tokens), so a sign-out
has nothing else to end. The actor of an access-token request records the token's scopes and sign-in family (`fid`)
for later use; nothing depends on them yet.

5.3 The production webhook must pick `custodian_change` (not a default pick) besides the defaults (`id_change`,
`display_name_change`, `pfp_change`, `access_removed`, `account_deleted`), or use every update (`events: null`);
see `cutover.md`.

### 6. Test environments (Remind's own product feature, kept)

6.1 Owned by the account that creates it (`owner_uuid`). Readers: the owner's circle (list, read metadata, read the
key). Managers: the owner and, when the owner is a Silicon, its custodian (rotate the key,
restore, retire). Clean: anyone with the key. This maps the contract's creator / org_admin / org_owner roles.

6.2 Creation takes only `name` and `description`; the IAM test key and app secret fields answer
`422 test_key_field_retired`. Keys stay 32 alphanumeric characters, encrypted at rest, `Cache-Control: no-store`.

6.3 Inside a test environment the caller signs in with a real Silicon Accounts token and gets the contract's god view
(reads every reminder of the environment); writes still follow ownership. Limits (100 reminders, 15 days idle, 30-day
recovery) are unchanged.

6.4 Environments created by IAM or Honeycomb automation (`iam_control_version` set) are dormant: never listed, never
selectable, data kept. Legacy key-based environments created by an IAM principal become visible once that principal
is linked (4.2).

6.5 Removed: Honeycomb sandbox discovery, `/testing-environment/iam`, the internal provisioning API and its token.

### 7. API contract

7.1 **Contract 2 at `/api/v2`.** `docs/version-policy.md` says "changing required inputs, removing fields or
endpoints, or changing permission semantics requires a new major wire contract", and the registry
(`api_contract_versions`) selects the contract by path plus the optional `X-Remind-API-Version` header. The identity
change does all three, so the survey's recommendation was followed: `/api/v2` with `X-Remind-API-Version: 2` is the
only contract. Deviation from the policy's deprecate-then-idle-sunset path: v1 cannot keep running, because its
authentication (IAM tokens and organizations) no longer exists after cutover, so migration 0010 registers 2 as active
and records 1 as sunset at once, and every `/api/v1` path answers `410 api_version_retired` with a pointer to v2. The
docs stage rewrites `docs/version-policy.md` for this.

7.2 **Removed:** `X-Org-ID`, `/auth/login`, `/auth/iam`, `/auth/refresh`, `/auth/logout`, `/auth/organizations`
(sign-in is between the client and Silicon Accounts now; the service never sees a refresh token),
`/testing-environment/iam`, `/internal/v1/*` (IAM events and hook-destination provisioning) and
`/internal/honeycomb/*`.

7.3 **Added:** `GET/POST /viewers`, `DELETE /viewers/{viewer}`, `GET/POST /allowed-accounts`,
`DELETE /allowed-accounts/{account}` (a Carbon names its Silicon with `silicon_id`). `GET /auth/me` describes the
account (uuid, kind, id, display name, photo, custodian, `can_manage_reminders`, credential and issuing app, visible
Silicon count). Schedules carry `owner` (`{uuid, id, kind}`) instead of `org_id`; subscriptions carry `owner` and
`silicon_uuid`; test environments carry `owner_uuid` and `owner`.

7.4 **Copy.** Error codes and messages a Carbon or Silicon reads never name IAM, Honeycomb or organizations
(`legacy_token_rejected`, `test_key_field_retired`). The OpenAPI document was rewritten for contract 2 (bearerAuth =
Silicon Accounts access token, proofAuth on the five read operations) and validates with openapi-spec-validator
0.7.2. Its copies in `crates/client` and `crates/cli` were refreshed with `scripts/sync-package-docs.py` because CI
fails when they differ, although the client and CLI code still speak v1 until their stage.

7.5 A new API test calls every operation of `openapi.yaml` without a credential and checks it is routed and refused the
way its security says, so the document and the router cannot drift apart silently.

### 8. Delivery

8.1 No Ting adapter: Remind delivers by signed webhooks to the Silicon's own subscriptions (Hook, or any https URL),
unchanged. Delivery stops for suspended and deleted accounts (5.2) and keeps going for unlinked IAM-era rows (4.3).

8.2 A Carbon never subscribes, changes or ends subscriptions; the Silicon does (it is the Silicon's delivery). The
custodian lists them read-only (7.3).

### 9. Versions and packaging touched by this stage

9.1 The service crate is `silicon-remind` 0.6.0 (`apps/remind.md`: 0.6.0 for client, CLI and apps.yaml; those are
bumped by their stages).

9.2 `.env.example`, `compose.yaml` (unchanged: it reads `.env`) and the Dockerfile (no vendored crate) follow the new
configuration. The AWS templates under `deploy/aws/` still carry IAM settings; the deploy stage owns them.

### 10. Left for later stages (not decided here)

- Client crate and CLI: still call `/api/v1` with IAM sessions; they move to contract 2, Silicon Accounts sign-in
  (positional `remind login <slt>`, device flow), `remind accounts --json` with the hidden `iam --json` alias.
- Web (Next.js + Arc UI), docs (`docs/*.md` still describe IAM; dated IAM-era reports move to `docs/history/`),
  packaging (`apps.yaml`, release workflow), deploy templates and docs-site navigation (`iam.md`).
