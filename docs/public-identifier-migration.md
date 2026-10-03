# Public identifier cutover

Remind uses `si:assistant`, `c:alice`, `remind`, and membership IDs such as `si:assistant[tos]`. A public Silicon ID has no organization component. IAM authorization, authenticated webhook organization metadata and stored `(org_id, private principal UUID)` relations determine organization ownership. The CLI selects an explicit/session organization or the sole IAM grant; multiple grants require `--org`.

With writers stopped, back up production and all sandbox schemas, run IAM's global collision preflight/cutover, then apply data migration 0009 and testing-control migration 0006. Set `REMIND_IAM_APP_ID=remind` and deploy the IAM SDK 4.0.0 consumer together. Each sandbox gets the same data migration through the existing schema migrator.

Data migration 0009 collision-checks renamed identity bindings, retains private principal and membership UUIDs, migrates Silicon references in schedules/executions/deletion ledgers/destinations, and replaces the old `handle:org` CHECK constraints with `si:handle` validation. The UUID resolver rejects retired public ID syntax after the migration. Schedule IDs, execution IDs, private owner keys, idempotency digests/response bytes and history ownership remain unchanged. Testing control updates the stored bare app identity without rewriting lifecycle receipts.

Encrypted webhook destinations remain readable: `destination_associated_data` maps `si:handle` to the original `handle:org` cryptographic component using the stored, authenticated organization. This compatibility operation does not grant organization access. The key, nonce and ciphertext do not change. A separately approved change of handle requires explicit decrypt/re-encrypt under the new AAD; do not improvise a collision rename inside this migration.

Verify existing scheduled reminders, pending delivery, destination decryption, lifecycle removal/tombstones, owner isolation, same-key replay and sandbox isolation. Immutable replay JSON and pre-cutover signed webhook receipts retain their exact historical bytes. Rollback restores coordinated database backups and old binaries/configuration with writers stopped.

The matching IAM SDK 4.0.0 source is vendored under `vendor/silicon-iam-client`, with normalized Cargo metadata and local snapshot provenance. Standalone and Docker builds use this source. This change does not publish a new SDK release.
