-- Accept canonical128-bit Accounts UUIDs during the coordinated identity backfill.
ALTER TABLE accounts DROP CONSTRAINT accounts_uuid_valid;
ALTER TABLE accounts ADD CONSTRAINT accounts_uuid_valid CHECK (uuid ~ '^([A-Za-z0-9]{1,64}|[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$');
ALTER TABLE accounts DROP CONSTRAINT accounts_custodian_valid;
ALTER TABLE accounts ADD CONSTRAINT accounts_custodian_valid CHECK (custodian_uuid IS NULL OR custodian_uuid ~ '^([A-Za-z0-9]{1,64}|[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$');
