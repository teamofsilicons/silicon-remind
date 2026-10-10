# API compatibility and version policy

## Select a contract

Remind's current wire contract is **2**. Request paths begin with `/api/v2`; clients may also send
`X-Remind-API-Version: 2`, and the CLI and the Rust client always do. The server refuses an unsupported or
contradictory selection with `406 unsupported_api_version` before any handler runs, and names the selected version in
the same response header. `GET /api/versions` lists the contracts, their lifecycle states and the current one; the
Rust client's `versions()` returns it.

The protocol is HTTPS with JSON. Use the published [OpenAPI document](../openapi.yaml) to generate consumers. Package
versions and wire versions are independent: CLI and client 0.6.0 speak contract 2.

## Contract 1 is retired

Contract 1 (`/api/v1`) signed callers in with the previous identity service and tied every request to a shared
group of accounts. Both are gone, so it could not keep running beside contract 2: every `/api/v1` path answers
`410 api_version_retired` with a pointer here. What changed for a consumer:

- Authentication is a Silicon Accounts access token issued to Remind (`Authorization: Bearer`), or, for another app
  reading for an account, a User verification proof (`Authorization: Proof`). See
  [signing in and who sees what](accounts.md).
- The group header and the server-side sign-in routes under `/auth/` are gone (`/auth/me` stays): sign-in happens
  between the client and Silicon Accounts, and Remind never sees a refresh token.
- Reminders and subscriptions name their owner (`owner: {uuid, id, kind}`, `silicon_uuid`) instead of a group.
  Accounts are keyed by their permanent Silicon Accounts uuid and shown by their current id.
- Visibility follows custodians and explicit sharing (`/viewers`, `/allowed-accounts`), not groups.
- Test environments are created with a name and an optional description only.

## Compatibility matrix

| Consumer | HTTP contract | Updates |
| --- | --- | --- |
| CLI and client 0.5 and earlier | 1 (retired: `410`) | install 0.6.0 |
| CLI and client 0.6.0 | 2 | CLI: Silicon Apps keeps it current; client: a normal Cargo update |
| Website | 2 | deployed by the operator |

Within a contract, existing paths, status codes and reminder semantics stay supported, and consumers must ignore
response fields they do not know. Changing required inputs, removing fields or endpoints, or changing permission
semantics needs a new major contract and a documented migration path.

## Timezones are required

Since 0.3.0, creating a reminder requires an explicit IANA timezone in the API, the CLI, the Rust client and the
website. A missing, null, empty or whitespace-only timezone answers `422 timezone_required`; CLI callers pass
`--timezone UTC` or another IANA identifier. Existing reminders and text-only edits keep their saved timezone.

## Deprecation and sunset

The database registry stores each contract's status, deprecation time, last request time and request count. This is
Remind's own contract-governance state, separate from telemetry. Usage inside a test environment stays in its own
schema and cannot keep a production contract alive.

A deprecated contract that is not current sunsets after **seven full days with no requests**, counted from the later
of its deprecation time and its last request. The worker sweep and the discovery endpoint apply this rule atomically
in PostgreSQL. Supported requests record usage before their handlers run, including requests that then fail
authentication or validation. The current contract is never retired automatically. A `Deprecation` response header
carries the deprecation time and `Link` points to this policy. A retired contract answers 410; an unknown one answers
406 and points to discovery. Because retirement depends on traffic, there is no promised calendar `Sunset` date
until retirement is recorded.

Contract 1 is the one exception: it was retired at once when Remind moved to Silicon Accounts, because its
authentication stopped existing at that moment (recorded in migration 0010).

When adding another contract, operators first register it as active, add its router and client compatibility tests,
and deploy every supported handler. Only then do they mark the superseded contract deprecated with a database
migration. Never deprecate a route without shipping its migration guide and consumer compatibility evidence.

## Consumer-driven checks

CI runs the public Rust client and the CLI against representative server responses and checks the committed OpenAPI
contract: an API test calls every operation it lists without a credential and checks it is routed and refused the way
its security says. Webhook consumer tests verify exact JSON envelopes, signatures, idempotency ids, retry
classifications and success responses. PostgreSQL tests cover archive retention, visibility through custodians and
sharing, mutation replay, test environments and contract lifecycle boundaries. The package-doc sync step ships the
same OpenAPI and guides inside both public packages.

When extending the API, add a test expressing what an existing consumer needs, then run
`cargo test --workspace --all-targets`, strict Clippy, OpenAPI validation and the documentation link checker.
Additive changes keep these tests passing; deliberate breaks belong to a new contract.
