-- Immutable routing identity obtained from live IAM authorization snapshots.
CREATE TABLE iam_organization_bindings (
    organization_id uuid PRIMARY KEY,
    org_id text NOT NULL UNIQUE,
    CHECK (org_id ~ '^[a-z0-9_-]{3,50}$')
);
