# Generic webhook delivery

Configure any absolute HTTP(S) endpoint with `POST /webhooks`. Remind does not
require, discover, or contact any provider-specific service. In production, endpoints
must use HTTPS; credentials and URL fragments are rejected. The endpoint may
use any host, path, and query string. A signing secret is optional.

A Silicon may have zero, one, or many active subscriptions. `GET /webhooks` lists
them and `DELETE /webhooks/{subscription_id}` disables one. The legacy
`PUT /webhook` and `DELETE /webhook` routes remain as compatibility aliases.

## Request contract

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

`webhook-id` and `Idempotency-Key` contain the execution UUID. When a signing
secret is configured, `webhook-timestamp` contains the attempt's Unix seconds
and `webhook-signature` is `v1,` plus standard padded base64 HMAC-SHA256 over
`<webhook-id>.<webhook-timestamp>.<exact JSON bytes>`. Unsigned destinations
receive no signature headers.

Any HTTP 2xx response, with any response body including an empty body, marks the
attempt delivered. Remind stores the execution UUID as its local receipt marker;
there is no provider-specific receipt format.

## Retries and duplicates

Transport errors, 408, 425, 429, and all 5xx responses retry with bounded
exponential backoff and jitter. Other 3xx/4xx responses terminate the
occurrence. Redirects are disabled and response bodies are size bounded.

Delivery is at least once across uncertain network outcomes. Consumers should
deduplicate using `payload.execution_id`. The scheduler's unique occurrence
constraint prevents multiple logical executions for one schedule/version/trigger;
it cannot create exactly-once network delivery.
