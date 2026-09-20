# Remind 0.3.0

Creating a reminder requires an explicit IANA timezone. The CLI accepts `--timezone Asia/Kolkata` or `--timezone UTC`; the API requires the `timezone` JSON field, the Rust client requires `CreateScheduleRequest.timezone`, and the website requires the Timezone input. No interface silently defaults a new reminder to UTC.

Missing, null, empty, and whitespace-only timezone values receive actionable errors. The API returns HTTP 422 with code `timezone_required` before writing a reminder, execution, idempotency record, or audit event. Existing reminders keep their timezone, including when only their text is edited.

Development workers now use the same HTTP webhook policy as the API. Production webhook delivery still requires HTTPS.

Update the CLI with `honeycomb update 'tos>remind'`. Rust consumers should update `silicon-remind-client` to `0.3.0` and pass an explicit IANA timezone when creating reminders. See the [migration policy](version-policy.md) and [end-to-end validation record](timezone-e2e.md).
