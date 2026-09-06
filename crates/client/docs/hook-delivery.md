# Delivery through Silicon Hook

Create a Hook endpoint using its default signature policy, then configure its
exact `endpoint_url` and one-time `signing_secret` in Remind. The secret is textual
(`v1.` plus random characters for Hook-generated values); Remind uses its complete
UTF-8 bytes as the HMAC key. Custom Hook signature policies must match the sender
contract below. The service origin must match `REMIND_HOOK_BASE_URL`.

Production endpoint: `/silicon/{handle:org}/{8-uppercase-alphanumeric-characters}`.
The `/api/v1/silicon/...` compatibility path and optional trailing slash work.
Test endpoint: `/test/silicon/{handle:org}/{8-uppercase-alphanumeric-characters}`.
Remind enforces the production/test path boundary and the authenticated Silicon
ID at configuration and again before delivery. No IAM or Remind root key is sent
to public Hook ingress.

## Request and receipt

The worker posts UTF-8 JSON:

```json
{
  "type": "remind.schedule.triggered",
  "source": "silicon-remind",
  "subject": "<schedule-uuid>",
  "occurred_at": "<intended-UTC-trigger>",
  "schema_version": "1.0",
  "payload": {
    "execution_id": "<stable-occurrence-uuid>",
    "schedule_id": "<schedule-uuid>",
    "silicon_id": "handle:org",
    "text": "<exact reminder text>",
    "scheduled_for": "<intended-UTC-trigger>",
    "timezone": "Asia/Kolkata"
  }
}
```

`webhook-id` and `Idempotency-Key` are the execution UUID. `webhook-timestamp`
is the attempt's Unix seconds. `webhook-signature` is `v1,` plus standard padded
base64 of HMAC-SHA256 over `<webhook-id>.<webhook-timestamp>.<exact JSON bytes>`.
The intended occurrence and text snapshot remain stable across retries, while
the attempt timestamp/signature change.

Success is HTTP 200 with `{"status":"webhook.ok","receipt_id":"<uuid>"}`.
The historical public field `hook_event_id` stores this ingress receipt UUID;
`delivered` means ingress processing completed. Hook deliberately returns the
same receipt shape for verified and withheld requests. It does not prove the
signature passed or the Silicon consumed the event. Inspect Hook's authenticated
verified/blocked history and consumer delivery cursor for those guarantees.

## Retries and duplicates

Transport errors, malformed success bodies, explicit transient statuses
(408/425/429) and server errors retry with bounded exponential backoff/jitter.
Other rejections terminate the occurrence. Attempts and reasons appear in
`remind executions <id>`. Redirects are disabled and responses are size bounded.

Delivery is at least once across uncertain network outcomes. Hook's current
ingress captures requests independently; its Idempotency-Key header does not
promise provider deduplication. Consumers should deduplicate using the stable
`payload.execution_id`. The scheduler's unique occurrence constraint prevents
multiple logical executions for one schedule/version/trigger; it cannot create
exactly-once network delivery.

The one-time reminder archives when its occurrence is materialized. Its retained
execution can still retry within the 45-day archive window. Explicit owner
archive cancels unaccepted work; a network request already accepted remotely
cannot be recalled.
