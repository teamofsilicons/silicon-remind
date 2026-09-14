-- IAM is the authority for discovered worlds; preserve legacy administrative pairing.
ALTER TABLE public.testing_environments ALTER COLUMN creator_id TYPE text USING creator_id::text;
ALTER TABLE public.testing_environments ADD COLUMN iam_control_version bigint;
ALTER TABLE public.testing_environments ADD COLUMN iam_cleaned_at timestamptz;
ALTER TABLE public.testing_environments ADD COLUMN webhook_key_digest text;

-- IAM controls imported names and deletion. A retired imported world must not
-- reserve a name in Remind, or conflict with an independently managed legacy root.
DROP INDEX public.testing_environments_active_name;
CREATE UNIQUE INDEX testing_environments_active_name
    ON public.testing_environments (org_id, name)
    WHERE deleted_at IS NULL AND iam_control_version IS NULL;
