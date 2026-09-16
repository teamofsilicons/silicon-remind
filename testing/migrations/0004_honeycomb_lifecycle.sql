-- Service authority and receipts survive clearing or removing sandbox data.
CREATE TABLE public.honeycomb_environments (
    environment_id uuid PRIMARY KEY,
    org_id text NOT NULL,
    app_id text NOT NULL,
    environment_revision bigint NOT NULL CHECK (environment_revision > 0),
    generation bigint NOT NULL CHECK (generation > 0),
    key_version bigint NOT NULL CHECK (key_version > 0),
    state text NOT NULL CHECK (state IN ('pending','active','disabled','retired','purged')),
    operation_id uuid NOT NULL,
    root_secret jsonb NOT NULL,
    root_digest text NOT NULL,
    require_iam_clean boolean NOT NULL DEFAULT false,
    iam_cleaned_before timestamptz,
    events_after timestamptz,
    last_activity_at timestamptz,
    activity_reported_at timestamptz
);
CREATE TABLE public.honeycomb_operations (
    operation_id uuid PRIMARY KEY,
    environment_id uuid NOT NULL,
    org_id text NOT NULL,
    request_hash bytea NOT NULL,
    receipt jsonb NOT NULL
);
