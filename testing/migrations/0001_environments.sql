-- Control metadata exists only in the dedicated shared testing database.
CREATE TABLE testing_environments (
    id uuid PRIMARY KEY,
    org_id text NOT NULL,
    creator_id uuid NOT NULL,
    name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    description text CHECK (octet_length(description) <= 10000),
    iam_environment_id uuid NOT NULL,
    key_hash bytea UNIQUE,
    secrets jsonb NOT NULL,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_activity_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    deleted_at timestamptz,
    purge_after timestamptz,
    CHECK ((deleted_at IS NULL) = (purge_after IS NULL)),
    CHECK ((deleted_at IS NULL) = (key_hash IS NOT NULL))
);
CREATE UNIQUE INDEX testing_environments_active_name
    ON testing_environments (org_id, name) WHERE deleted_at IS NULL;
CREATE INDEX testing_environments_activity ON testing_environments (last_activity_at)
    WHERE deleted_at IS NULL;
CREATE INDEX testing_environments_retention ON testing_environments (purge_after)
    WHERE deleted_at IS NOT NULL;
