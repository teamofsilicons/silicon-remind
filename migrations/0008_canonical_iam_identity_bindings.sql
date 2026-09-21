-- IAM public identities are authoritative. Existing UUIDs remain Remind-owned
-- storage keys so schedule ownership, subscriptions, and retry history survive.
-- Each sandbox receives its own copy through the existing schema migrator.
CREATE TABLE iam_identity_bindings (
    identity_kind text NOT NULL CHECK (identity_kind IN ('carbon','silicon','membership')),
    public_id text NOT NULL CHECK (length(public_id) BETWEEN 1 AND 255),
    local_id uuid NOT NULL CHECK (local_id <> '00000000-0000-0000-0000-000000000000'),
    PRIMARY KEY (identity_kind, public_id),
    UNIQUE (identity_kind, local_id)
);

-- These public handles were already bound by authenticated IAM snapshots.
-- Conflicting retained identities fail migration rather than changing owners.
INSERT INTO iam_identity_bindings(identity_kind,public_id,local_id)
SELECT DISTINCT 'silicon',silicon_id,local_id FROM (
    SELECT silicon_id,principal_id AS local_id FROM silicon_identities
    UNION SELECT silicon_id,owner_principal_id FROM schedules
    UNION SELECT silicon_id,owner_principal_id FROM hook_destinations
    UNION SELECT silicon_id,owner_principal_id FROM deleted_reminders
) identities WHERE silicon_id IS NOT NULL;

CREATE FUNCTION iam_unmapped_identity_keys()
RETURNS TABLE(identity_kind text,local_id uuid)
LANGUAGE sql STABLE SET search_path FROM CURRENT AS $$
    SELECT DISTINCT reference.identity_kind,reference.local_id FROM (
        SELECT 'silicon'::text AS identity_kind,principal_id AS local_id FROM silicon_identities
        UNION SELECT 'silicon',owner_principal_id FROM schedules
        UNION SELECT 'silicon',owner_principal_id FROM hook_destinations
        UNION SELECT 'silicon',owner_principal_id FROM deleted_reminders
        UNION SELECT actor_type,actor_id::uuid FROM idempotency_records
            WHERE actor_type IN ('carbon','silicon') AND actor_id ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
        UNION SELECT actor_type,actor_id::uuid FROM audit_records
            WHERE actor_type IN ('carbon','silicon') AND actor_id ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
        UNION SELECT NULL,actor_id::uuid FROM bug_reports
            WHERE actor_id ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
    ) reference WHERE NOT EXISTS (
        SELECT 1 FROM iam_identity_bindings binding
        WHERE binding.local_id=reference.local_id AND binding.identity_kind IN ('carbon','silicon')
          AND (reference.identity_kind IS NULL OR binding.identity_kind=reference.identity_kind)
    );
$$;

CREATE FUNCTION resolve_iam_identity_key(p_kind text,p_public_id text,p_new_local_id uuid)
RETURNS uuid LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
DECLARE result uuid;
BEGIN
    IF p_kind NOT IN ('carbon','silicon','membership') OR p_public_id IS NULL
        OR length(p_public_id) NOT BETWEEN 1 AND 255 OR p_new_local_id IS NULL
        OR p_new_local_id='00000000-0000-0000-0000-000000000000' THEN
        RAISE EXCEPTION 'invalid canonical identity binding' USING ERRCODE='22023';
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
