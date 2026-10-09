-- Silicon Accounts replaces Silicon IAM as Remind's only identity.
--
-- This migration is additive. It deletes no row and rewrites no existing value:
-- it adds the account tables, relaxes NOT NULL on the organization columns so
-- new rows can be owned by an account instead of an organization, adds indexes,
-- and registers API contract v2 (the identity change retires v1, which only
-- IAM tokens could use). The same file is replayed into every test-environment
-- schema by remind-migrate, so it must run on empty and populated schemas alike.
--
-- Storage keys: the existing uuid-typed ownership columns
-- (schedules.owner_principal_id, hook_destinations.owner_principal_id,
-- deleted_reminders.owner_principal_id) keep their meaning as Remind-private
-- storage keys. account_keys maps each storage key to the Silicon Accounts
-- account that owns it: one key per account for new rows, plus every legacy
-- IAM principal key that an operator links with `remind-migrate link-identities`.

-- Accounts known to Remind, keyed on the permanent Silicon Accounts uuid: a
-- short, case-sensitive base62 string (not an RFC 4122 UUID). The public c:/si:
-- id is for display only and can change. kind is NULL only for an account that
-- Remind has heard about from a webhook before seeing it sign in or look it up.
CREATE TABLE accounts (
    uuid text PRIMARY KEY,
    kind text,
    public_id text NOT NULL DEFAULT '',
    display_name text NOT NULL DEFAULT '',
    pfp_url text NOT NULL DEFAULT '',
    custodian_uuid text,
    custodian_id text,
    status text NOT NULL DEFAULT 'active',
    profile_version bigint NOT NULL DEFAULT 0,
    id_observed_at timestamptz,
    custodian_observed_at timestamptz,
    looked_up_at timestamptz,
    revoked_before timestamptz,
    last_token_iat timestamptz,
    status_changed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT accounts_uuid_valid
        CHECK (uuid ~ '^[A-Za-z0-9]{1,64}$'),
    CONSTRAINT accounts_kind_valid
        CHECK (kind IS NULL OR kind IN ('carbon', 'silicon')),
    CONSTRAINT accounts_public_id_valid
        CHECK (
            public_id = ''
            OR (
                length(public_id) <= 100
                AND public_id ~ '^(c|si):[^[:space:][:cntrl:]]+$'
            )
        ),
    CONSTRAINT accounts_status_valid
        CHECK (status IN ('active', 'access_removed', 'deleted')),
    CONSTRAINT accounts_custodian_valid
        CHECK (custodian_uuid IS NULL OR custodian_uuid ~ '^[A-Za-z0-9]{1,64}$'),
    CONSTRAINT accounts_custodian_is_silicon
        CHECK (custodian_uuid IS NULL OR kind = 'silicon'),
    CONSTRAINT accounts_display_name_size
        CHECK (octet_length(display_name) <= 1000),
    CONSTRAINT accounts_pfp_url_size
        CHECK (octet_length(pfp_url) <= 4096)
);

CREATE INDEX accounts_custodian_idx
    ON accounts (custodian_uuid)
    WHERE custodian_uuid IS NOT NULL;

CREATE INDEX accounts_public_id_idx
    ON accounts (public_id)
    WHERE public_id <> '';

-- Remind-private storage keys and the account behind each one.
CREATE TABLE account_keys (
    storage_id uuid PRIMARY KEY,
    account_uuid text NOT NULL REFERENCES accounts (uuid),
    origin text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT account_keys_origin_valid
        CHECK (origin IN ('accounts', 'identity_link')),
    CONSTRAINT account_keys_storage_id_not_nil
        CHECK (storage_id <> '00000000-0000-0000-0000-000000000000')
);

-- Exactly one key for rows an account creates from now on.
CREATE UNIQUE INDEX account_keys_primary_idx
    ON account_keys (account_uuid)
    WHERE origin = 'accounts';

CREATE INDEX account_keys_account_idx
    ON account_keys (account_uuid, storage_id);

-- The operator's record of which legacy IAM principal became which account.
-- iam_principal_id is the Remind storage key IAM-era rows carry; iam_public_id
-- and iam_kind come from iam_identity_bindings when known. Re-running
-- link-identities with a corrected mapping before cutover replaces a row here
-- and re-points the matching account_keys row; no data row is ever rewritten.
CREATE TABLE identity_links (
    iam_principal_id uuid PRIMARY KEY,
    iam_kind text,
    iam_public_id text,
    accounts_uuid text NOT NULL REFERENCES accounts (uuid),
    linked_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    source text NOT NULL,

    CONSTRAINT identity_links_kind_valid
        CHECK (iam_kind IS NULL OR iam_kind IN ('carbon', 'silicon')),
    CONSTRAINT identity_links_source_not_blank
        CHECK (length(btrim(source)) BETWEEN 1 AND 255)
);

-- Account-level viewer grants: the owner Silicon (or its custodian) lets
-- another account read the owner's reminders. Revoked grants stay as history.
CREATE TABLE reminder_viewers (
    id uuid PRIMARY KEY,
    owner_uuid text NOT NULL REFERENCES accounts (uuid),
    viewer_uuid text NOT NULL REFERENCES accounts (uuid),
    granted_by_uuid text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    revoked_by_uuid text,

    CONSTRAINT reminder_viewers_not_self
        CHECK (owner_uuid <> viewer_uuid),
    CONSTRAINT reminder_viewers_revocation_pair
        CHECK ((revoked_at IS NULL) = (revoked_by_uuid IS NULL))
);

CREATE UNIQUE INDEX reminder_viewers_active_idx
    ON reminder_viewers (owner_uuid, viewer_uuid)
    WHERE revoked_at IS NULL;

CREATE INDEX reminder_viewers_viewer_idx
    ON reminder_viewers (viewer_uuid)
    WHERE revoked_at IS NULL;

-- Silicons are not open to the world: a Silicon (or its custodian) lists the
-- accounts outside its own circle that may share reminders with it.
CREATE TABLE silicon_allowances (
    id uuid PRIMARY KEY,
    silicon_uuid text NOT NULL REFERENCES accounts (uuid),
    allowed_uuid text NOT NULL REFERENCES accounts (uuid),
    created_by_uuid text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    revoked_at timestamptz,
    revoked_by_uuid text,

    CONSTRAINT silicon_allowances_not_self
        CHECK (silicon_uuid <> allowed_uuid),
    CONSTRAINT silicon_allowances_revocation_pair
        CHECK ((revoked_at IS NULL) = (revoked_by_uuid IS NULL))
);

CREATE UNIQUE INDEX silicon_allowances_active_idx
    ON silicon_allowances (silicon_uuid, allowed_uuid)
    WHERE revoked_at IS NULL;

-- New rows are owned by an account, not an organization. Existing rows keep
-- their organization values unchanged; the CHECK constraints on org_id already
-- accept NULL.
ALTER TABLE schedules ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE executions ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE deleted_reminders ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE hook_destinations ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE idempotency_records ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE bug_reports ALTER COLUMN org_id DROP NOT NULL;

-- The composite executions(schedule_id, org_id, silicon_id) key stops being
-- enforced (MATCH SIMPLE) once org_id is NULL, so every execution also
-- references its schedule directly and is removed with it at purge.
ALTER TABLE executions
    ADD CONSTRAINT executions_schedule_fk
    FOREIGN KEY (schedule_id) REFERENCES schedules (id) ON DELETE CASCADE;

-- Destination ciphertext written from now on is bound to the destination row
-- and its owner storage key (aad_version 2). Existing rows keep the original
-- organization-bound associated data (aad_version 1), which stays derivable
-- from their immutable org_id and silicon_id columns. ADD COLUMN with a
-- default fills existing rows without firing the update trigger; the default
-- then moves to 2 for new rows.
ALTER TABLE hook_destinations ADD COLUMN aad_version smallint NOT NULL DEFAULT 1;
ALTER TABLE hook_destinations ALTER COLUMN aad_version SET DEFAULT 2;
ALTER TABLE hook_destinations
    ADD CONSTRAINT hook_destinations_aad_version_valid
    CHECK (aad_version IN (1, 2));
ALTER TABLE hook_destinations
    ADD CONSTRAINT hook_destinations_aad_scope
    CHECK (aad_version = 2 OR org_id IS NOT NULL);

-- Owner-scoped keyset reads and delivery lookups.
CREATE INDEX schedules_owner_created_keyset_idx
    ON schedules (owner_principal_id, created_at DESC, id DESC)
    WHERE deleted_at IS NULL AND status <> 'completed';

CREATE INDEX schedules_archive_owner_created_keyset_idx
    ON schedules (owner_principal_id, created_at DESC, id DESC)
    WHERE deleted_at IS NOT NULL OR status = 'completed';

CREATE INDEX executions_schedule_history_idx
    ON executions (schedule_id, scheduled_for DESC, id DESC);

CREATE INDEX hook_destinations_owner_active_idx
    ON hook_destinations (owner_principal_id, created_at, id)
    WHERE disabled_at IS NULL;

-- Bug reports are replayed and rate limited per account. The original unique
-- constraint treats NULL org_id values as distinct, so account rows get their
-- own uniqueness.
CREATE UNIQUE INDEX bug_reports_account_idempotency_idx
    ON bug_reports (actor_id, idempotency_key)
    WHERE org_id IS NULL;

-- API contract v2: Silicon Accounts access tokens, no organization header,
-- owner-scoped visibility. v1 accepted only IAM tokens, which no longer exist,
-- so it is retired at this cutover instead of waiting for the idle sunset.
INSERT INTO api_contract_versions (version, status)
VALUES (2, 'active')
ON CONFLICT (version) DO NOTHING;

UPDATE api_contract_versions
SET status = 'sunset',
    deprecated_at = COALESCE(deprecated_at, clock_timestamp()),
    sunset_at = COALESCE(sunset_at, clock_timestamp())
WHERE version = 1 AND status <> 'sunset';
