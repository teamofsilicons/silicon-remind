-- IAM's cutover must pass its global collision preflight before this migration.
-- Only declared identity columns are rewritten. Signed/replay JSON, request
-- hashes, free text, ciphertext and private UUID keys are deliberately retained.
CREATE OR REPLACE FUNCTION pg_temp.schema_actor(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^si:[a-z0-9_-]{3,50}$' OR value ~ '^c:[a-z0-9_-]{3,30}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]{3,50}:[a-z0-9_-]{3,50}$' THEN RETURN 'si:' || split_part(value, ':', 1); END IF;
    IF value ~ '^[a-z0-9_-]{3,30}$' THEN RETURN 'c:' || value; END IF;
    RAISE EXCEPTION 'unmapped public actor ID in schema cutover: %', value USING ERRCODE='22023';
END $$;
CREATE OR REPLACE FUNCTION pg_temp.schema_app(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $$
BEGIN
    IF value ~ '^[a-z][a-z0-9_-]{0,79}$' THEN RETURN value; END IF;
    IF value ~ '^[a-z0-9_-]+>[a-z][a-z0-9_-]{0,79}$' THEN RETURN split_part(value, '>', 2); END IF;
    RAISE EXCEPTION 'unmapped application ID in schema cutover: %', value USING ERRCODE='22023';
END $$;

UPDATE public.honeycomb_environments SET app_id=pg_temp.schema_app(app_id);
-- Creator IDs here are Remind-private keys; preserve them and all receipts.
