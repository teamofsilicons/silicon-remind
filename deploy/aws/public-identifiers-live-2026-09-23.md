# Public identifier cutover — 2026-09-23

Remind API, worker and frontend run version 0.4.0 from source
`cc370a037df33c29295b7178393a4558fff1d6f5`. IAM app ID is `remind`;
actor public IDs use `c:` and `si:`. Remind's private principal and membership
UUIDs remain unchanged.

## Verified runtime

Host: `i-0546693fac4a32d6d` (us-east-1).

- API and worker image: `silicon-remind-production@sha256:f1711b968247c5f8696901146bccd1feea4bd89f4d4224419252d045588cdc82`.
- Frontend image: `silicon-remind-production@sha256:203c9515f85bf6ba3f275d0a6b6bd04c8b11bdda0b9e1be2c76ae7d044b305f4`.
- Data migration 9 and testing control migration 6 applied with exact embedded checksums.
- All three containers run their pinned images with zero restarts; API and worker health checks pass.
- Public backend `/health/ready`, frontend `/`, and frontend `/ui/api/health/ready` returned HTTP 200. Backend reports 0.4.0.
- A fresh, narrowly scoped IAM grant and the 0.4.0 CLI authenticated as `c:saket` in `tos`. Status, active reminders and archived reminders reads succeeded. Both lists are empty in that org scope; this does not assert that the whole database is empty.

Final acceptance SSM command: `a9ee20f0-ce5a-4bd0-87a5-bdaff3260e52`.
Migration command: `6de7cbeb-bc61-44fa-901e-2f7c17e2ff4b`.
Local operator evidence is retained under `/tmp/remind-cutover-20260923`;
authenticated responses and credentials are private and are not checked in.

## Preservation checks and recovery

Both databases were dumped after writers stopped; `pg_restore --list` verified
the archives. The new embedded migrator first passed on an isolated restored
copy. It then applied to the live databases using existing migrator credentials.
No permanent role or password changes were made. The temporary encrypted SSM
parameter carrying those credentials was removed after acceptance.

The migration verified 28 identity bindings against IAM's final quiesced mapping.
Fingerprints of 14 production and three testing tables, excluding only the
declared public identity columns, were unchanged. User trigger modes were
preserved and no unmapped identity keys remained. Both encrypted fields of the
one retained destination decrypted successfully using the preserved AAD rules.

Recoverable backups remain on the host at
`/var/backups/remind/public-id-1790175843706895243`:

| File | SHA-256 |
| --- | --- |
| `production.dump` | `d5cdfc7f28174246ee892386bbf2e4b698d922bd2c9eef7a285ead10b1893b04` |
| `testing.dump` | `4bcdeae1fa505b9d11d007c18005beb5c2ea25890b11a372c3873e42e11aefe9` |
| `configuration-sessions.tar.gz` | `71cc59185dacda323a5a7c40c35b6c0cfbfb3f862a2f52e04e842a511c736a91` |

All four recovery files, including the preparation receipt, have verified
encrypted off-host copies in bucket
`silicon-browser-production-234951665042-us-east-1`, prefix
`backups/public-identifiers-20260923/remind-frozen-1790175843706895243/`.
The protected `offhost-verified.json` receipt records checksums and object
version IDs. The frozen encrypted RDS snapshots are available:

- `silicon-remind-production-id-cutover-20260923-145640`
- `silicon-remind-testing-production-id-cutover-20260923-145640`

Rollback requires a coordinated IAM-compatible database/configuration/runtime
restore; starting an old binary against the converted database is not a rollback.

## Persistent configuration and browser sessions

Only `REMIND_IAM_APP_ID` changed in `/etc/remind/runtime.env`; every other byte
was preserved. Secrets Manager `silicon-remind/runtime-production-dbqkfb` now
contains `REMIND_IAM_APP_ID=remind`, version
`9adef876-6fdc-4d4e-b7d0-cc9c5bc30ad7`, with all 12 prior fields preserved.
The existing startup script received only the two exact backend image-reference
replacements; its whole provisioning path was not rerun. `remind.service`
was not stopped. The Caddy container and configuration were unchanged.

The offline session converter retained all 26 records. It changed public IDs in
two cached identities, preserving private principal/membership UUIDs and all
other plaintext fields; the remaining 24 ciphertexts were byte-identical.
The converted store uses the existing persistent bind mount. The old store
remains at `/var/lib/remind-frontend/sessions-before-public-id-20260923`.
The frontend environment and encryption key did not change.

Both identity-bearing browser sessions had already exceeded the seven-day idle
TTL before cutover (12.60 and 15.07 days). A retained-browser-login claim is
therefore not made; fresh scoped CLI authentication was used for acceptance.

## Native distribution

The separate six-platform CLI 0.4.0 build is workflow `35881364818`, source
`197039b989e526ddd3bce2c4351c0fd74a47afb7`. It pins Honeycomb 0.4.0 and preserves
the existing glibc 2.28 Linux baseline. Native/registry publication is pending
this build's verification; runtime deployment above is complete.

## Client publication

GitHub v0.4.0 is public. All three release asset SHA256 values match the verified six-platform workflow 35881364818 at 197039b989e526ddd3bce2c4351c0fd74a47afb7. Honeycomb prod releaseed81b514-be14-4da7-b33b-cc6637d15b98 is accepted, package SHA256d44a4aa26a2367905e2df0dc469fe41ac342235aa0c6067896927c77feba98ce. A clean anonymous installation reports remind0.4.0. Both silicon-remind-client and silicon-remind-cli0.4.0 are published to crates.io.
