-- IAM controls imported names and deletion. A retired imported world must not
-- reserve a name in Remind, or conflict with an independently managed legacy root.
DROP INDEX public.testing_environments_active_name;
CREATE UNIQUE INDEX testing_environments_active_name
    ON public.testing_environments (org_id, name)
    WHERE deleted_at IS NULL AND iam_control_version IS NULL;
