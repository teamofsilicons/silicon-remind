# Session reliability release — 22 September 2026

Final CLI release: **0.3.2**. The production backend and browser deployment revisions below remain unchanged by this CLI follow-up.

Remind client and CLI 0.3.1 are deployed and published from `a37ddc66e2e4f7f7717576f3596f4da22596692e`, tag `v0.3.1`. The backend package version remains 0.1.0; source revision and image digest identify this rollout.

## Deployment

On `i-0546693fac4a32d6d`, the API and worker run:

`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:b27174213801bd17e50d84af4c6f19bdb83f9c32cccec2262689da1e2623b03f`

The frontend runs:

`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-remind-production@sha256:6ba5a43f6de91439567b0edb821e68dff1d57cf9263ad7d5d2ec75db4b10bc77`

The backend image was built in ARM64 CI and checked against its archive checksum and exact source label. Frontend assets were built from the isolated release tag and added to the existing immutable runtime; package manifests and lockfile were unchanged. Existing frontend session storage and private key remained mounted at `/sessions` from `/var/lib/remind-frontend/sessions`.

Only API/worker image references in `/usr/local/sbin/remind-start` and the frontend systemd image reference changed. Runtime/frontend environment and Caddy hashes were unchanged. Backend replacement preserved the frontend, Caddy and docs containers. API/worker and frontend reported zero restarts after readiness recovered.

No schema changes or identity import were required. The previous API/worker containers remain stopped with suffix `-before-session-20260922`; configuration backup is `/var/backups/remind/session-20260922`. The prior frontend unit remains as `.before-session-20260922`. Existing startup files pin the new images and survive reboot. Future CloudFormation reprovisioning must select these digests and retain frontend session storage.

## Verification

- Public backend `/health/ready`: 200; frontend and anonymous `/ui/session`: 200.
- [Full source CI](https://github.com/teamofsilicons/silicon-remind/actions/runs/35662257456), [ARM64 image build](https://github.com/teamofsilicons/silicon-remind/actions/runs/35662262771), and [six-platform native CLI tests/build/package](https://github.com/teamofsilicons/silicon-remind/actions/runs/35662257369) passed. One transient artifact-upload 403 required retry after a successful build.
- Maharaj's retained production session authenticated after upgrade and returned the same six existing reminder IDs. No new reminder or outbound delivery was created for this release check.

## Publication

[GitHub release 0.3.1](https://github.com/teamofsilicons/silicon-remind/releases/tag/v0.3.1) is public. Honeycomb accepted `tos>remind` 0.3.1 as release `de9f1568-6e4c-4381-ba35-ecf0ef5898db`. Archive SHA-256: `9040e704f25fcf2992bb39f48d2f0833f326a293e952e9f9597857a5599be91c`. All six platform binaries and the manifest were validated; GitHub asset sizes and digests match the locally verified archive. A fresh anonymous default-latest installation resolved 0.3.1 and passed native macOS ARM64 version/help checks.

Registry packages `silicon-remind-client`, `silicon-remind-cli` 0.3.1 were downloaded from crates.io and checked for the exact clean tagged source revision.

## Early access rejection follow-up

CLI 0.3.2, source `90c32a3ece98ca5d3179afc217e76547ea2f56d7` (tag `v0.3.2`), additionally recovers when the backend rejects an access token before its locally stored expiry. Authenticated commands perform a read-only live session check under the existing session-store lock. An inactive session triggers one refresh and one status retry; the command executes only after verified recovery, so mutation payloads and idempotency keys are not replayed. Permission denials and provider outages remain errors and retain credentials. A rejected refresh family is reported as inactive. Targeted regression tests and Clippy with warnings denied passed. The successor is saved before use; next-process retention and ordinary reads are covered.

Backend and browser deployments above remain at their verified source revisions; this follow-up changes CLI/session-runtime behavior only. Publication and platform validation for this follow-up are recorded below.

[Final CLI full CI](https://github.com/teamofsilicons/silicon-remind/actions/runs/35666144915) passed. All six native builds in [release CI](https://github.com/teamofsilicons/silicon-remind/actions/runs/35666144903) passed at the tagged source revision. The downloaded immutable native artifacts were checked for all six operating-system/architecture formats and packaged locally using the clean tagged packaging script and Honeycomb; this avoided rebuilding the packaging CLI inside CI. The archive contains only the manifest and six native executables.

[GitHub 0.3.2](https://github.com/teamofsilicons/silicon-remind/releases/tag/v0.3.2) is public; every release asset size and SHA-256 matches its locally verified file. Honeycomb accepted `tos>remind` 0.3.2 in the production channel, release `3b520afb-ecf2-4f70-9b61-f0bcebbe1a96`. Archive SHA-256:

`4a715ccb775e5fc55b86d87e0571d5ff9cef7321cefd34d829c36efef8759aea`

A fresh anonymous default-latest install resolved 0.3.2 and passed native macOS ARM64 version/help checks. `HONEYCOMB_NO_SERVICE=1` prevented installing a background service in the disposable verification home.

New registry archives were downloaded independently and verified against the clean tagged source:

- `silicon-remind-cli` 0.3.2: SHA-256 `6f371a080921e9643a7700c4d6483f6ff6382b0745efc4f4ee777fc654fa14fe`.

Machine-readable final CLI release evidence: `/tmp/session-deploy-dhr-20260922/remind-final-release-proof.json`. The retained Maharaj package rollout is coordinated separately so its existing session and package homes stay intact.
