# Standard account UUID cutover

Accounts now issues canonical lowercase UUIDv4 identifiers (128 bits / 16 bytes).
Remind accepts them alongside legacy short IDs during migration. Public `c:` and
`si:` names retain their display and lookup meaning. Use immutable account UUIDs
for filters and relationships.

Use the single CSV exported by Accounts' `accounts-migrate-uuids prepare`. Its
header is exactly `old_uuid,new_uuid,kind`; never generate a separate mapping for
Remind. Back up production and testing databases, stop API/worker/maintenance
writers, and run `remind-migrate` first so production, control metadata and all
active or retained test schemas have the latest schema.

Install the pinned tools into a virtual environment:

```sh
python3 -m venv .venv-uuid
.venv-uuid/bin/pip install -r scripts/accounts_uuid128.requirements.txt
```

Set `REMIND_DATABASE_URL` to production and `REMIND_TESTING_DATABASE_URL` to the
separate testing database. URLs are read from environment variables, never
included in reports. Both commands below are rollback-only dry runs:

```sh
.venv-uuid/bin/python scripts/migrate_account_uuids.py --file account-uuid-map.csv
.venv-uuid/bin/python scripts/migrate_account_uuids.py --file account-uuid-map.csv --testing --database-env REMIND_TESTING_DATABASE_URL
```

Add `--apply --writers-stopped` to each command to persist changes. A database is
one transaction; the production and testing databases must both finish before
restarting writers. A failure rolls back that database and can be retried with the
same map. Mapping/kind conflicts and account merges fail preflight. The testing
run checks that registered active and retained schemas match the stored schemas,
then migrates all of them together.

Accounts, custody, storage-key ownership, IAM identity links, viewer/allow-list
edges, typed actor references, event receipt subjects, structured identity metadata
and test-environment owners/creators move to the new UUIDs. Null-kind webhook-first
tombstones are supported. Real foreign keys are deferred and rechecked; immutable
row and custody-transfer triggers are restored to their previous states. Rekeying
a custodian does not revoke grants as though custody changed.

Remind-private storage UUIDs, reminder and execution IDs, public IDs, snapshots,
text, ciphertext, destination signing secrets and test keys remain unchanged.
Credential encryption is tied to the private destination/owner key or retained
legacy namespace, so no credential re-encryption is needed. Signed incoming body
hashes remain exact. Unrelated service actor names are never treated as accounts.

The persisted mapping is a retired-subject deny list, never an authentication
alias. Old bearer/proof subjects and in-flight old-identity webhooks cannot recreate
accounts. Re-running legacy IAM link imports with retired destination IDs is
refused. Accounts revokes old sessions/proofs and sends fresh reconciliation events;
users must sign in again. Restart only after Accounts and every linked consumer
have applied the same map. Rollback before accepting new writes requires restoring
all participants' coordinated backups, not swapping the map backwards.

Validation uses full production migrations and multiple active/retained testing
schemas (`scripts/test_accounts_uuid128.py`), checking dry-run, apply, replay,
conflicts, grants, encrypted credential decryption and trigger restoration. Rust
regressions cover canonical UUID filtering, old-subject denial and ignored in-flight
events. A fresh hosted Carbon and Silicon sign-in plus real reminder delivery is
the live cutover acceptance check.
