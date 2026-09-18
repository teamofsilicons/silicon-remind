# API compatibility and version policy

## Select a contract

Remind's current wire contract is **v1**. Request paths begin with `/api/v1`; clients may explicitly send `X-Remind-API-Version: 1`. The server rejects an unsupported or contradictory selection with HTTP 406 before running the handler. Responses identify the selected version with the same header. `GET /api/versions` advertises the implemented protocols, lifecycle states, and current version; the Rust client's `versions()` method exposes this catalog.

The protocol is HTTPS with JSON. Use the published [OpenAPI document](../openapi.yaml) to generate consumers. Package versions and wire versions are independent: the new CLI/client source release is 0.2.0 while the HTTP API remains v1.

## Compatibility matrix

| Consumer | HTTP contract | Sandbox selection | Background updates |
| --- | --- | --- | --- |
| CLI/client 0.1.2 | v1 | Legacy Remind key | Usage-triggered CLI check |
| CLI/client 0.2.0 source release | v1 | IAM app_secret or legacy key | OS-supervised CLI daemon |
| Current website | v1 | IAM app_secret or legacy key | Deployed by operator |

Old optional-header consumers remain accepted. Existing paths, status codes, reminder semantics, and legacy keys remain supported. Additive response fields must be ignored by consumers. Changing required inputs, removing fields or endpoints, or changing permission semantics requires a new major wire contract and a documented migration path. Sandbox-only public-ID login never changes production authentication rules.

## Deprecation and sunset

The database registry stores status, deprecation time, last request time, and total requests per version. This is local contract-governance state, independent of external telemetry. Test usage stays in its own schema and cannot keep a production contract alive.

A deprecated, non-current version sunsets after **seven full days with zero requests**, measured from the later of its deprecation time and last request. The worker sweep and discovery endpoint apply this rule atomically in PostgreSQL. Supported requests update usage before executing their handlers, including requests that fail authentication or validation. The current version is never automatically retired. A `Deprecation` response header carries the deprecation timestamp; `Link` points to this migration policy. A retired implementation returns 410; an unimplemented version returns 406 and points to discovery. Because retirement depends on traffic, there is no promised calendar `Sunset` date until retirement is recorded.

When adding another implemented version, operators first register its active contract, add its router/client compatibility tests, and deploy all supported handlers. Only then mark the superseded registry entry deprecated with a database migration. Never mark a route deprecated without shipping its migration guide and consumer compatibility evidence.

## Consumer-driven checks

CI runs the public Rust client and CLI against representative server responses and inspects the committed OpenAPI contract. Webhook consumer tests verify exact JSON envelopes, signatures, idempotency IDs, retry classifications, and success responses. PostgreSQL tests cover archive retention, cross-identity visibility, mutation replay, sandbox discovery and cleanup, and version lifecycle boundaries. The package-doc sync step ships the same OpenAPI and guides inside both public packages.

When extending the API, add a test expressing what an existing consumer requires, then run `cargo test --workspace --all-targets`, strict Clippy, OpenAPI validation, gateway tests, and the documentation link checker. Additive changes should preserve these tests; deliberate breaks belong to a new contract.
