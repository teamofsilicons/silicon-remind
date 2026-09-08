# Internal service API

These routes are for trusted backend integration. They are deliberately absent
from the public Rust client and CLI. Use public `/api/v1/webhook` for a signed-in
Silicon's normal configuration.

## Provisioning

`Authorization: Bearer <REMIND_INTERNAL_API_TOKEN>` is required. Production
transport must be HTTPS. Requests use JSON and stable error envelopes.

- `PUT /internal/v1/hook-destinations`: body `org_id`, `silicon_id`, immutable
  IAM `principal_id` UUID, `endpoint_url`, and `signing_secret`. Returns 201 on
  initial configuration or 200 on replacement, with org/Silicon/version/time.
  Credentials are encrypted at rest and never echoed. The Silicon ID must
  belong to the org and endpoint; URL/signature format is described in
  [webhook delivery](webhook-delivery.md). An IAM revocation tombstone cannot be
  cleared by provisioning.
- `DELETE /internal/v1/hook-destinations/{org_id}/{silicon_id}` disables that
  destination, returning 204. It does not disclose or rotate its old secret.

Internal provisioning addresses production data. Test headers are rejected;
sandbox Silicons use the normal owner-authenticated public route.

## IAM receiver

`POST /webhook/`, with legacy alias `/internal/v1/iam/events`, authenticates by
IAM's signature, not the internal bearer. Follow [IAM integration](iam.md).
The exact-byte official verifier checks the signed envelope and key version.
A processed event returns 202 with `receipt_id` and `status: accepted`; exact
replays return the original durable receipt, conflicting reuse returns 409.

Signed testing wrappers route only to linked active Remind test environments.
The root key is excluded from persisted normalized envelopes. Current member
and organization revocations are committed with the receipt. Background cleanup
archives inaccessible resources and cancels unaccepted executions. IAM logout
also takes effect through live introspection on the next authenticated request.
