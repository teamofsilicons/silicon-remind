-- Custodian changes learned by webhook OR authoritative lookup end delegation
-- made by the previous custodian, including a grant to itself.
CREATE FUNCTION fence_previous_custodian() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.custodian_uuid IS NOT NULL
       AND OLD.custodian_uuid IS DISTINCT FROM NEW.custodian_uuid THEN
        UPDATE reminder_viewers
        SET revoked_at = clock_timestamp(), revoked_by_uuid = OLD.custodian_uuid
        WHERE owner_uuid = NEW.uuid AND revoked_at IS NULL
          AND (viewer_uuid = OLD.custodian_uuid OR granted_by_uuid = OLD.custodian_uuid);
        UPDATE silicon_allowances
        SET revoked_at = clock_timestamp(), revoked_by_uuid = OLD.custodian_uuid
        WHERE silicon_uuid = NEW.uuid AND revoked_at IS NULL
          AND (allowed_uuid = OLD.custodian_uuid OR created_by_uuid = OLD.custodian_uuid);
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER accounts_custodian_grants
AFTER UPDATE OF custodian_uuid ON accounts
FOR EACH ROW EXECUTE FUNCTION fence_previous_custodian();

-- Keep deduplication metadata and the signed-body digest, never profile bodies.
UPDATE internal_event_receipts SET payload = '{}'::jsonb
WHERE source = 'silicon-accounts';
