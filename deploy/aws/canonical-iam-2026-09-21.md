# Canonical IAM identity deployment — 2026-09-21

Remind's API and worker now resolve canonical IAM public identities to the existing
Remind storage keys. Existing reminders, archive records, delivery history, and
sessions retain their ownership. The adapter accepts the previous IAM response
and the canonical IAM 3 response, allowing the consumer rollout before IAM cutover.

## Deployed artifacts

The existing ARM64 host `i-0546693fac4a32d6d` in account `234951665042`, region
`us-east-1`, runs both updated containers from source
`6f7e5bc8859454cf363aa36e2fe5b399cd38da29`:

```
234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:32b85ef1a7d08ce7d70a4d42fbffc927afee43adb9d28bdc81bc63e58474a656
```

The binaries were built for `aarch64-unknown-linux-gnu.2.28` and added to the
existing pinned runtime image. The backend package continues to report `0.1.0`;
the source revision and immutable image digest identify this rollout. Existing
CLI and Rust client `0.3.0` remain compatible and need no release for this adapter.

Importer-only fixes `b50614c` and `49246ef` were staged from their committed
sources and verified by SHA-256. They do not change the backend binaries.
The final importer SHA-256 is
`9e1b6d2f4b1184269a03e7f3e4b7182cde88190ace28ca424d135a437c8f1c6c`.

## Migration and preservation

A fresh private IAM export supplied production and environment-scoped identity
mappings. Production and all 13 retained test schemas passed a rollback-only
migration/import before any service pause. Six retained Remind environments have
a local ID different from their IAM environment ID. The importer checks each
registered pair under a lock and selects the local schema explicitly.

The existing testing owner has database temporary-table creation revoked. The
importer uses inline JSON recordsets, so the rollout needed no new database
privileges or password changes. Native PostgreSQL regression checks verified
both the differing-environment-ID case and imports with TEMP explicitly revoked.

The worker and API were stopped while both databases were dumped. The embedded
migrator applied migration 8, then all identity imports completed before either
updated process started. The production runtime grants were reapplied by the
existing migration owner. The result is **124 bindings across production and
13 retained test schemas, with zero unmapped identity references**.

Counts and complete row hashes for every preexisting application table matched
before and after migration while writers were paused. Migration ledgers and the
new binding table were excluded from that comparison. Runtime and frontend
environment files, Caddy configuration, and supporting container identities were
unchanged. Only the API and worker image references in the existing startup
script were updated; the full startup script was not executed.

The rollout command completed at `2026-09-21T09:41:42Z`, taking 23 seconds including
its final preflight. Both new containers became ready within that command.

## Backups and rollback

Two encrypted RDS snapshots are available:

- `silicon-remind-production-before-iam3-20260921-092635`
- `silicon-remind-testing-production-before-iam3-20260921-092635`

The paused backups and original startup script are retained at
`/var/backups/remind/iam3-20260921T094120Z`. Both custom dumps passed
`pg_restore --list` validation:

| Database | Bytes | SHA-256 |
| --- | ---: | --- |
| Production | 82,587 | `b6e1d0be7506fe0cb1308e1c0d18fb1b03fd4d75330becd11e9466d3a3f04b8c` |
| Testing | 980,370 | `d7f4879fedb0922adca7350df38470cef8db2d70fe2f724b4c6c0183b1160248` |

The old API and worker containers are stopped and retained with suffix
`-before-iam3-20260921`. Their image digest is
`sha256:c4036287b448ec9fba590ea65dab924c267e0ca469e32dd9967ee950dafba200`.
Migration 8 is additive and does not rewrite the existing storage keys. Restoring
the old backend is an option only while IAM still serves its old contract; after
IAM cutover, backend rollback requires coordinating IAM rollback as well.

Temporary owner credentials were supplied through a dedicated SecureString
parameter without changing stored application credentials. That parameter,
the host/local owner files, and the staged private identity export were removed
after verification. Database backups and sanitized rollout evidence remain.

## Live verification before IAM cutover

- Backend `/health/live` and `/health/ready` returned 200.
- API and worker reported the expected image/revision, `healthy`, and zero restarts.
- Every production/retained schema reported migration 8 successful and zero
  unmapped identities.
- The actual restricted production runtime role resolved canonical `chef:bricks`
  to its retained Remind owner key.
- Maharaj's existing saved `chef:bricks` session remained authenticated without
  another login and read its reminders successfully.
- All six previously active reminders and all seven archived reminders matched
  the earlier API responses exactly. A separately created seventh active reminder
  was also visible after deployment.
- No saved test token was available locally. Testing preservation was verified
  through the 13 real schema imports, registered environment pairs, complete
  preexisting row hashes, and zero-unmapped-reference checks; it is not claimed
  as an authenticated HTTP test in every retained environment.

SSM evidence:

| Operation | Command ID |
| --- | --- |
| Final rollback-only preflight | `b4daf98d-7328-45c6-b0fe-f24285941c25` |
| Paused backup, migration, import, rollout | `ea4546fd-14a9-4b8d-a67f-64d8c3fddb5c` |
| Runtime owner, all schemas, container health | `cc12710c-eb53-4a7c-bc11-6357ea41b99c` |
| Host credential cleanup | `696425e5-cd6f-432f-8771-ee28b9cbd7e9` |

This record covers the compatible consumer rollout while IAM still served its
previous contract. The coordinated IAM cutover has separate post-cutover checks.
