-- Contract usage is local operational state, never external telemetry.
CREATE TABLE api_contract_versions (
    version integer PRIMARY KEY CHECK(version > 0),
    status text NOT NULL CHECK(status IN ('active','deprecated','sunset')),
    deprecated_at timestamptz,
    last_requested_at timestamptz,
    request_count bigint NOT NULL DEFAULT 0 CHECK(request_count >= 0),
    sunset_at timestamptz,
    CHECK(status = 'active' OR deprecated_at IS NOT NULL),
    CHECK(status <> 'sunset' OR sunset_at IS NOT NULL)
);
INSERT INTO api_contract_versions(version,status) VALUES(1,'active');
