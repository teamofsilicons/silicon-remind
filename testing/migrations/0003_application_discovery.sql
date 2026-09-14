-- IAM is the authority for discovered worlds; preserve legacy administrative pairing.
ALTER TABLE public.testing_environments ALTER COLUMN creator_id TYPE text USING creator_id::text;
ALTER TABLE public.testing_environments ADD COLUMN iam_control_version bigint;
ALTER TABLE public.testing_environments ADD COLUMN iam_cleaned_at timestamptz;
ALTER TABLE public.testing_environments ADD COLUMN webhook_key_digest text;
