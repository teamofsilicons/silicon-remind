# Silicon Remind internal integration API

This document defines Remind's non-public integration surface. These routes are
not part of `openapi.yaml`, must never be exposed through a public ingress, and
use different trust boundaries for provisioning and IAM events.

All JSON errors use Remind's standard envelope and include a request ID:

```json
{
  "error": {
    "code": "validation_failed",
    "message": "The request contains invalid data.",
    "request_id": "0199..."
  }
}
```

## Hook destination provisioning

The Hook provisioning routes require:

```http
Authorization: Bearer <REMIND_INTERNAL_API_TOKEN>
Content-Type: application/json
```

The credential is a service secret, not an IAM user token. Deployments should
give it only to the trusted control-plane integration that receives Hook's
one-time endpoint credential.

### Register or rotate a destination

```http
PUT /internal/v1/hook-destinations
```

```json
{
  "org_id": "tos",
  "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
  "silicon_id": "assistant:tos",
  "endpoint_url": "https://hook.teamofsilicons.com/silicon/assistant:tos/40AE2F",
  "signing_secret": "whsec_<unpadded-base64url-32-byte-key>"
}
```

`principal_id` is the stable IAM UUID used for authorization and ownership.
`silicon_id` is the immutable public global ID used in the Remind API and Hook
routing. The local and organization labels are lowercase, 3–50 characters, and
contain only letters, digits, `_`, or `-`; the global ID must be exactly
`{local}:{org_id}`.

The endpoint must use the configured Hook origin and one of Hook's documented
ingress paths:

```text
/silicon/{silicon_id}/{UPPERCASE_6_HEX}
/api/v1/silicon/{silicon_id}/{UPPERCASE_6_HEX}
```

An optional trailing slash is accepted. Userinfo, query strings, fragments,
additional path segments, a different Silicon, and a different origin are
rejected. Production destinations must use HTTPS. The `whsec_` suffix must be
canonical unpadded base64url for exactly 32 bytes.

The first registration returns `201 Created`; replacement with the same
principal/public-ID binding returns `200 OK`:

```json
{
  "org_id": "tos",
  "silicon_id": "assistant:tos",
  "version": 1,
  "updated_at": "2026-08-31T12:00:00Z"
}
```

Registration atomically establishes the principal-to-public-ID binding and
stores the endpoint URL and signing secret encrypted. A Silicon cannot create a
schedule before an enabled destination exists. The public create operation
returns `409 webhook_not_configured` with `Set the webhook url first.` when the
binding or enabled destination is absent. A different public ID for an
established principal, or any provisioning attempt after an IAM revocation
tombstone, instead returns `409 silicon_unavailable` and cannot silently
reactivate the identity.

### Disable a destination

```http
DELETE /internal/v1/hook-destinations/{org_id}/{silicon_id}
```

A successful disable returns `204 No Content` and immediately prevents further
delivery through that destination. It does not revoke the IAM identity. The
encrypted row is retained for 45 days and then permanently purged by a bounded
worker sweep. New schedule creation returns the documented
`webhook_not_configured` conflict until trusted provisioning rotates or
re-enables the destination.

## Silicon IAM application events

IAM events use HMAC authentication, not the internal bearer credential:

```http
POST /internal/v1/iam/events
X-Silicon-IAM-Event-ID: <uuid>
X-Silicon-IAM-Timestamp: <unix-seconds>
X-Silicon-IAM-Key-Version: <positive-integer>
X-Silicon-IAM-Signature: v1=<64-lowercase-hex-characters>
Content-Type: application/json
```

For the selected version in `REMIND_IAM_WEBHOOK_KEYRING`, IAM computes
HMAC-SHA-256 with the complete literal `whs_...` credential as the key and these
exact message bytes:

```text
{X-Silicon-IAM-Timestamp}.{exact raw request body bytes}
```

This differs from Silicon Hook's `whsec_` convention: Hook decodes the suffix,
whereas the IAM sender and receiver use the full issued `whs_` credential.
Remind rejects duplicate security headers, unknown versions, non-canonical
signatures, timestamps more than five minutes in the past or future, and a body
whose `event_id` differs from the signed header. Verification happens before
JSON parsing or state changes.

The envelope follows IAM's published version 1.0 schema:

```json
{
  "spec_version": "1.0",
  "event_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5ca",
  "event_type": "organization.membership.removed.v1",
  "occurred_at": "2026-08-31T12:00:00Z",
  "aggregate": {
    "id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cb",
    "type": "membership",
    "version": 7
  },
  "data": {
    "org_id": "tos",
    "principal_id": "0198f74d-7ef7-7c9f-95bf-7d403a61e5cc",
    "principal_type": "silicon"
  }
}
```

Remind applies local lifecycle state for two projections:

- `organization.membership.removed.v1` with aggregate type `membership` and
  `data.org_id`, `data.principal_id`, and `data.principal_type`. A Silicon
  principal is revoked; a Carbon removal is an accepted no-op.
- `organization.updated.v1` with `data.org_id` and `data.status: "disabled"`.
  The entire organization is revoked.

Every other syntactically valid, positively versioned IAM event name is
authenticated and durably recorded as a processed no-op. This includes future
additive producer events. Malformed event names, invalid envelope versions, and
invalid payload field types still fail validation so wire corruption remains
visible.

A newly accepted or exactly replayed event returns `202 Accepted`:

```json
{
  "receipt_id": "0199d8d5-2f62-7eb7-81c5-1e456778f8de",
  "status": "accepted"
}
```

The pair `(source, event_id)` is durable. Repeating the exact signed event is
safe; reusing its event ID for a changed envelope returns
`409 event_id_conflict`.

Lifecycle processing writes an irreversible organization or principal
tombstone before bounded inline cleanup. Those tombstones immediately block
schedule creation, reads, materialization, delivery claims, and destination
lookup. The worker drains any remaining schedules, pending executions, and
destinations in bounded transactions on later cycles.

## Operational endpoints

The API process exposes `/health/live`, `/health/ready`, and `/metrics` on its
API listener. The worker exposes only those same operational paths on
`REMIND_WORKER_OPERATIONAL_BIND_ADDR`, loopback port `9090` by default. They are
unauthenticated and contain no tenant data, so deployments must restrict them to
health probes and the metrics collector rather than a public ingress.

Readiness has a two-second deadline and validates PostgreSQL connectivity,
every migration embedded in the running binary by version and checksum, and
critical schema fields. It tolerates additional migrations from a newer binary
to support rolling deployments.
