ALTER TABLE public.testing_environments DROP CONSTRAINT testing_environments_owner_uuid_valid;
ALTER TABLE public.testing_environments ADD CONSTRAINT testing_environments_owner_uuid_valid CHECK (owner_uuid IS NULL OR owner_uuid ~ '^([A-Za-z0-9]{1,64}|[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$');
