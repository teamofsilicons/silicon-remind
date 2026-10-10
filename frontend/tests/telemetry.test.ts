import assert from "node:assert/strict";
import { test } from "node:test";
import { telemetryBatch } from "../lib/remind/telemetry";

test("browser telemetry strips identities, content, credentials, URL queries and raw errors", () => {
  const batch = telemetryBatch({ account_uuid: "account-secret", events: [
    { type: "network", data: { duration_ms: 23.8, status: 200, url: "https://example.com/?key=secret", headers: { Authorization: "Bearer secret" }, text: "private reminder" } },
    { type: "error", data: { message: "private stack trace", account: "account-secret" } },
  ] });
  assert.deepEqual(batch, { table: "remindtelemetry", events: [
    { source: "web", event: "request_completed", step: "browser", success: true, duration_ms: 24, status_code: 200 },
    { source: "web", event: "error", step: "browser", success: false, duration_ms: 0, status_code: null },
  ] });
  assert.equal(telemetryBatch({ events: Array(70).fill({}) }).events.length, 40);
});
