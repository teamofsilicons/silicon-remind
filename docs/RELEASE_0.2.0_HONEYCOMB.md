# Remind 0.2.0 Honeycomb release — 2026-09-16

Application `remind` is registered in Honeycomb and release `0.2.0` is uploaded.
It is currently private. Publication request
`2f517049-2d06-465e-ab91-ce33d8045cec` is `awaiting_validator`, revision 1,
with one pending `honeycomb` review gate. The current organization-owner session
does not have validator rights. A Honeycomb validator must approve the request
before automatic publication can proceed; public activation is not yet verified.

## Package and verification

- Source commit: `fcf4baf`.
- Manifest: repository-root `honeycomb.yaml`, package format 1, command `remind`.
- Archive: `dist/remind-0.2.0.tar.gz`, 13,546,327 bytes.
- SHA-256: `0a026784d0d03a9d165bfe0cfd43783c883b775771cc1fd3d1bcd1d22e440615`.
- Payloads: optimized executables for Linux, Windows and macOS, each on
  x86_64 and aarch64, under `targets/<platform>/bin/`.
- Honeycomb validated the populated manifest, staged package and final archive.
- Both macOS executables passed `--version` and `--help` locally; Linux binaries
  passed those checks in disposable Debian Bookworm containers.
- Windows binaries compiled and passed PE architecture checks. They were not
  executed on Windows.
- A fresh Honeycomb download/install into an isolated home passed on macOS
  aarch64. Its installed checksum matches the archive; `--version` returned
  `remind 0.2.0` and `--help` succeeded. The alias `remind-release-check` avoided
  replacing the existing command. Temporary copied access credentials were removed.

Generated payloads and archives are ignored by Git. Reproduce them using
`scripts/build-honeycomb-release.py` and `scripts/package-release.py` as documented
in [release packaging](releases.md). Uploaded version bytes are immutable.

## Registration and publication evidence

- Accepted registration operation: `6534fe3f-5faa-43b6-aed3-c0c588749514`.
- Accepted application revision: 1; IAM revision: 7.
- Accepted release upload operation: `763a627c-0562-4847-b3d6-a6e68922ff22`.
- Publication plan: `d7cd1664-d8d8-4bc1-a85e-e164cbf158b1`.
- IAM scopes: `self.identity.read` and `self.membership.read`; no external scopes
  or delegated endpoints requested.
- Receiver: `https://backend.remind.teamofsilicons.com/webhook/`, using the
  configured version-1 signing secret and `membership`/`updates` categories.

The creation input and one-time returned application secret are stored only in
mode-0600 files under the ignored directory
`target/manual-secrets/honeycomb-release-0.2.0/`. No secrets are included here.
This packaging/publication run did not deploy the returned application credential
or the updated backend. Registration is not evidence that production login or
webhook processing works with the new identity; deployment and integration checks
are still required.

Check completion through the Honeycomb console's Sent requests or
`honeycomb publication get 'remind' --json`. After approval, verify the app is
public and anonymously installable before describing public distribution as done.
