# IAM canonical identity upgrade

Remind authenticates Carbon IDs and Silicon IDs from the current IAM authorization snapshot. An older IAM response can omit the top-level public ID; its authorized snapshot must still disclose it. A private IAM UUID cannot authenticate a request. Actor type, application audience, organization, public membership ID, and testing environment must agree.

Remind retains its own UUID storage keys for schedule ownership, destinations, idempotency, and audit history. The `iam_identity_bindings` table binds those keys to canonical IDs. This is a local persistence detail; IAM public IDs are authoritative. Every testing schema has an independent table and resolver, with no production fallback.

Before enabling the new server:

1. Stop the old Remind API/worker from writing during migration and backfill. Retain a database backup and the previous image.
2. Apply migration `0008` through the normal migrator, including existing testing schemas. Existing trusted Silicon handles seed their retained keys automatically. Conflicting handles fail the migration.
3. Obtain the complete trusted IAM identity export for production and every retained testing environment. Keep the export private and outside the repository. Set `DATABASE_URL` to the intended database using an owner connection.
4. Run `python3 scripts/backfill-iam-identities.py /private/identity-mapping.json --production`. This validates all imported bindings and retained references, then rolls back. Rerun with `--apply` to persist them.
5. For each testing schema run the same command with `--testing-environment-id UUID` instead of `--production`, first as a check and then with `--apply`. For an existing schema still at migration0007, include `--migrate-retained-schema`; it verifies all previous migration checksums and applies only0008 in the same transaction as the import. The default check rolls back both. The command derives the exact schema; it never falls back to production.
6. Start the updated server and verify an existing owner's reminders and a test world's reminders before IAM cutover.

The import fails rather than overwriting an existing canonical/local-key disagreement. Unknown actors cannot receive a new local key while any retained legacy actor reference remains unmapped. New actors receive fresh local UUIDs only after that check passes. Do not rerun a historical export as an overwrite operation after new identities have been admitted.

Signed retained webhook deliveries may contain old UUID references. New membership-removal tombstones resolve `resource.membership_id`, or the retained membership UUID map, to the same local owner key. This legacy notification support does not permit UUID authentication.
