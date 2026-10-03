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

-- The binding's private UUID remains the principal/membership storage key.
-- This unique preflight rejects two old actors becoming the same new actor.
CREATE TEMP TABLE remind_schema_mapping ON COMMIT DROP AS
SELECT identity_kind, public_id AS old_id,
    CASE WHEN identity_kind='membership' THEN
        pg_temp.schema_actor(split_part(public_id,'[',1)) || '[' || split_part(public_id,'[',2)
    ELSE pg_temp.schema_actor(public_id) END AS new_id, local_id
FROM iam_identity_bindings;
CREATE UNIQUE INDEX ON remind_schema_mapping(identity_kind,new_id);
DO $$ BEGIN
    IF EXISTS(SELECT 1 FROM remind_schema_mapping WHERE
        (identity_kind='carbon' AND new_id NOT LIKE 'c:%') OR
        (identity_kind='silicon' AND new_id NOT LIKE 'si:%')) THEN
        RAISE EXCEPTION 'actor kind disagrees with public ID mapping' USING ERRCODE='22023';
    END IF;
END $$;

CREATE TEMP TABLE remind_schema_triggers ON COMMIT DROP AS
SELECT format('%I.%I',n.nspname,c.relname) AS relation,t.tgname,t.tgenabled
FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid
JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE NOT t.tgisinternal AND c.oid IN (
    'silicon_identities'::regclass,'schedules'::regclass,'executions'::regclass,
    'deleted_reminders'::regclass,'hook_destinations'::regclass);

ALTER TABLE silicon_identities DROP CONSTRAINT silicon_identities_public_id_valid;
ALTER TABLE schedules DROP CONSTRAINT schedules_silicon_id_valid;
ALTER TABLE deleted_reminders DROP CONSTRAINT deleted_reminders_silicon_id_valid;
ALTER TABLE hook_destinations DROP CONSTRAINT hook_destinations_silicon_id_valid;
ALTER TABLE executions DROP CONSTRAINT executions_schedule_tenant_fk;
DO $$ DECLARE relation text; BEGIN
    FOREACH relation IN ARRAY ARRAY['silicon_identities','schedules','executions','deleted_reminders','hook_destinations'] LOOP
        EXECUTE format('ALTER TABLE %I DISABLE TRIGGER USER',relation);
        EXECUTE format('UPDATE %I SET silicon_id=pg_temp.schema_actor(silicon_id) WHERE silicon_id IS NOT NULL',relation);
    END LOOP;
END $$;
DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT * FROM remind_schema_triggers LOOP
        EXECUTE format('ALTER TABLE %s %s TRIGGER %I',r.relation,
            CASE r.tgenabled WHEN 'D' THEN 'DISABLE' WHEN 'R' THEN 'ENABLE REPLICA'
                WHEN 'A' THEN 'ENABLE ALWAYS' ELSE 'ENABLE' END,r.tgname);
    END LOOP;
END $$;
UPDATE iam_identity_bindings b SET public_id=m.new_id FROM remind_schema_mapping m
WHERE b.identity_kind=m.identity_kind AND b.public_id=m.old_id;
ALTER TABLE executions ADD CONSTRAINT executions_schedule_tenant_fk
    FOREIGN KEY(schedule_id,org_id,silicon_id) REFERENCES schedules(id,org_id,silicon_id) ON DELETE CASCADE;
ALTER TABLE silicon_identities ADD CONSTRAINT silicon_identities_public_id_valid CHECK(silicon_id IS NULL OR silicon_id ~ '^si:[a-z0-9_-]{3,50}$');
ALTER TABLE schedules ADD CONSTRAINT schedules_silicon_id_valid CHECK(silicon_id ~ '^si:[a-z0-9_-]{3,50}$');
ALTER TABLE deleted_reminders ADD CONSTRAINT deleted_reminders_silicon_id_valid CHECK(silicon_id ~ '^si:[a-z0-9_-]{3,50}$');
ALTER TABLE hook_destinations ADD CONSTRAINT hook_destinations_silicon_id_valid CHECK(silicon_id ~ '^si:[a-z0-9_-]{3,50}$');
-- Destination encryption retains its original handle:org AAD component in
-- crypto::destination_associated_data. Never alter destination ciphertext.

CREATE OR REPLACE FUNCTION resolve_iam_identity_key(p_kind text,p_public_id text,p_new_local_id uuid)
RETURNS uuid LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
DECLARE result uuid;
BEGIN
    IF p_kind NOT IN ('carbon','silicon','membership') OR p_public_id IS NULL
        OR length(p_public_id) NOT BETWEEN 1 AND 255 OR p_new_local_id IS NULL
        OR p_new_local_id='00000000-0000-0000-0000-000000000000' THEN
        RAISE EXCEPTION 'invalid canonical identity binding' USING ERRCODE='22023';
    END IF;
    IF (p_kind='silicon' AND p_public_id !~ '^si:[a-z0-9_-]{3,50}$')
        OR (p_kind='carbon' AND p_public_id !~ '^c:[a-z0-9_-]{3,30}$')
        OR (p_kind='membership' AND p_public_id !~ '^(si:[a-z0-9_-]{3,50}|c:[a-z0-9_-]{3,30})\[[a-z0-9_-]{3,50}\]$') THEN
        RAISE EXCEPTION 'invalid public identifier schema' USING ERRCODE='22023';
    END IF;
    SELECT local_id INTO result FROM iam_identity_bindings
        WHERE identity_kind=p_kind AND public_id=p_public_id;
    IF FOUND THEN RETURN result; END IF;
    -- A missing canonical binding must never silently strand a legacy owner.
    -- Import the trusted IAM mapping before permitting genuinely new actors.
    IF EXISTS(SELECT 1 FROM iam_unmapped_identity_keys()) THEN
        RAISE EXCEPTION 'canonical identity backfill incomplete' USING ERRCODE='55000';
    END IF;
    INSERT INTO iam_identity_bindings(identity_kind,public_id,local_id)
        VALUES(p_kind,p_public_id,p_new_local_id) ON CONFLICT(identity_kind,public_id) DO NOTHING;
    SELECT local_id INTO STRICT result FROM iam_identity_bindings
        WHERE identity_kind=p_kind AND public_id=p_public_id;
    RETURN result;
END;
$$;
