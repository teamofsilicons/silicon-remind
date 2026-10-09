-- Test environments belong to the Silicon Accounts account that created them.
-- Additive only: legacy environments keep org_id, creator_id and their IAM
-- columns. An operator links a legacy 32-character-key environment to its
-- account with `remind-migrate link-identities` (which sets owner_uuid).
-- Environments that IAM or Honeycomb controlled (iam_control_version set) stay
-- dormant: Remind no longer lists, enters, works or sweeps them, and their
-- schemas are kept until the Carbon decides what to do with them.
ALTER TABLE public.testing_environments ADD COLUMN owner_uuid text;
ALTER TABLE public.testing_environments ALTER COLUMN org_id DROP NOT NULL;
ALTER TABLE public.testing_environments ALTER COLUMN iam_environment_id DROP NOT NULL;
ALTER TABLE public.testing_environments
    ADD CONSTRAINT testing_environments_owner_uuid_valid
    CHECK (owner_uuid IS NULL OR owner_uuid ~ '^[A-Za-z0-9]{1,64}$');

-- Active names are unique per owning account.
CREATE UNIQUE INDEX testing_environments_owner_active_name
    ON public.testing_environments (owner_uuid, name)
    WHERE deleted_at IS NULL AND owner_uuid IS NOT NULL AND iam_control_version IS NULL;

CREATE INDEX testing_environments_owner_idx
    ON public.testing_environments (owner_uuid, id)
    WHERE owner_uuid IS NOT NULL;
