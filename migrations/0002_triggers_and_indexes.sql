-- Derived lifecycle values, append-only protection, and workload-specific
-- indexes are kept separate from the table definitions for readability.

CREATE FUNCTION remind_set_organization_lifecycle_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'organization lifecycle identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.state = 'revoked' AND NEW.state <> 'revoked' THEN
        RAISE EXCEPTION 'organization revocation tombstones are irreversible'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER organization_lifecycle_set_updated_at
BEFORE UPDATE ON organization_lifecycle
FOR EACH ROW
EXECUTE FUNCTION remind_set_organization_lifecycle_updated_at();

CREATE FUNCTION remind_set_silicon_identity_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.principal_id IS DISTINCT FROM OLD.principal_id
        OR NEW.silicon_id IS DISTINCT FROM OLD.silicon_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'Silicon principal and public identity are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.state = 'revoked' AND NEW.state <> 'revoked' THEN
        RAISE EXCEPTION 'Silicon revocation tombstones are irreversible'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER silicon_identities_set_updated_at
BEFORE UPDATE ON silicon_identities
FOR EACH ROW
EXECUTE FUNCTION remind_set_silicon_identity_updated_at();

CREATE FUNCTION remind_set_schedule_lifecycle()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.id IS DISTINCT FROM OLD.id
            OR NEW.org_id IS DISTINCT FROM OLD.org_id
            OR NEW.owner_principal_id IS DISTINCT FROM OLD.owner_principal_id
            OR NEW.silicon_id IS DISTINCT FROM OLD.silicon_id
            OR NEW.created_at IS DISTINCT FROM OLD.created_at
        THEN
            RAISE EXCEPTION 'schedule identity and ownership are immutable'
                USING ERRCODE = '23514';
        END IF;
        NEW.updated_at := clock_timestamp();
    END IF;

    IF NEW.deleted_at IS NOT NULL THEN
        NEW.purge_after := NEW.deleted_at + interval '3888000 seconds';
    ELSIF NEW.completed_at IS NOT NULL THEN
        NEW.purge_after := NEW.completed_at + interval '3888000 seconds';
    ELSE
        NEW.purge_after := NULL;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER schedules_set_lifecycle
BEFORE INSERT OR UPDATE ON schedules
FOR EACH ROW
EXECUTE FUNCTION remind_set_schedule_lifecycle();

CREATE FUNCTION remind_set_execution_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.schedule_id IS DISTINCT FROM OLD.schedule_id
        OR NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.silicon_id IS DISTINCT FROM OLD.silicon_id
        OR NEW.schedule_version IS DISTINCT FROM OLD.schedule_version
        OR NEW.schedule_kind IS DISTINCT FROM OLD.schedule_kind
        OR NEW.scheduled_for IS DISTINCT FROM OLD.scheduled_for
        OR NEW.reminder_text IS DISTINCT FROM OLD.reminder_text
        OR NEW.timezone IS DISTINCT FROM OLD.timezone
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'execution occurrence snapshots are immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER executions_set_updated_at
BEFORE UPDATE ON executions
FOR EACH ROW
EXECUTE FUNCTION remind_set_execution_updated_at();

CREATE FUNCTION remind_set_destination_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.owner_principal_id IS DISTINCT FROM OLD.owner_principal_id
        OR NEW.silicon_id IS DISTINCT FROM OLD.silicon_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'Hook destination identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    IF NEW.disabled_at IS NULL THEN
        NEW.purge_after := NULL;
    ELSE
        NEW.purge_after := NEW.disabled_at + interval '3888000 seconds';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER hook_destinations_set_updated_at
BEFORE UPDATE ON hook_destinations
FOR EACH ROW
EXECUTE FUNCTION remind_set_destination_updated_at();

CREATE FUNCTION remind_set_idempotency_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.actor_type IS DISTINCT FROM OLD.actor_type
        OR NEW.actor_id IS DISTINCT FROM OLD.actor_id
        OR NEW.operation IS DISTINCT FROM OLD.operation
        OR NEW.target_id IS DISTINCT FROM OLD.target_id
        OR NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
        OR NEW.request_hash IS DISTINCT FROM OLD.request_hash
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR NEW.expires_at IS DISTINCT FROM OLD.expires_at
    THEN
        RAISE EXCEPTION 'idempotency scope and request identity are immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER idempotency_records_set_updated_at
BEFORE UPDATE ON idempotency_records
FOR EACH ROW
EXECUTE FUNCTION remind_set_idempotency_updated_at();

CREATE FUNCTION remind_set_internal_event_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.source IS DISTINCT FROM OLD.source
        OR NEW.event_id IS DISTINCT FROM OLD.event_id
        OR NEW.event_type IS DISTINCT FROM OLD.event_type
        OR NEW.org_id IS DISTINCT FROM OLD.org_id
        OR NEW.subject_id IS DISTINCT FROM OLD.subject_id
        OR NEW.payload IS DISTINCT FROM OLD.payload
        OR NEW.payload_hash IS DISTINCT FROM OLD.payload_hash
        OR NEW.received_at IS DISTINCT FROM OLD.received_at
    THEN
        RAISE EXCEPTION 'internal event envelopes are immutable'
            USING ERRCODE = '23514';
    END IF;
    NEW.updated_at := clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TRIGGER internal_event_receipts_set_updated_at
BEFORE UPDATE ON internal_event_receipts
FOR EACH ROW
EXECUTE FUNCTION remind_set_internal_event_updated_at();

CREATE FUNCTION remind_reject_audit_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'audit_records is append-only'
        USING ERRCODE = '55000';
END;
$$;

CREATE TRIGGER audit_records_reject_update
BEFORE UPDATE ON audit_records
FOR EACH ROW
EXECUTE FUNCTION remind_reject_audit_mutation();

CREATE TRIGGER audit_records_reject_delete
BEFORE DELETE ON audit_records
FOR EACH ROW
EXECUTE FUNCTION remind_reject_audit_mutation();

CREATE TRIGGER audit_records_reject_truncate
BEFORE TRUNCATE ON audit_records
FOR EACH STATEMENT
EXECUTE FUNCTION remind_reject_audit_mutation();

-- Public schedule reads and opaque keyset pagination.
CREATE INDEX schedules_org_created_keyset_idx
    ON schedules (org_id, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX schedules_org_owner_created_keyset_idx
    ON schedules (org_id, silicon_id, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX schedules_org_principal_idx
    ON schedules (org_id, owner_principal_id, id);

CREATE INDEX schedules_org_status_created_keyset_idx
    ON schedules (org_id, status, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

-- Multi-worker due scans and bounded retention cleanup.
CREATE INDEX schedules_due_idx
    ON schedules (next_run_at, id)
    WHERE status = 'active'
      AND deleted_at IS NULL
      AND next_run_at IS NOT NULL;

CREATE INDEX schedules_retention_idx
    ON schedules (purge_after, id)
    WHERE purge_after IS NOT NULL;

-- Execution history and lease-based Hook delivery.
CREATE INDEX executions_org_schedule_history_idx
    ON executions (org_id, schedule_id, scheduled_for DESC, id DESC);

CREATE INDEX executions_delivery_due_idx
    ON executions (next_attempt_at, lease_expires_at, id)
    WHERE status IN ('pending', 'retrying');

CREATE INDEX hook_destinations_active_tenant_idx
    ON hook_destinations (org_id, silicon_id)
    WHERE disabled_at IS NULL;

CREATE INDEX hook_destinations_retention_idx
    ON hook_destinations (purge_after, id)
    WHERE purge_after IS NOT NULL;

CREATE INDEX silicon_identities_public_lookup_idx
    ON silicon_identities (org_id, silicon_id)
    WHERE state = 'active';

CREATE INDEX idempotency_expiry_idx
    ON idempotency_records (expires_at, id);

CREATE INDEX internal_event_processing_idx
    ON internal_event_receipts (next_attempt_at, lease_expires_at, id)
    WHERE status IN ('pending', 'failed');

CREATE INDEX internal_event_tenant_timeline_idx
    ON internal_event_receipts (org_id, received_at DESC, id DESC)
    WHERE org_id IS NOT NULL;

CREATE INDEX audit_records_tenant_timeline_idx
    ON audit_records (org_id, occurred_at DESC, id DESC);

CREATE INDEX audit_records_resource_timeline_idx
    ON audit_records (resource_type, resource_id, occurred_at DESC, id DESC);
