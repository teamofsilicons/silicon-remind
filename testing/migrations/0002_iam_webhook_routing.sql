-- Multiple Remind sandboxes may be bound to one IAM test environment.
ALTER TABLE testing_environments ADD COLUMN iam_key_hash bytea;
CREATE INDEX testing_environments_iam_key ON testing_environments(iam_key_hash)
    WHERE deleted_at IS NULL;
