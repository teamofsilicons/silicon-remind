-- A Silicon may subscribe one or more independent webhook receivers.
-- The original schema treated a receiver as a singleton per Silicon/principal;
-- retain the rows for compatibility while removing those singleton constraints.
ALTER TABLE hook_destinations
    DROP CONSTRAINT hook_destinations_silicon_unique,
    DROP CONSTRAINT hook_destinations_principal_unique;

CREATE INDEX hook_destinations_active_silicon_idx
    ON hook_destinations (org_id, silicon_id, created_at, id)
    WHERE disabled_at IS NULL;

CREATE INDEX hook_destinations_active_principal_idx
    ON hook_destinations (org_id, owner_principal_id, created_at, id)
    WHERE disabled_at IS NULL;
