-- A retired subject is never an authentication alias. The offline consumer
-- inserts the shared immutable mapping here after migrating linked data.
CREATE TABLE accounts_uuid128_map (
    old_uuid text PRIMARY KEY,
    new_uuid text UNIQUE NOT NULL,
    kind text NOT NULL CHECK (kind IN ('carbon','silicon')),
    mapping_sha256 text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
